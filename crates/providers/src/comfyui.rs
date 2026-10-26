//! ComfyUI as a provider: runs an API-format workflow on a local or remote ComfyUI server.
//!
//! The protocol lives in `nodecraft_comfy` (sans-IO); this module sends its requests over
//! [`Http`]. A job carries:
//! - `params.workflow`: the API-format workflow (the Workflow node's mapped graph);
//! - `params.values`: `{"<node>.<input>": value}` overrides, checked against `/object_info`;
//! - media inputs whose role is `<node>.<input>` (an upload input such as `LoadImage.image`):
//!   uploaded under a content-hash name with the extension the loader lists, then set to the
//!   stored name. A file the loader can't read (a sound for an image loader) is refused first.
//!
//! A catalog model's job carries no workflow: [`template`] builds it from the model's
//! `provider_model` and the card's prompt, image and parameters.
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
//! message.
//!
//! **Progress.** Polling reports queue position and completion. With [`progress::Feeds`] set
//! ([`ComfyProvider::with_progress`]), the server's WebSocket is opened before a prompt is queued
//! and a running prompt also reports how far it is and which node runs ([`progress`]).

use std::hash::BuildHasher;
use std::sync::{Arc, Mutex, PoisonError};

pub mod progress;
pub(crate) mod template;
pub(crate) mod upload;

use serde_json::{Map, Value};

use nodecraft_comfy::{
    self as comfy, ApiWorkflow, ComfyError, FileRef, HistoryEntry, InputKind, NodeCatalog, PromptRequest, QueueState, Server, SystemStats, UploadKind,
};

use crate::http::{Http, HttpMethod, HttpRequest, HttpResponse};
use crate::provider::{BoxFuture, Completed, JobHandle, JobInput, JobProgress, Output, Provider, ProviderJob, Submission};
use crate::{ErrorKind, ProviderError};

/// Cap on one downloaded output file. A run keeps at most 256 MiB of results in all (the
/// engine's `MAX_INLINE_OUTPUT_BYTES`), so a longer video is refused before it is fetched,
/// not after (#110).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
/// Cap on all output files of one prompt together.
pub const DEFAULT_MAX_TOTAL_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
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
/// `/system_stats` is a few lines per device.
const MAX_STATS_BYTES: usize = 4 * 1024 * 1024;

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
    live: Option<progress::Live>,
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

pub(crate) fn uuid_from(seed: &str) -> String {
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
        // Random per provider: the server keeps one socket per client id, so two processes (the
        // app and the CLI) sharing one would take each other's progress.
        let session = std::collections::hash_map::RandomState::new().hash_one(install_salt);
        let client_id = format!("nodecraft-{}-{session:016x}", uuid_from(&format!("{install_salt}\u{0}client")));
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
            live: None,
        })
    }

    pub fn with_output_limits(mut self, per_file: usize, total: usize) -> Self {
        self.max_output_bytes = per_file;
        self.max_total_output_bytes = total;
        self
    }

    /// Reports a running prompt's progress from the server's WebSocket, opened with `feeds`.
    pub fn with_progress(mut self, feeds: Arc<dyn progress::Feeds>) -> Self {
        self.live = Some(progress::Live::new(feeds));
        self
    }

    /// Opens the progress socket if there is none. Best effort: a failure only means no fraction.
    async fn connect_progress(&self) {
        let Some(live) = &self.live else { return };
        let Ok(url) = self.server.websocket_url(&self.client_id) else { return };
        live.connect(url.to_string(), self.server.websocket_headers(), (self.now_ms)()).await;
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
        // A catalog card names a built-in workflow instead of carrying one.
        let templated;
        let job = if template::is_templated(job) {
            templated = template::expand(job, catalog)?;
            &templated
        } else {
            job
        };
        let empty = Map::new();
        let carried: Map<String, Value>;
        let (mut workflow, values) = match (job.params.get("workflow"), template::card_text(job, template::CARD_WORKFLOW)) {
            (Some(raw), None) => {
                if template::card_text(job, template::CARD_VALUES).is_some() || template::card_text(job, template::CARD_OUTPUTS).is_some() {
                    return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, "the job mixes a workflow card's inputs into another workflow")));
                }
                let bytes = serde_json::to_vec(raw).map_err(|e| plain(ProviderError::new(ErrorKind::UnsupportedInput, e.to_string())))?;
                let workflow = ApiWorkflow::parse(&bytes, Some(catalog)).map_err(from_catalog)?;
                (workflow, job.params.get("values").and_then(Value::as_object).unwrap_or(&empty))
            }
            // A workflow card (#9) carries its workflow and values as text inputs.
            (None, Some(text)) => {
                if job.params.contains_key("values") {
                    return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, "a workflow card's job carries its values as an input")));
                }
                carried = card_values(job).map_err(plain)?;
                if text.len() > MAX_CARD_TEXT_BYTES {
                    return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, "the card's ComfyUI workflow is too large")));
                }
                let mut workflow = ApiWorkflow::parse(text.as_bytes(), Some(catalog)).map_err(from_catalog)?;
                keep_card_outputs(&mut workflow, job, catalog).map_err(plain)?;
                (workflow, &carried)
            }
            (Some(_), Some(_)) => return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, "the job carries two ComfyUI workflows"))),
            (None, None) => return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, "the job has no ComfyUI workflow"))),
        };
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
            let loader = catalog.get(&class).and_then(|i| i.input(name)).and_then(|spec| match spec.kind {
                InputKind::Combo { upload, .. } => upload,
                _ => None,
            });
            let Some(loader) = loader else {
                return Err(plain(ProviderError::new(ErrorKind::UnsupportedInput, format!("{node}.{name} is not a file input"))));
            };
            uploads.push((node, name, loader, bytes));
        }
        workflow.check(catalog).into_result().map_err(from_catalog)?;
        // Every input is checked before any is uploaded: a refused run leaves nothing on the server.
        let mut checked_uploads = Vec::with_capacity(uploads.len());
        for (node, name, loader, bytes) in uploads {
            let checked = crate::check_media(bytes, None).map_err(plain)?;
            upload::check_kind(&format!("{node}.{name}"), loader, bytes, checked.mime)
                .map_err(|e| plain(ProviderError::new(ErrorKind::UnsupportedInput, e)))?;
            // Recognition isn't upload permission (#394): an HDR input is refused here exactly as it
            // was before those formats were recognised at all, instead of landing on the user's
            // server under a name no Comfy loader can read.
            if nodecraft_media::is_undecoded_image(checked.mime) {
                return Err(plain(ProviderError::new(
                    ErrorKind::UnsupportedInput,
                    format!("can't upload {node}.{name}: NodeCraft can't read {} files yet; convert the input to PNG or JPEG", checked.mime),
                )));
            }
            checked_uploads.push((node, name, bytes, checked));
        }
        for (node, name, bytes, checked) in checked_uploads {
            let ext = upload::extension(checked.mime);
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

    /// The server's health check (#9): its versions, devices and free VRAM.
    pub async fn system_stats(&self) -> Result<SystemStats, ProviderError> {
        let response = self.send_ok(self.server.system_stats().map_err(comfy_error)?, MAX_STATS_BYTES).await?;
        SystemStats::parse(&response.body).map_err(comfy_error)
    }

    /// The prompts the server is running and has waiting.
    pub async fn queue_state(&self) -> Result<QueueState, ProviderError> {
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
                // An HDR image is recognised (#394) but has no decoder, so it is skipped like a
                // latent rather than kept as a result the engine then refuses: one EXR saved next to
                // the PNG must not cost the user the whole paid run.
                Some(s) if nodecraft_media::is_undecoded_image(s.mime) => skipped.push(file.filename.clone()),
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

/// Most bytes of a workflow card's workflow or values text: one text input's limit.
pub(crate) const MAX_CARD_TEXT_BYTES: usize = 1024 * 1024;
/// Most values one workflow card sets.
const MAX_CARD_VALUES: usize = 1024;

/// A workflow card's values (#9): a JSON object of `"<node>.<input>"` to a string, number or
/// boolean, bounded before it is parsed. No values input means none.
pub(crate) fn card_values(job: &ProviderJob) -> Result<Map<String, Value>, ProviderError> {
    let refused = |message: &str| ProviderError::new(ErrorKind::UnsupportedInput, format!("the card's ComfyUI values {message}"));
    let Some(text) = template::card_text(job, template::CARD_VALUES) else { return Ok(Map::new()) };
    if text.len() > MAX_CARD_TEXT_BYTES {
        return Err(refused("are too large"));
    }
    // Only scalars are accepted, so serde_json's own nesting limit bounds the parse.
    let Ok(Value::Object(values)) = serde_json::from_str::<Value>(text) else { return Err(refused("are not a JSON object")) };
    if values.len() > MAX_CARD_VALUES {
        return Err(refused("set too many inputs"));
    }
    if values.values().any(|v| !(v.is_string() || v.is_number() || v.is_boolean())) {
        return Err(refused("must be strings, numbers or booleans"));
    }
    Ok(values)
}

/// Keeps only a workflow card's output nodes (#9), named by its outputs input as a JSON list of
/// node ids: the workflow's other output nodes are dropped before it is queued, so the server
/// never runs them and no file comes back that the card has no port for. A node another node
/// reads from is kept. No outputs input keeps every node.
pub(crate) fn keep_card_outputs(workflow: &mut ApiWorkflow, job: &ProviderJob, catalog: &NodeCatalog) -> Result<(), ProviderError> {
    let refused = |message: String| ProviderError::new(ErrorKind::UnsupportedInput, format!("the card's ComfyUI outputs {message}"));
    let Some(text) = template::card_text(job, template::CARD_OUTPUTS) else { return Ok(()) };
    if text.len() > MAX_CARD_TEXT_BYTES {
        return Err(refused("are too large".into()));
    }
    let Ok(kept) = serde_json::from_str::<std::collections::BTreeSet<String>>(text) else {
        return Err(refused("are not a list of node ids".into()));
    };
    if kept.is_empty() {
        return Err(refused("name no node".into()));
    }
    if let Some(missing) = kept.iter().find(|id| !workflow.nodes.contains_key(id.as_str())) {
        let shown: String = missing.chars().take(64).collect();
        return Err(refused(format!("name node {shown}, which the workflow doesn't have")));
    }
    let read: std::collections::BTreeSet<&str> = workflow
        .nodes
        .values()
        .flat_map(|node| node.inputs.values())
        .filter_map(|value| match value {
            comfy::InputValue::Link { node, .. } => Some(node.as_str()),
            comfy::InputValue::Value(_) => None,
        })
        .collect();
    let dropped: Vec<String> = workflow
        .nodes
        .iter()
        .filter(|(id, node)| !kept.contains(*id) && !read.contains(id.as_str()) && catalog.get(&node.class_type).is_some_and(|info| info.output_node))
        .map(|(id, _)| id.clone())
        .collect();
    for id in dropped {
        workflow.nodes.remove(&id);
    }
    Ok(())
}

pub(crate) fn split_target(target: &str) -> Result<(&str, &str), ProviderError> {
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
        let oom = lower.contains("out of memory") || lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "oom");
        let kind = if oom { ErrorKind::Server } else { ErrorKind::JobFailed };
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

// Deliberately uses Provider's conservative evidence default: submission can query history,
// fetch catalogs, upload inputs and dispatch a prompt. A local later error proves no absence.
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
            // Opened before queueing: the server sends a prompt's progress only to a socket that
            // is already open.
            self.connect_progress().await;
            let mut request = PromptRequest::new(workflow, self.client_id.clone());
            request.prompt_id = Some(prompt_id.clone());
            let response = self.send(self.server.prompt(&request).map_err(comfy_error)?, 4 * 1024 * 1024, None).await?;
            let queued = comfy::parse_prompt_response(response.status, &response.body).map_err(comfy_error)?;
            if let Some(live) = &self.live {
                live.track(&queued.prompt_id, &request.workflow);
            }
            // Older servers ignore a client-chosen id: follow the one they assigned.
            Ok(Submission::Queued(handle_for(&queued.prompt_id)))
        })
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            let id = &handle.remote_id;
            let known = self.locate(id).await?;
            let live = self.live.as_ref();
            if matches!(known, Known::Pending(_) | Known::Running) && live.is_some_and(progress::Live::wanted) {
                // The socket was lost: open another (with the same client id the server routes by).
                self.connect_progress().await;
            }
            // Read the socket even while queued, so its messages don't pile up.
            let now = live.and_then(|live| live.progress(id, (self.now_ms)()));
            if !matches!(known, Known::Pending(_) | Known::Running)
                && let Some(live) = live
            {
                live.forget(id);
            }
            match known {
                Known::Pending(i) => Ok(JobProgress::Queued { position: u32::try_from(i.saturating_add(1)).ok() }),
                Known::Running => {
                    let (fraction, message) = now.map_or((None, None), |(fraction, node)| (Some(fraction), node));
                    Ok(JobProgress::Running { fraction, message, partial: None })
                }
                Known::Finished(entry) => match finished(&entry) {
                    Ok(files) => Ok(JobProgress::Done(Completed::new(self.download(&files).await?))),
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
            if let Some(live) = &self.live {
                live.forget(&id);
            }
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
      "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
      "LoadAudio": {"input": {"required": {"audio": [["a.wav"], {"audio_upload": true}]}}, "output": ["AUDIO"]},
      "LoadVideo": {"input": {"required": {"file": [["a.mp4"], {"video_upload": true}]}}, "output": ["VIDEO"]},
      "SaveAudio": {"input": {"required": {"audio": ["AUDIO", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
      "SaveVideo": {"input": {"required": {"video": ["VIDEO", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
      "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["flux1-dev-fp8.safetensors"]]}}, "output": ["MODEL", "CLIP", "VAE"]},
      "FluxGuidance": {"input": {"required": {"conditioning": ["CONDITIONING"], "guidance": ["FLOAT", {"default": 3.5}]}}, "output": ["CONDITIONING"]},
      "ImageScaleToTotalPixels": {"input": {"required": {"image": ["IMAGE"], "upscale_method": [["lanczos"]], "megapixels": ["FLOAT", {}]}}, "output": ["IMAGE"]},
      "VAEEncode": {"input": {"required": {"pixels": ["IMAGE"], "vae": ["VAE"]}}, "output": ["LATENT"]},
      "KSampler": {"input": {"required": {
        "model": ["MODEL"], "positive": ["CONDITIONING"], "negative": ["CONDITIONING"], "latent_image": ["LATENT"],
        "seed": ["INT", {"min": 0, "max": 18446744073709551615}], "steps": ["INT", {"min": 1, "max": 10000}], "cfg": ["FLOAT", {"min": 0.0, "max": 100.0}],
        "sampler_name": [["euler"]], "scheduler": [["simple", "normal"]], "denoise": ["FLOAT", {"min": 0.0, "max": 1.0}]
      }}, "output": ["LATENT"]},
      "VAEDecode": {"input": {"required": {"samples": ["LATENT"], "vae": ["VAE"]}}, "output": ["IMAGE"]}
    }"#;
    const EMPTY_QUEUE: &str = r#"{"queue_running":[],"queue_pending":[]}"#;
    const SALT: &str = "install-salt";
    /// The OpenEXR magic plus a version field: all `sniff` reads, and all Comfy can ever be given.
    const EXR_BYTES: &[u8] = b"\x76\x2f\x31\x01\x02\x00\x00\x00";
    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nimage";

    /// A scripted ComfyUI server: answers by path (queue and history answers can be scripted as a
    /// sequence), records requests.
    struct FakeServer {
        seen: Mutex<Vec<HttpRequest>>,
        history: Mutex<VecDeque<String>>,
        queue: Mutex<VecDeque<String>>,
        view: Mutex<Vec<u8>>,
        /// `/view` answers by file name, before `view`.
        files: Mutex<Vec<(String, Vec<u8>)>>,
        object_info_calls: AtomicUsize,
        /// A checkpoint `/object_info` lists from its second fetch on, as if installed meanwhile.
        installed_later: Mutex<Option<String>>,
    }

    impl FakeServer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(vec![]),
                history: Mutex::new(VecDeque::new()),
                queue: Mutex::new(VecDeque::new()),
                view: Mutex::new(b"\x89PNG\r\n\x1a\nimage".to_vec()),
                files: Mutex::new(vec![]),
                object_info_calls: AtomicUsize::new(0),
                installed_later: Mutex::new(None),
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
                    let earlier = self.object_info_calls.fetch_add(1, Ordering::SeqCst);
                    match self.installed_later.lock().unwrap().as_deref() {
                        Some(name) if earlier > 0 => {
                            OBJECT_INFO.replace(r#"[["flux1-dev-fp8.safetensors"]]"#, &format!(r#"[["flux1-dev-fp8.safetensors", "{name}"]]"#)).into()
                        }
                        _ => OBJECT_INFO.into(),
                    }
                }
                "/queue" if request.method == HttpMethod::Get => Self::next(&self.queue, EMPTY_QUEUE).into_bytes(),
                "/upload/image" => r#"{"name":"stored.png","subfolder":"in","type":"input"}"#.into(),
                "/prompt" => {
                    let v: Value = serde_json::from_slice(&request.body).unwrap();
                    format!(r#"{{"prompt_id":"{}","number":1,"node_errors":{{}}}}"#, v["prompt_id"].as_str().unwrap()).into_bytes()
                }
                "/view" => {
                    let files = self.files.lock().unwrap();
                    let named = files.iter().find(|(name, _)| path.contains(&format!("filename={name}")));
                    named.map_or_else(|| self.view.lock().unwrap().clone(), |(_, bytes)| bytes.clone())
                }
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
    fn submission_evidence_is_conservative_for_every_comfy_stage_and_cached_local_error() {
        struct FailsAt {
            server: Arc<FakeServer>,
            path: &'static str,
        }
        impl Http for FailsAt {
            fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
                if request.url.contains(self.path) {
                    Box::pin(async { Err(ProviderError::new(ErrorKind::Auth, "public fixture failure")) })
                } else {
                    self.server.send(request)
                }
            }
        }
        for path in ["/queue", "/history/", "/object_info", "/upload/image", "/prompt"] {
            let p = ComfyProvider::new("http://gpu-box:8188", None, SALT, Arc::new(FailsAt { server: FakeServer::new(), path }), Arc::new(|| 0)).unwrap();
            let c = crate::SubmitContext::new();
            let request = job();
            assert!(block_on(p.submit_with_delivery(&request, &c)).is_err(), "{path}");
            assert!(c.finish_error().is_none());
        }
        let server = FakeServer::new();
        let p = provider(server);
        block_on(p.catalog()).unwrap();
        let mut bad = job();
        bad.params.clear();
        let c = crate::SubmitContext::new();
        assert!(block_on(p.submit_with_delivery(&bad, &c)).is_err());
        assert!(c.finish_error().is_none());
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
    fn a_catalog_card_runs_its_built_in_workflow() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let card = ProviderJob {
            model: "flux-dev".into(),
            params: serde_json::json!({"steps": 20, "seed": 7}).as_object().cloned().unwrap(),
            inputs: vec![
                JobInput::Text { role: "prompt".into(), text: "as a watercolour".into() },
                JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(PNG_BYTES) },
            ],
            idempotency_key: "nodecraft:run-uuid:2".into(),
        };
        block_on(p.submit(&card)).unwrap();
        let seen = server.seen.lock().unwrap();
        let prompt = seen.iter().find(|r| r.url.ends_with("/prompt")).unwrap();
        let body: Value = serde_json::from_slice(&prompt.body).unwrap();
        let workflow = &body["prompt"];
        assert_eq!(workflow["1"]["inputs"]["ckpt_name"], "flux1-dev-fp8.safetensors");
        assert_eq!(workflow["2"]["inputs"]["text"], "as a watercolour");
        assert_eq!(workflow["5"]["inputs"]["image"], "in/stored.png", "the input image is uploaded and set on the loader");
        assert_eq!(workflow["8"]["inputs"]["steps"], 20);
        assert_eq!(workflow["10"]["class_type"], "SaveImage");
        drop(seen);

        let mut sdxl = card.clone();
        sdxl.model = "sdxl-base".into();
        let e = block_on(p.submit(&sdxl)).unwrap_err();
        assert!(e.message.contains("sd_xl_base_1.0.safetensors"), "{e}");
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 2, "a missing checkpoint refetches the catalog once");

        // Installed after the catalog was fetched: the refetch finds it and the run is queued.
        let server = FakeServer::new();
        *server.installed_later.lock().unwrap() = Some("sd_xl_base_1.0.safetensors".into());
        let p = provider(server.clone());
        block_on(p.catalog()).unwrap();
        block_on(p.submit(&sdxl)).unwrap();
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 2);
        assert!(server.paths().iter().any(|path| path == "/prompt"));
    }

    /// A workflow card's job (#9): no parameters, the workflow and values as text inputs.
    fn card_job(values: &str) -> ProviderJob {
        let workflow = r#"{
            "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "old"}},
            "3": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "NodeCraft"}}
        }"#;
        ProviderJob {
            model: "workflow".into(),
            params: Map::new(),
            inputs: vec![
                JobInput::Text { role: template::CARD_WORKFLOW.into(), text: workflow.into() },
                JobInput::Text { role: template::CARD_VALUES.into(), text: values.into() },
                JobInput::Media { role: "1.image".into(), mime: "image/png".into(), bytes: Arc::from(PNG_BYTES) },
            ],
            idempotency_key: "nodecraft:run-uuid:9".into(),
        }
    }

    #[test]
    fn a_workflow_card_runs_the_workflow_it_carries() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let card = card_job(r#"{"2.text": "a red fox"}"#);
        assert!(!template::is_templated(&card), "a card's workflow is never replaced by a built-in one");
        block_on(p.submit(&card)).unwrap();
        let seen = server.seen.lock().unwrap();
        let prompt = seen.iter().find(|r| r.url.ends_with("/prompt")).unwrap();
        let body: Value = serde_json::from_slice(&prompt.body).unwrap();
        assert_eq!(body["prompt"]["2"]["inputs"]["text"], "a red fox", "the card's value is set");
        assert_eq!(body["prompt"]["1"]["inputs"]["image"], "in/stored.png", "the wired file is uploaded and set");
        assert_eq!(body["prompt"]["3"]["class_type"], "SaveImage");
        drop(seen);
        // Without values the workflow keeps its own.
        let server = FakeServer::new();
        let mut plain = card_job("");
        plain.inputs.retain(|i| !matches!(i, JobInput::Text { role, .. } if role == template::CARD_VALUES));
        block_on(provider(server.clone()).submit(&plain)).unwrap();
        let seen = server.seen.lock().unwrap();
        let body: Value = serde_json::from_slice(&seen.iter().find(|r| r.url.ends_with("/prompt")).unwrap().body).unwrap();
        assert_eq!(body["prompt"]["2"]["inputs"]["text"], "old");
    }

    #[test]
    fn a_workflow_card_with_bad_values_or_workflow_uploads_nothing() {
        let deep = format!("{{\"2.text\": {}1{}}}", "[".repeat(200), "]".repeat(200));
        let huge = format!("{{\"2.text\": \"{}\"}}", "x".repeat(MAX_CARD_TEXT_BYTES));
        let many = format!("{{{}}}", (0..=MAX_CARD_VALUES).map(|n| format!("\"2.t{n}\": 1")).collect::<Vec<_>>().join(","));
        let bad = [
            "not json".to_owned(),
            "[1, 2]".to_owned(),
            r#"{"2.text": {"nested": 1}}"#.to_owned(),
            r#"{"nodot": "x"}"#.to_owned(),
            r#"{"2.missing": "x"}"#.to_owned(),
            r#"{"9.text": "x"}"#.to_owned(),
            deep,
            huge,
            many,
        ];
        for values in bad {
            let server = FakeServer::new();
            let e = block_on(provider(server.clone()).submit(&card_job(&values))).unwrap_err();
            assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{e}");
            assert!(!server.paths().iter().any(|p| p == "/upload/image" || p == "/prompt"), "nothing uploaded for {}", &values[..values.len().min(40)]);
        }
        let mut broken = card_job("{}");
        broken.inputs[0] = JobInput::Text { role: template::CARD_WORKFLOW.into(), text: "{\"1\": [".into() };
        let server = FakeServer::new();
        assert!(block_on(provider(server.clone()).submit(&broken)).is_err());
        assert!(!server.paths().iter().any(|p| p == "/upload/image"));
        let mut oversized = card_job("{}");
        oversized.inputs[0] = JobInput::Text { role: template::CARD_WORKFLOW.into(), text: " ".repeat(MAX_CARD_TEXT_BYTES + 1) };
        let server = FakeServer::new();
        assert!(block_on(provider(server.clone()).submit(&oversized)).unwrap_err().message.contains("too large"));
        assert!(!server.paths().iter().any(|p| p == "/upload/image"));
        // A job carrying a workflow both ways is ambiguous.
        let mut both = card_job("{}");
        both.params = job().params;
        let server = FakeServer::new();
        assert!(block_on(provider(server.clone()).submit(&both)).unwrap_err().message.contains("two"));
        assert!(!server.paths().iter().any(|p| p == "/upload/image"));
    }

    #[test]
    fn a_workflow_card_queues_only_the_outputs_it_keeps() {
        let workflow = r#"{
            "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "3": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "kept"}},
            "4": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "dropped"}}
        }"#;
        let with_outputs = |outputs: &str| {
            let mut job = card_job("{}");
            job.inputs[0] = JobInput::Text { role: template::CARD_WORKFLOW.into(), text: workflow.into() };
            job.inputs.push(JobInput::Text { role: template::CARD_OUTPUTS.into(), text: outputs.into() });
            job
        };
        let server = FakeServer::new();
        block_on(provider(server.clone()).submit(&with_outputs(r#"["3"]"#))).unwrap();
        let seen = server.seen.lock().unwrap();
        let body: Value = serde_json::from_slice(&seen.iter().find(|r| r.url.ends_with("/prompt")).unwrap().body).unwrap();
        assert_eq!(body["prompt"]["3"]["inputs"]["filename_prefix"], "kept");
        assert!(body["prompt"].get("4").is_none(), "an output the card has no port for never runs");
        assert!(body["prompt"].get("1").is_some(), "nodes that aren't outputs stay");
        drop(seen);
        for bad in ["[]", r#"["9"]"#, "nope", r#"{"3": 1}"#] {
            let server = FakeServer::new();
            let e = block_on(provider(server.clone()).submit(&with_outputs(bad))).unwrap_err();
            assert!(e.message.contains("outputs"), "{bad}: {e}");
            assert!(!server.paths().iter().any(|p| p == "/upload/image" || p == "/prompt"), "nothing uploaded for {bad}");
        }
        // A card's inputs never mix with a workflow carried in the parameters, or the other way.
        let mut mixed = job();
        mixed.inputs.push(JobInput::Text { role: template::CARD_OUTPUTS.into(), text: r#"["3"]"#.into() });
        assert!(block_on(provider(FakeServer::new()).submit(&mixed)).unwrap_err().message.contains("mixes"));
        let mut mixed = card_job("{}");
        mixed.params.insert("values".into(), serde_json::json!({"2.text": "x"}));
        assert!(block_on(provider(FakeServer::new()).submit(&mixed)).unwrap_err().message.contains("as an input"));
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
        assert!(matches!(block_on(p.poll(&handle_for("abc"))).unwrap(), JobProgress::Done(o) if o.outputs.len() == 1));
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
        let JobProgress::Done(Completed { outputs, .. }) = block_on(p.poll(&handle_for("abc"))).unwrap() else { panic!("not done") };
        assert_eq!(outputs.len(), 1, "previews (temp) are not outputs");
        assert!(matches!(&outputs[0], Output::Bytes { mime, .. } if mime == "image/png"));
    }

    #[test]
    fn a_running_prompt_reports_progress_from_the_socket() {
        use progress::tests::{Scripted, text};
        let server = FakeServer::new();
        let messages = Arc::new(Mutex::new(VecDeque::new()));
        let feeds = Scripted { messages: messages.clone(), opens: Arc::default(), refuse: false };
        let opens = feeds.opens.clone();
        let p = provider(server.clone()).with_progress(Arc::new(feeds));
        let Submission::Queued(handle) = block_on(p.submit(&job())).unwrap() else { panic!("not queued") };
        assert_eq!(*opens.lock().unwrap(), 1, "the socket is open before the prompt is queued");
        let id = handle.remote_id.clone();

        FakeServer::set(&server.queue, &[&format!(r#"{{"queue_running":[],"queue_pending":[[0,"{id}"]]}}"#)]);
        assert_eq!(block_on(p.poll(&handle)).unwrap(), JobProgress::Queued { position: Some(1) });
        FakeServer::set(&server.queue, &[&format!(r#"{{"queue_running":[[0,"{id}"]],"queue_pending":[]}}"#)]);
        assert_eq!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { fraction: None, message: None, partial: None }, "not started yet");
        messages.lock().unwrap().extend([
            text(&format!(r#"{{"type":"execution_start","data":{{"prompt_id":"{id}"}}}}"#)),
            text(&format!(r#"{{"type":"execution_cached","data":{{"prompt_id":"{id}","nodes":["1"]}}}}"#)),
            text(&format!(r#"{{"type":"executing","data":{{"node":"2","prompt_id":"{id}"}}}}"#)),
        ]);
        let JobProgress::Running { fraction: Some(fraction), message, .. } = block_on(p.poll(&handle)).unwrap() else { panic!("no fraction") };
        assert!((fraction - 1.0 / 3.0).abs() < 1e-6, "{fraction}");
        assert_eq!(message.as_deref(), Some("CLIPTextEncode"));

        FakeServer::set(&server.queue, &[EMPTY_QUEUE]);
        FakeServer::set(&server.history, &[&history_ok(&id, r#"[{"filename":"a.png","subfolder":"","type":"output"}]"#)]);
        assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Done(_)));
        assert!(!p.live.as_ref().unwrap().wanted(), "a finished prompt is no longer tracked");
        assert_eq!(*opens.lock().unwrap(), 1, "an open socket is reused");
    }

    #[test]
    fn without_a_socket_runs_still_finish_by_polling() {
        let server = FakeServer::new();
        let feeds = progress::tests::Scripted { messages: Arc::default(), opens: Arc::default(), refuse: true };
        let opens = feeds.opens.clone();
        let p = provider(server.clone()).with_progress(Arc::new(feeds));
        let Submission::Queued(handle) = block_on(p.submit(&job())).unwrap() else { panic!("not queued") };
        let id = handle.remote_id.clone();
        FakeServer::set(&server.queue, &[&format!(r#"{{"queue_running":[[0,"{id}"]],"queue_pending":[]}}"#)]);
        assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { fraction: None, .. }));
        assert_eq!(*opens.lock().unwrap(), 1, "a refused socket isn't retried on every poll");
    }

    #[test]
    fn a_cancelled_prompt_is_no_longer_tracked() {
        let server = FakeServer::new();
        let feeds = progress::tests::Scripted { messages: Arc::default(), opens: Arc::default(), refuse: false };
        let p = provider(server.clone()).with_progress(Arc::new(feeds));
        let Submission::Queued(handle) = block_on(p.submit(&job())).unwrap() else { panic!("not queued") };
        assert!(p.live.as_ref().unwrap().wanted());
        FakeServer::set(&server.queue, &[EMPTY_QUEUE]);
        block_on(p.cancel(&handle)).unwrap();
        assert!(!p.live.as_ref().unwrap().wanted(), "nothing keeps the socket open");
    }

    const WAV: &[u8] = include_bytes!("fal/fixtures/tone.wav");
    const MP4: &[u8] = include_bytes!("fal/fixtures/blue.mp4");

    /// Loads a sound and a video, saves both (#110).
    fn av_job(sound_to: &str) -> ProviderJob {
        let workflow = serde_json::json!({
            "1": {"class_type": "LoadAudio", "inputs": {"audio": "a.wav"}},
            "2": {"class_type": "LoadVideo", "inputs": {"file": "a.mp4"}},
            "3": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "4": {"class_type": "SaveVideo", "inputs": {"video": ["2", 0], "filename_prefix": "NodeCraft"}},
            "5": {"class_type": "SaveAudio", "inputs": {"audio": ["1", 0], "filename_prefix": "NodeCraft"}}
        });
        ProviderJob {
            model: "comfyui:workflow".into(),
            params: serde_json::json!({"workflow": workflow}).as_object().cloned().unwrap(),
            inputs: vec![
                // The video comes first, so a refused sound shows nothing at all was uploaded.
                JobInput::Media { role: "2.file".into(), mime: "video/mp4".into(), bytes: Arc::from(MP4) },
                JobInput::Media { role: sound_to.into(), mime: "audio/wav".into(), bytes: Arc::from(WAV) },
            ],
            idempotency_key: "nodecraft:run-uuid:av".into(),
        }
    }

    #[test]
    fn sounds_and_videos_upload_under_the_names_their_loaders_list() {
        let server = FakeServer::new();
        block_on(provider(server.clone()).submit(&av_job("1.audio"))).unwrap();
        let seen = server.seen.lock().unwrap();
        let names: Vec<String> = seen
            .iter()
            .filter(|r| r.url.ends_with("/upload/image"))
            .map(|r| String::from_utf8_lossy(&r.body).split("filename=\"").nth(1).unwrap_or_default().split('"').next().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].starts_with("nodecraft-") && names[0].ends_with(".mp4") && names[1].ends_with(".wav"), "{names:?}");
        assert!(seen.iter().any(|r| r.url.ends_with("/prompt")));
    }

    #[test]
    fn a_sound_sent_to_an_image_loader_is_refused_before_anything_is_uploaded() {
        let server = FakeServer::new();
        let e = block_on(provider(server.clone()).submit(&av_job("3.image"))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{e}");
        assert_eq!(e.message, "3.image takes an image, not audio/wav");
        let paths = server.paths();
        assert!(!paths.iter().any(|p| p.starts_with("/upload") || p == "/prompt"), "{paths:?}");
    }

    #[test]
    fn saved_videos_and_sounds_come_back_as_outputs() {
        let server = FakeServer::new();
        *server.files.lock().unwrap() = vec![("clip.mp4".into(), MP4.to_vec()), ("line.wav".into(), WAV.to_vec())];
        // A video saver's files come under `gifs` or `images`, a sound saver's under `audio`.
        FakeServer::set(
            &server.history,
            &[r#"{"av":{"outputs":{"4":{"gifs":[{"filename":"clip.mp4","subfolder":"video","type":"output","format":"video/h264-mp4"}]},
                "5":{"audio":[{"filename":"line.wav","subfolder":"audio","type":"output"}]}},
                "status":{"status_str":"success","completed":true,"messages":[]}}}"#],
        );
        let JobProgress::Done(Completed { outputs, .. }) = block_on(provider(server).poll(&handle_for("av"))).unwrap() else { panic!("not done") };
        let mimes: Vec<&str> = outputs
            .iter()
            .map(|o| match o {
                Output::Bytes { mime, .. } => mime.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(mimes, ["video/mp4", "audio/wav"]);
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

    /// A server whose finished prompt wrote `out.png` *and* an `out.exr` side by side; `/view`
    /// answers per file, which one scripted body can't.
    struct SavesPngAndExr;

    impl Http for SavesPngAndExr {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            let path = request.url.split("8188").nth(1).unwrap_or_default().to_owned();
            let body: Vec<u8> = match path.split('?').next().unwrap_or_default() {
                "/queue" => EMPTY_QUEUE.as_bytes().to_vec(),
                "/view" if request.url.contains("filename=out.exr") => EXR_BYTES.to_vec(),
                "/view" => PNG_BYTES.to_vec(),
                p if p.starts_with("/history/") => {
                    history_ok("abc", r#"[{"filename":"out.png","subfolder":"","type":"output"},{"filename":"out.exr","subfolder":"","type":"output"}]"#)
                        .into_bytes()
                }
                _ => b"{}".to_vec(),
            };
            Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
        }
    }

    /// #394 made an EXR sniff as an image. Recognition is not permission: the undecodable file is
    /// skipped, so a workflow that saved a PNG alongside it still delivers that PNG instead of the
    /// engine refusing the whole (already paid for) run.
    #[test]
    fn an_hdr_output_is_skipped_and_the_png_from_the_same_run_lands() {
        let p = ComfyProvider::new("http://gpu-box:8188", None, SALT, Arc::new(SavesPngAndExr), Arc::new(|| 0)).unwrap();
        let JobProgress::Done(Completed { outputs, .. }) = block_on(p.poll(&handle_for("abc"))).unwrap() else { panic!("not done") };
        assert_eq!(outputs.len(), 1, "the EXR must be skipped, not kept: {outputs:?}");
        assert!(matches!(&outputs[0], Output::Bytes { mime, .. } if mime == "image/png"), "{outputs:?}");
    }

    /// The same boundary on the way in: an HDR input is refused before `/upload/image`, exactly as
    /// it was while `sniff` didn't know these bytes at all, so nothing unopenable lands on the
    /// user's server and the prompt is never queued.
    #[test]
    fn an_hdr_input_is_refused_before_it_is_uploaded() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let mut j = job();
        j.inputs = vec![JobInput::Media { role: "1.image".into(), mime: "image/x-exr".into(), bytes: Arc::from(EXR_BYTES) }];
        let e = block_on(p.submit(&j)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{e}");
        assert!(e.message.contains("1.image") && e.message.contains("image/x-exr") && e.message.contains("PNG"), "{e}");
        let paths = server.paths();
        assert!(!paths.iter().any(|p| p.starts_with("/upload")), "recognition grants no upload permission: {paths:?}");
        assert!(!paths.iter().any(|p| p == "/prompt"), "a run that can't work is never queued: {paths:?}");
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
