//! ComfyUI as a provider: runs an API-format workflow on a local or remote ComfyUI server.
//!
//! The protocol lives in `nodecraft_comfy` (sans-IO); this module sends its requests over
//! [`Http`]. A job carries:
//! - `params.workflow`: the API-format workflow (the Workflow node's mapped graph);
//! - `params.values`: `{"<node>.<input>": value}` overrides, checked against `/object_info`;
//! - media inputs whose role is `<node>.<input>` (an upload input such as `LoadImage.image`):
//!   uploaded under a content-hash name, then set to the stored name.
//!
//! **Submitting once.** The prompt id is derived from a per-install salt and the job's
//! idempotency key, so different installs (and sessions using fresh run ids) never share ids. A
//! submit first asks the server whether that prompt is already queued, running or done, so a
//! retried submit never queues the work twice; a prompt that *failed* is run again under a new
//! attempt id. Servers that ignore client-chosen ids fall back to the id they assign (then a lost
//! `/prompt` reply can't be detected).
//!
//! **Outputs** are the `output`-type files of the finished prompt, downloaded with the server's
//! auth, capped per file and in total, and sniffed; preview-only workflows fail with a clear
//! message. Live progress over the WebSocket is not wired yet: polling reports queue position and
//! completion.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use nodecraft_comfy::{self as comfy, ApiWorkflow, ComfyError, FileRef, HistoryEntry, InputKind, NodeCatalog, PromptRequest, QueueState, Server, UploadKind};

use crate::http::{Http, HttpMethod, HttpRequest, HttpResponse};
use crate::provider::{BoxFuture, JobHandle, JobInput, JobProgress, Output, Provider, ProviderJob, Submission};
use crate::{ErrorKind, ProviderError};

/// Cap on one downloaded output file.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;
/// Cap on all output files of one prompt together.
pub const DEFAULT_MAX_TOTAL_OUTPUT_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// Cap on output files of one prompt.
pub const MAX_OUTPUT_FILES: usize = 512;
/// How long a fetched `/object_info` is reused before fetching it again.
const CATALOG_TTL_MS: u64 = 5 * 60 * 1000;
/// Attempts (fresh prompt ids) tried for one job whose earlier prompts failed on the server.
const MAX_ATTEMPT_IDS: u32 = 16;
/// Downloads of large outputs from a remote server take time.
const DOWNLOAD_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// `/queue` lists every pending prompt's workflow; on a busy server it is large.
const MAX_QUEUE_BYTES: usize = 64 * 1024 * 1024;

/// A ComfyUI server as a provider.
pub struct ComfyProvider {
    server: Server,
    /// The auth header name, if any; its value travels as a secret header (never to another host).
    auth_name: Option<String>,
    http: Arc<dyn Http>,
    salt: String,
    client_id: String,
    max_output_bytes: usize,
    max_total_output_bytes: usize,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    catalog: Mutex<Option<(u64, Arc<NodeCatalog>)>>,
}

impl std::fmt::Debug for ComfyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComfyProvider").field("server", &self.server).finish_non_exhaustive()
    }
}

fn comfy_error(e: ComfyError) -> ProviderError {
    let kind = match &e {
        ComfyError::Server { status, .. } => match status {
            400 => ErrorKind::UnsupportedInput,
            s => ProviderError::from_status(*s, b"").kind,
        },
        ComfyError::TooLarge { .. } | ComfyError::Malformed { .. } => ErrorKind::Malformed,
        // Bad URLs, settings, workflows, inputs, missing nodes, and anything added later: the
        // request itself is the problem, so retrying won't help.
        _ => ErrorKind::UnsupportedInput,
    };
    ProviderError::new(kind, e.to_string())
}

fn uuid_from(seed: &str) -> String {
    let h = nodecraft_media::ContentHash::of(seed.as_bytes()).0;
    let hex: String = h.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let part = |a: usize, b: usize| hex.get(a..b).unwrap_or_default().to_owned();
    format!("{}-{}-{}-{}-{}", part(0, 8), part(8, 12), part(12, 16), part(16, 20), part(20, 32))
}

/// The prompt id (UUID form) for attempt `attempt` of the job with `idempotency_key`, on the
/// install with `salt`: the same job and attempt always get the same id.
pub fn prompt_id_for(salt: &str, idempotency_key: &str, attempt: u32) -> String {
    uuid_from(&format!("{salt}\u{0}{idempotency_key}\u{0}{attempt}"))
}

/// What the server knows about a prompt id.
enum Known {
    Unknown,
    Pending(usize),
    Running,
    Finished(HistoryEntry),
}

impl ComfyProvider {
    /// A provider for the server at `url`, with an optional auth header (from the keychain).
    /// `install_salt` is a random value stored once per install (in settings): it keeps prompt
    /// ids of different installs apart on a shared server. `now_ms` is a monotonic clock.
    pub fn new(
        url: &str,
        auth: Option<(String, String)>,
        install_salt: &str,
        http: Arc<dyn Http>,
        now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Result<Self, ProviderError> {
        if install_salt.trim().is_empty() {
            return Err(ProviderError::new(ErrorKind::UnsupportedInput, "the ComfyUI provider needs a per-install salt"));
        }
        let mut server = Server::new(url).map_err(comfy_error)?;
        if let Some((name, value)) = &auth {
            server = server.with_auth_header(name, value).map_err(comfy_error)?;
        }
        let client_id = format!("nodecraft-{}", uuid_from(&format!("{install_salt}\u{0}client")));
        Ok(Self {
            server,
            auth_name: auth.map(|(name, _)| name.to_ascii_lowercase()),
            http,
            salt: install_salt.to_owned(),
            client_id,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_total_output_bytes: DEFAULT_MAX_TOTAL_OUTPUT_BYTES,
            now_ms,
            catalog: Mutex::new(None),
        })
    }

    pub fn with_output_limits(mut self, per_file: usize, total: usize) -> Self {
        self.max_output_bytes = per_file;
        self.max_total_output_bytes = total;
        self
    }

    /// Sends a comfy-crate request. Its headers are sent as given, the auth header as a secret.
    async fn send(&self, request: comfy::Request, max_bytes: usize, timeout_ms: Option<u64>) -> Result<HttpResponse, ProviderError> {
        let method = match request.method {
            comfy::Method::Get => HttpMethod::Get,
            comfy::Method::Post => HttpMethod::Post,
        };
        let content_type = request.content_type();
        let body = match request.body {
            comfy::Body::Empty => Vec::new(),
            comfy::Body::Json(bytes) => bytes,
            comfy::Body::Multipart { bytes, .. } => bytes,
        };
        let mut http = HttpRequest { method, body, ..HttpRequest::get(request.url.as_str()) }.max_response_bytes(max_bytes);
        if let Some(ms) = timeout_ms {
            http = http.timeout_ms(ms);
        }
        if let Some(ct) = content_type {
            http = http.header("Content-Type", ct);
        }
        for (name, value) in request.headers {
            if self.auth_name.as_deref() == Some(name.to_ascii_lowercase().as_str()) {
                http = http.secret_header(name, value);
            } else {
                http = http.header(name, value);
            }
        }
        self.http.send(http).await
    }

    async fn send_ok(&self, request: comfy::Request, max_bytes: usize) -> Result<HttpResponse, ProviderError> {
        self.send(request, max_bytes, None).await?.error_for_status()
    }

    /// The server's node catalog, cached for a few minutes.
    pub async fn catalog(&self) -> Result<Arc<NodeCatalog>, ProviderError> {
        let now = (self.now_ms)();
        if let Some((at, c)) = self.catalog.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && now.saturating_sub(*at) < CATALOG_TTL_MS
        {
            return Ok(Arc::clone(c));
        }
        let response = self.send_ok(self.server.object_info().map_err(comfy_error)?, comfy::MAX_OBJECT_INFO_BYTES).await?;
        let (catalog, _skipped) = NodeCatalog::parse(&response.body).map_err(comfy_error)?;
        let catalog = Arc::new(catalog);
        *self.catalog.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, Arc::clone(&catalog)));
        Ok(catalog)
    }

    /// Forgets the cached catalog (after installing nodes or models; the servers panel's refresh).
    pub fn invalidate_catalog(&self) {
        *self.catalog.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Builds the workflow to queue. Value overrides are applied and the whole workflow is checked
    /// against the server *before* anything is uploaded; then media inputs are uploaded and set.
    /// Errors that a fresher catalog could fix (missing nodes, values not available) are marked
    /// `true`, so the caller refetches `/object_info` only for those.
    async fn prepare(&self, job: &ProviderJob, catalog: &NodeCatalog) -> Result<ApiWorkflow, (ProviderError, bool)> {
        let plain = |e: ProviderError| (e, false);
        let from_catalog = |e: ComfyError| {
            let stale = matches!(e, ComfyError::MissingNodes(_) | ComfyError::InvalidInput { .. } | ComfyError::InvalidWorkflow(_));
            (comfy_error(e), stale)
        };
        let raw = job.params.get("workflow").ok_or_else(|| plain(ProviderError::new(ErrorKind::UnsupportedInput, "the job has no ComfyUI workflow")))?;
        let bytes = serde_json::to_vec(raw).map_err(|e| plain(ProviderError::new(ErrorKind::UnsupportedInput, e.to_string())))?;
        let mut workflow = ApiWorkflow::parse(&bytes, Some(catalog)).map_err(from_catalog)?;
        let empty = Map::new();
        let values = job.params.get("values").and_then(Value::as_object).unwrap_or(&empty);
        for (target, value) in values {
            let (node, input) = split_target(target).map_err(plain)?;
            workflow.set_input(catalog, node, input, value.clone()).map_err(from_catalog)?;
        }
        // Every media input must target an upload input of a node in the workflow.
        let mut uploads = Vec::new();
        for input in &job.inputs {
            let JobInput::Media { role, bytes, .. } = input else { continue };
            let (node, name) = split_target(role).map_err(plain)?;
            let class = workflow
                .nodes
                .get(node)
                .map(|n| n.class_type.clone())
                .ok_or_else(|| plain(ProviderError::new(ErrorKind::UnsupportedInput, format!("no node {node} in the workflow"))))?;
            let is_upload = catalog.get(&class).and_then(|i| i.input(name)).is_some_and(|spec| matches!(spec.kind, InputKind::Combo { upload: Some(_), .. }));
            if !is_upload {
                return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, format!("{node}.{name} is not a file input"))));
            }
            uploads.push((node, name, bytes));
        }
        workflow.check(catalog).into_result().map_err(from_catalog)?;
        for (node, name, bytes) in uploads {
            let checked = crate::check_media(bytes, None).map_err(plain)?;
            let ext = checked.mime.rsplit('/').next().unwrap_or("bin").split(['+', ';']).next().unwrap_or("bin");
            // Named by content: a name collision means identical bytes, so overwriting is harmless
            // and repeated uploads of the same input dedupe.
            let filename = format!("nodecraft-{}.{ext}", checked.hash.to_hex());
            let upload = self.server.upload_image(&filename, bytes, UploadKind::Input, true).map_err(|e| plain(comfy_error(e)))?;
            let reply = self.send_ok(upload, 64 * 1024).await.map_err(plain)?;
            let stored = comfy::parse_upload_response(&reply.body).map_err(|e| plain(comfy_error(e)))?;
            let name_on_server = if stored.subfolder.is_empty() { stored.filename } else { format!("{}/{}", stored.subfolder, stored.filename) };
            workflow.set_input(catalog, node, name, Value::String(name_on_server)).map_err(|e| plain(comfy_error(e)))?;
        }
        Ok(workflow)
    }

    async fn history(&self, prompt_id: &str) -> Result<Option<HistoryEntry>, ProviderError> {
        let response = self.send_ok(self.server.history(prompt_id).map_err(comfy_error)?, 64 * 1024 * 1024).await?;
        HistoryEntry::parse(&response.body, prompt_id).map_err(comfy_error)
    }

    async fn queue_state(&self) -> Result<QueueState, ProviderError> {
        let response = self.send_ok(self.server.queue().map_err(comfy_error)?, MAX_QUEUE_BYTES).await.map_err(|e| {
            // A huge queue on a busy shared server is a passing condition, not a broken reply.
            if e.kind == ErrorKind::Malformed {
                ProviderError::new(ErrorKind::Busy, format!("the server's queue is too large to read: {}", e.message))
            } else {
                e
            }
        })?;
        QueueState::parse(&response.body).map_err(comfy_error)
    }

    /// Where a prompt is. The queue is read before the history: a prompt leaves the queue only once
    /// it is in history, so a run finishing between the two reads is found there, never lost.
    async fn locate(&self, prompt_id: &str) -> Result<Known, ProviderError> {
        let queue = self.queue_state().await?;
        if queue.running.iter().any(|p| p == prompt_id) {
            return Ok(Known::Running);
        }
        if let Some(i) = queue.pending.iter().position(|p| p == prompt_id) {
            return Ok(Known::Pending(i));
        }
        match self.history(prompt_id).await? {
            Some(entry) => Ok(Known::Finished(entry)),
            None => Ok(Known::Unknown),
        }
    }

    /// Downloads a finished prompt's output files with the server's auth, within the caps.
    async fn download(&self, files: &[FileRef]) -> Result<Vec<Output>, ProviderError> {
        if files.len() > MAX_OUTPUT_FILES {
            return Err(ProviderError::new(
                ErrorKind::UnsupportedInput,
                format!("the prompt saved {} files; at most {MAX_OUTPUT_FILES} are fetched", files.len()),
            ));
        }
        let mut outputs = Vec::new();
        let mut skipped = Vec::new();
        let mut total = 0usize;
        for file in files {
            let remaining = self.max_total_output_bytes.saturating_sub(total);
            let cap = self.max_output_bytes.min(remaining);
            let response = self.send(self.server.view(file).map_err(comfy_error)?, cap, Some(DOWNLOAD_TIMEOUT_MS)).await?.error_for_status()?;
            if let Some(announced) = response.header("content-length").and_then(|v| v.trim().parse::<usize>().ok())
                && response.header("content-encoding").is_none()
                && announced != response.body.len()
            {
                return Err(ProviderError::new(ErrorKind::Network, format!("the download of {} was cut short", file.filename)));
            }
            total = total.saturating_add(response.body.len());
            // Files NodeCraft can't show (latents, model files) are skipped, not fatal; a text or
            // HTML body for a file output is a proxy or error page, not a result.
            match nodecraft_media::sniff(&response.body) {
                Some(s) if s.kind != nodecraft_media::MediaKind::Text => {
                    outputs.push(Output::Bytes { mime: s.mime.to_owned(), bytes: Arc::from(response.body) });
                }
                Some(_) if !looks_textual(&file.filename) => {
                    return Err(ProviderError::new(
                        ErrorKind::Malformed,
                        format!("{} came back as a text page, not a file (check the server's auth)", file.filename),
                    ));
                }
                _ => skipped.push(file.filename.clone()),
            }
        }
        if outputs.is_empty() {
            let names: Vec<String> = skipped.iter().take(5).cloned().collect();
            return Err(ProviderError::new(ErrorKind::JobFailed, format!("the workflow saved no files NodeCraft can use (skipped: {})", names.join(", "))));
        }
        Ok(outputs)
    }

    /// Picks the prompt id for a submit: an id the server already has queued, running or finished
    /// successfully is reused (no second run); one whose prompt failed moves on to a new attempt.
    /// The queue is read once (it can be large); history is checked per attempt id.
    async fn choose_prompt_id(&self, key: &str) -> Result<(String, bool), ProviderError> {
        let queue = self.queue_state().await?;
        for attempt in 0..MAX_ATTEMPT_IDS {
            let id = prompt_id_for(&self.salt, key, attempt);
            if queue.contains(&id) {
                return Ok((id, true));
            }
            match self.history(&id).await? {
                None => return Ok((id, false)),
                Some(entry) if entry.completed && entry.error.is_none() && !entry.interrupted => return Ok((id, true)),
                Some(_) => {}
            }
        }
        Err(ProviderError::new(ErrorKind::UnsupportedInput, "this job failed on the server too many times"))
    }
}

/// File names whose text content is a legitimate output (`.txt`, `.json`, `.srt`, …).
fn looks_textual(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "txt" | "json" | "srt" | "vtt" | "csv" | "md" | "obj" | "gltf" | "svg")
}

fn split_target(target: &str) -> Result<(&str, &str), ProviderError> {
    target
        .split_once('.')
        .filter(|(n, i)| !n.is_empty() && !i.is_empty())
        .ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, format!("{target:?} is not a <node>.<input> target")))
}

fn handle_for(prompt_id: &str) -> JobHandle {
    JobHandle { remote_id: prompt_id.to_owned(), data: Value::Null }
}

/// A finished history entry as a job result.
fn finished(entry: &HistoryEntry) -> Result<Vec<FileRef>, ProviderError> {
    if entry.interrupted {
        return Err(ProviderError::new(ErrorKind::Cancelled, "the prompt was interrupted on the server"));
    }
    if let Some(error) = &entry.error {
        let at = entry.error_node.as_ref().map(|(id, class)| format!(" (node {id} {class})")).unwrap_or_default();
        // Out of memory is worth retrying once the GPU frees up; other workflow errors would just
        // fail (and maybe cost) again.
        let lower = error.to_ascii_lowercase();
        let kind = if lower.contains("out of memory") || lower.contains("oom") { ErrorKind::Server } else { ErrorKind::JobFailed };
        return Err(ProviderError::new(kind, format!("{error}{at}")));
    }
    if !entry.completed {
        let status = if entry.status.is_empty() { "unknown".to_owned() } else { entry.status.clone() };
        return Err(ProviderError::new(ErrorKind::JobFailed, format!("the prompt ended without completing (status: {status})")));
    }
    let files: Vec<FileRef> = entry.outputs.iter().flat_map(|(_, files)| files.iter().filter(|f| f.kind == "output").cloned()).collect();
    if files.is_empty() {
        return Err(ProviderError::new(ErrorKind::UnsupportedInput, "the workflow saved no outputs; add a Save node (previews aren't kept)"));
    }
    Ok(files)
}

impl Provider for ComfyProvider {
    fn id(&self) -> &str {
        "comfyui"
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            let (prompt_id, already) = self.choose_prompt_id(&job.idempotency_key).await?;
            if already {
                return Ok(Submission::Queued(handle_for(&prompt_id)));
            }
            let mut catalog = self.catalog().await?;
            let workflow = match self.prepare(job, &catalog).await {
                Ok(w) => w,
                // Maybe nodes or models were installed since the catalog was fetched: refetch once.
                Err((_, true)) => {
                    self.invalidate_catalog();
                    catalog = self.catalog().await?;
                    self.prepare(job, &catalog).await.map_err(|(e, _)| e)?
                }
                Err((e, false)) => return Err(e),
            };
            let mut request = PromptRequest::new(workflow, self.client_id.clone());
            request.prompt_id = Some(prompt_id.clone());
            let response = self.send(self.server.prompt(&request).map_err(comfy_error)?, 4 * 1024 * 1024, None).await?;
            let queued = comfy::parse_prompt_response(response.status, &response.body).map_err(comfy_error)?;
            // Older servers ignore a client-chosen id: follow the one they assigned.
            Ok(Submission::Queued(handle_for(&queued.prompt_id)))
        })
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            match self.locate(&handle.remote_id).await? {
                Known::Pending(i) => Ok(JobProgress::Queued { position: u32::try_from(i.saturating_add(1)).ok() }),
                Known::Running => Ok(JobProgress::Running { fraction: None, message: None, partial: None }),
                Known::Finished(entry) => match finished(&entry) {
                    Ok(files) => Ok(JobProgress::Done(self.download(&files).await?)),
                    Err(e) => Ok(JobProgress::Failed(e)),
                },
                // Not queued, running or in history: the server restarted (history is in memory).
                Known::Unknown => Err(ProviderError::new(ErrorKind::NotFound, "the server no longer knows this prompt (was it restarted?)")),
            }
        })
    }

    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let id = handle.remote_id.clone();
            if let Known::Pending(_) = self.locate(&id).await? {
                self.send_ok(self.server.delete_from_queue(std::slice::from_ref(&id)).map_err(comfy_error)?, 64 * 1024).await?;
            }
            // Re-check: it may have started between reading the queue and deleting it. Interrupt
            // by prompt id, so another user's prompt on a shared server is never stopped (servers
            // too old for prompt-scoped interrupts stop whatever runs; see docs/providers.md).
            if let Known::Running = self.locate(&id).await? {
                self.send_ok(self.server.interrupt(Some(&id)).map_err(comfy_error)?, 64 * 1024).await?;
            }
            // Finished or unknown: nothing left to stop.
            Ok(())
        })
    }

    fn poll_interval_ms(&self, progress: &JobProgress) -> u64 {
        match progress {
            JobProgress::Queued { .. } => 2_000,
            _ => 1_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OBJECT_INFO: &str = r#"{
      "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
      "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["CONDITIONING"]},
      "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true}
    }"#;
    const EMPTY_QUEUE: &str = r#"{"queue_running":[],"queue_pending":[]}"#;
    const SALT: &str = "install-salt";

    /// A scripted ComfyUI server: answers by path (queue and history answers can be scripted as a
    /// sequence), records requests.
    struct FakeServer {
        seen: Mutex<Vec<HttpRequest>>,
        history: Mutex<VecDeque<String>>,
        queue: Mutex<VecDeque<String>>,
        view: Mutex<Vec<u8>>,
        object_info_calls: AtomicUsize,
    }

    impl FakeServer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(vec![]),
                history: Mutex::new(VecDeque::new()),
                queue: Mutex::new(VecDeque::new()),
                view: Mutex::new(b"\x89PNG\r\n\x1a\nimage".to_vec()),
                object_info_calls: AtomicUsize::new(0),
            })
        }
        /// Next answer from a script; the last one repeats.
        fn next(script: &Mutex<VecDeque<String>>, default: &str) -> String {
            let mut s = script.lock().unwrap();
            if s.len() > 1 { s.pop_front().unwrap() } else { s.front().cloned().unwrap_or_else(|| default.to_owned()) }
        }
        fn set(script: &Mutex<VecDeque<String>>, answers: &[&str]) {
            *script.lock().unwrap() = answers.iter().map(|a| (*a).to_owned()).collect();
        }
        fn paths(&self) -> Vec<String> {
            self.seen.lock().unwrap().iter().map(|r| r.url.split("8188").nth(1).unwrap_or_default().to_owned()).collect()
        }
    }

    impl Http for FakeServer {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            let path = request.url.split("8188").nth(1).unwrap_or_default().to_owned();
            let body: Vec<u8> = match path.split('?').next().unwrap_or_default() {
                "/object_info" => {
                    self.object_info_calls.fetch_add(1, Ordering::SeqCst);
                    OBJECT_INFO.into()
                }
                "/queue" if request.method == HttpMethod::Get => Self::next(&self.queue, EMPTY_QUEUE).into_bytes(),
                "/upload/image" => r#"{"name":"stored.png","subfolder":"in","type":"input"}"#.into(),
                "/prompt" => {
                    let v: Value = serde_json::from_slice(&request.body).unwrap();
                    format!(r#"{{"prompt_id":"{}","number":1,"node_errors":{{}}}}"#, v["prompt_id"].as_str().unwrap()).into_bytes()
                }
                "/view" => self.view.lock().unwrap().clone(),
                p if p.starts_with("/history/") => Self::next(&self.history, "{}").into_bytes(),
                _ => b"{}".to_vec(),
            };
            self.seen.lock().unwrap().push(request);
            Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
        }
    }

    fn provider(http: Arc<FakeServer>) -> ComfyProvider {
        ComfyProvider::new("http://gpu-box:8188", Some(("X-Comfy-Key".into(), "secret".into())), SALT, http, Arc::new(|| 0)).unwrap()
    }

    fn job() -> ProviderJob {
        let workflow = serde_json::json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "old"}},
            "3": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "NodeCraft"}}
        });
        ProviderJob {
            model: "comfyui:workflow".into(),
            params: serde_json::json!({"workflow": workflow, "values": {"2.text": "a red fox"}}).as_object().cloned().unwrap(),
            inputs: vec![JobInput::Media { role: "1.image".into(), mime: "image/png".into(), bytes: Arc::from(&b"\x89PNG\r\n\x1a\n"[..]) }],
            idempotency_key: "nodecraft:run-uuid:1".into(),
        }
    }

    fn history_ok(id: &str, files: &str) -> String {
        format!(r#"{{"{id}":{{"outputs":{{"3":{{"images":{files}}}}},"status":{{"status_str":"success","completed":true,"messages":[]}}}}}}"#)
    }

    #[test]
    fn submit_uploads_by_content_applies_values_and_queues_with_a_salted_id() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let submitted = block_on(p.submit(&job())).unwrap();
        let id = prompt_id_for(SALT, "nodecraft:run-uuid:1", 0);
        assert_eq!(submitted, Submission::Queued(handle_for(&id)));
        assert_ne!(id, prompt_id_for("other-install", "nodecraft:run-uuid:1", 0), "installs never share ids");
        let seen = server.seen.lock().unwrap();
        let prompt = seen.iter().find(|r| r.url.ends_with("/prompt")).unwrap();
        let body: Value = serde_json::from_slice(&prompt.body).unwrap();
        assert_eq!(body["prompt"]["2"]["inputs"]["text"], "a red fox");
        assert_eq!(body["prompt"]["1"]["inputs"]["image"], "in/stored.png", "the stored name with its subfolder");
        assert_eq!(body["prompt_id"], id.as_str());
        let upload = seen.iter().find(|r| r.url.ends_with("/upload/image")).unwrap();
        let hash = nodecraft_media::ContentHash::of(b"\x89PNG\r\n\x1a\n").to_hex();
        assert!(String::from_utf8_lossy(&upload.body).contains(&format!("nodecraft-{hash}.png")), "named by content");
        for r in seen.iter() {
            assert!(r.secret_headers.contains(&"x-comfy-key".to_string()), "the key is a secret header on {}", r.url);
        }
    }

    #[test]
    fn a_retried_submit_does_not_queue_twice_but_a_failed_prompt_runs_again() {
        let server = FakeServer::new();
        let first = prompt_id_for(SALT, "nodecraft:run-uuid:1", 0);
        FakeServer::set(&server.queue, &[&format!(r#"{{"queue_running":[],"queue_pending":[[1,"{first}"]]}}"#)]);
        let p = provider(server.clone());
        assert_eq!(block_on(p.submit(&job())).unwrap(), Submission::Queued(handle_for(&first)));
        assert!(!server.paths().iter().any(|p| p == "/prompt"), "already queued: nothing posted");

        let server = FakeServer::new();
        let failed = format!(
            r#"{{"{first}":{{"outputs":{{}},"status":{{"status_str":"error","completed":false,"messages":[["execution_error",{{"exception_message":"OOM"}}]]}}}}}}"#
        );
        FakeServer::set(&server.history, &[&failed, "{}"]);
        let p = provider(server.clone());
        let second = prompt_id_for(SALT, "nodecraft:run-uuid:1", 1);
        assert_eq!(block_on(p.submit(&job())).unwrap(), Submission::Queued(handle_for(&second)), "a new attempt id");
        assert!(server.paths().iter().any(|p| p == "/prompt"));
    }

    #[test]
    fn checks_run_before_uploads_and_targets_are_validated() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let mut j = job();
        j.params.insert(
            "workflow".into(),
            serde_json::json!({"1": {"class_type": "LoadImage", "inputs": {"image": "x"}}, "9": {"class_type": "FancyUpscale", "inputs": {}}}),
        );
        j.params.remove("values");
        let e = block_on(p.submit(&j)).unwrap_err();
        assert!(e.message.contains("FancyUpscale"), "{e}");
        assert!(!server.paths().iter().any(|p| p == "/upload/image"), "nothing uploaded for a workflow that can't run");
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 2, "the catalog is refetched once in case nodes were installed");
        let mut j = job();
        j.inputs = vec![JobInput::Media { role: "2.text".into(), mime: "image/png".into(), bytes: Arc::from(&b"\x89PNG\r\n\x1a\n"[..]) }];
        assert!(block_on(p.submit(&j)).unwrap_err().message.contains("not a file input"));
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 2, "errors a catalog can't fix don't refetch it");
        let mut j = job();
        j.params.insert("values".into(), serde_json::json!({"nodot": 1}));
        assert!(block_on(p.submit(&j)).unwrap_err().message.contains("<node>.<input>"));
    }

    #[test]
    fn a_run_finishing_between_reads_is_not_lost() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        // Running at the queue read; history then shows it done.
        FakeServer::set(&server.queue, &[EMPTY_QUEUE]);
        FakeServer::set(&server.history, &[&history_ok("abc", r#"[{"filename":"o.png","subfolder":"","type":"output"}]"#)]);
        assert!(matches!(block_on(p.poll(&handle_for("abc"))).unwrap(), JobProgress::Done(o) if o.len() == 1));
    }

    #[test]
    fn poll_reports_queue_progress_and_downloads_outputs() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        FakeServer::set(&server.queue, &[r#"{"queue_running":[[0,"other"]],"queue_pending":[[1,"x"],[2,"abc"]]}"#]);
        assert_eq!(block_on(p.poll(&handle_for("abc"))).unwrap(), JobProgress::Queued { position: Some(2) });
        FakeServer::set(&server.queue, &[r#"{"queue_running":[[0,"abc"]],"queue_pending":[]}"#]);
        assert!(matches!(block_on(p.poll(&handle_for("abc"))).unwrap(), JobProgress::Running { .. }));
        FakeServer::set(&server.queue, &[EMPTY_QUEUE]);
        FakeServer::set(
            &server.history,
            &[&history_ok("abc", r#"[{"filename":"a.png","subfolder":"","type":"output"},{"filename":"p.png","subfolder":"","type":"temp"}]"#)],
        );
        let JobProgress::Done(outputs) = block_on(p.poll(&handle_for("abc"))).unwrap() else { panic!("not done") };
        assert_eq!(outputs.len(), 1, "previews (temp) are not outputs");
        assert!(matches!(&outputs[0], Output::Bytes { mime, .. } if mime == "image/png"));
    }

    #[test]
    fn empty_failed_and_vanished_prompts() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let fail = |h: &str| {
            FakeServer::set(&server.history, &[h]);
            match block_on(p.poll(&handle_for("abc"))).unwrap() {
                JobProgress::Failed(e) => e,
                other => panic!("{other:?}"),
            }
        };
        let e = fail(&history_ok("abc", r#"[{"filename":"p.png","subfolder":"","type":"temp"}]"#));
        assert!(e.message.contains("saved no outputs"), "preview-only workflows fail clearly");
        let e = fail(
            r#"{"abc":{"outputs":{},"status":{"status_str":"error","completed":false,"messages":[["execution_error",{"node_id":"3","node_type":"KSampler","exception_message":"CUDA out of memory"}]]}}}"#,
        );
        assert!(e.message.contains("CUDA") && e.message.contains("KSampler"), "{e}");
        assert!(e.is_transient(), "out of memory may pass once the GPU frees up");
        let e = fail(
            r#"{"abc":{"outputs":{},"status":{"status_str":"error","completed":false,"messages":[["execution_error",{"node_id":"4","exception_message":"Value not in list: ckpt_name"}]]}}}"#,
        );
        assert_eq!(e.kind, ErrorKind::JobFailed);
        assert!(!e.is_transient(), "a workflow error isn't rerun (and paid for) automatically");
        let e = fail(r#"{"abc":{"outputs":{},"status":{"status_str":"error","completed":false,"messages":[]}}}"#);
        assert!(e.message.contains("status: error"), "{e}");
        let e = fail(r#"{"abc":{"outputs":{},"status":{"status_str":"error","completed":false,"messages":[["execution_interrupted",{}]]}}}"#);
        assert_eq!(e.kind, ErrorKind::Cancelled);
        FakeServer::set(&server.history, &["{}"]);
        assert_eq!(block_on(p.poll(&handle_for("gone"))).unwrap_err().kind, ErrorKind::NotFound);
    }

    #[test]
    fn downloads_skip_unknown_files_refuse_text_pages_and_respect_caps() {
        let server = FakeServer::new();
        let p = provider(server.clone()).with_output_limits(1024, 20);
        *server.view.lock().unwrap() = b"\x00\x01latent-bytes".to_vec();
        let files = [FileRef { filename: "x.latent".into(), subfolder: String::new(), kind: "output".into() }];
        let e = block_on(p.download(&files)).unwrap_err();
        assert!(e.message.contains("x.latent") && e.kind == ErrorKind::JobFailed, "all skipped is a clear failure: {e}");
        *server.view.lock().unwrap() = b"<html>Please sign in</html>".to_vec();
        let png = [FileRef { filename: "x.png".into(), subfolder: String::new(), kind: "output".into() }];
        assert_eq!(block_on(p.download(&png)).unwrap_err().kind, ErrorKind::Malformed);
        let many: Vec<FileRef> =
            (0..=MAX_OUTPUT_FILES).map(|i| FileRef { filename: format!("{i}.png"), subfolder: String::new(), kind: "output".into() }).collect();
        assert!(block_on(p.download(&many)).is_err());
        let seen = server.seen.lock().unwrap();
        let view = seen.iter().find(|r| r.url.contains("/view")).unwrap();
        assert_eq!((view.max_response_bytes, view.timeout_ms), (20, DOWNLOAD_TIMEOUT_MS), "capped by the remaining total");
    }

    #[test]
    fn cancel_deletes_pending_and_interrupts_only_this_prompt() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        // Pending at the first read, still pending after the delete.
        FakeServer::set(&server.queue, &[r#"{"queue_running":[],"queue_pending":[[1,"wait"]]}"#, EMPTY_QUEUE]);
        block_on(p.cancel(&handle_for("wait"))).unwrap();
        // Pending at the first read, started before the delete landed: interrupted.
        FakeServer::set(&server.queue, &[r#"{"queue_running":[],"queue_pending":[[1,"race"]]}"#, r#"{"queue_running":[[0,"race"]],"queue_pending":[]}"#]);
        block_on(p.cancel(&handle_for("race"))).unwrap();
        let seen = server.seen.lock().unwrap();
        let deletes: Vec<Value> =
            seen.iter().filter(|r| r.url.ends_with("/queue") && r.method == HttpMethod::Post).map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
        assert_eq!(deletes, vec![serde_json::json!({"delete": ["wait"]}), serde_json::json!({"delete": ["race"]})]);
        let interrupts: Vec<Value> = seen.iter().filter(|r| r.url.ends_with("/interrupt")).map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
        assert_eq!(interrupts, vec![serde_json::json!({"prompt_id": "race"})]);
    }

    #[test]
    fn the_catalog_is_cached_and_can_be_invalidated() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        block_on(p.catalog()).unwrap();
        block_on(p.catalog()).unwrap();
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 1);
        p.invalidate_catalog();
        block_on(p.catalog()).unwrap();
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 2);
        assert!(!format!("{p:?}").contains("secret"));
        assert!(ComfyProvider::new("http://h:8188", None, " ", FakeServer::new(), Arc::new(|| 0)).is_err(), "a salt is required");
    }
}
