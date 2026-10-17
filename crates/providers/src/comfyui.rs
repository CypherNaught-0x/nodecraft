//! ComfyUI as a provider: runs an API-format workflow on a local or remote ComfyUI server.
//!
//! The protocol lives in `nodecraft_comfy` (sans-IO); this module sends its requests over
//! [`Http`]. A job carries:
//! - `params.workflow`: the API-format workflow (the Workflow node's mapped graph);
//! - `params.values`: `{"<node>.<input>": value}` overrides, checked against `/object_info`;
//! - media inputs whose role is `<node>.<input>`: uploaded first, then set to the stored name.
//!
//! The prompt id is derived from the job's idempotency key, and a submit first asks the server
//! whether it already has that prompt, so a retried submit never queues the work twice.
//! Outputs are downloaded with the server's auth when the prompt finishes, size-capped and sniffed.
//! Live progress over the WebSocket is not wired yet: polling reports queue position and
//! completion.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use nodecraft_comfy::{self as comfy, ApiWorkflow, ComfyError, FileRef, HistoryEntry, NodeCatalog, PromptRequest, QueueState, Server, UploadKind};

use crate::http::{Http, HttpMethod, HttpRequest, HttpResponse};
use crate::provider::{BoxFuture, JobHandle, JobInput, JobProgress, Output, Provider, ProviderJob, Submission};
use crate::{ErrorKind, ProviderError};

/// Cap on one downloaded output file.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;
/// How long a fetched `/object_info` is reused before fetching it again.
const CATALOG_TTL_MS: u64 = 5 * 60 * 1000;

/// A ComfyUI server as a provider.
pub struct ComfyProvider {
    server: Server,
    /// The auth header, if any: sent as a secret header (never across a redirect to another host).
    auth: Option<(String, String)>,
    http: Arc<dyn Http>,
    client_id: String,
    max_output_bytes: usize,
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
        ComfyError::InvalidUrl(_) | ComfyError::InvalidSetting(_) => ErrorKind::UnsupportedInput,
        ComfyError::MissingNodes(_) | ComfyError::InvalidWorkflow(_) | ComfyError::InvalidInput { .. } => ErrorKind::UnsupportedInput,
        ComfyError::TooLarge { .. } | ComfyError::Malformed { .. } => ErrorKind::Malformed,
        ComfyError::Server { status, .. } => match status {
            400 => ErrorKind::UnsupportedInput,
            s => ProviderError::from_status(*s, b"").kind,
        },
        _ => ErrorKind::Server,
    };
    ProviderError::new(kind, e.to_string())
}

/// A prompt id in UUID form derived from the idempotency key: the same job always gets the same id.
pub fn prompt_id_for(idempotency_key: &str) -> String {
    let h = nodecraft_media::ContentHash::of(idempotency_key.as_bytes()).0;
    let hex: String = h.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let part = |a: usize, b: usize| hex.get(a..b).unwrap_or_default().to_owned();
    format!("{}-{}-{}-{}-{}", part(0, 8), part(8, 12), part(12, 16), part(16, 20), part(20, 32))
}

impl ComfyProvider {
    /// A provider for the server at `url`, with an optional auth header (from the keychain).
    /// `now_ms` is a monotonic clock for the catalog cache.
    pub fn new(url: &str, auth: Option<(String, String)>, http: Arc<dyn Http>, now_ms: Arc<dyn Fn() -> u64 + Send + Sync>) -> Result<Self, ProviderError> {
        let mut server = Server::new(url).map_err(comfy_error)?;
        if let Some((name, value)) = &auth {
            server = server.with_auth_header(name, value).map_err(comfy_error)?;
        }
        let client_id = format!("nodecraft-{}", prompt_id_for(url));
        Ok(Self { server, auth, http, client_id, max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES, now_ms, catalog: Mutex::new(None) })
    }

    pub fn with_max_output_bytes(mut self, max: usize) -> Self {
        self.max_output_bytes = max;
        self
    }

    /// Sends a comfy-crate request; the auth header goes as a secret header.
    async fn send(&self, request: comfy::Request, max_bytes: usize) -> Result<HttpResponse, ProviderError> {
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
        if let Some(ct) = content_type {
            http = http.header("Content-Type", ct);
        }
        if let Some((name, value)) = &self.auth {
            http = http.secret_header(name.clone(), value.clone());
        }
        self.http.send(http).await
    }

    async fn send_ok(&self, request: comfy::Request, max_bytes: usize) -> Result<HttpResponse, ProviderError> {
        self.send(request, max_bytes).await?.error_for_status()
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

    /// Builds the workflow to queue: parse, apply value overrides, upload and set media inputs,
    /// then check it against the server (missing custom nodes and models become clear errors).
    async fn prepare(&self, job: &ProviderJob, catalog: &NodeCatalog) -> Result<ApiWorkflow, ProviderError> {
        let raw = job.params.get("workflow").ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "the job has no ComfyUI workflow"))?;
        let bytes = serde_json::to_vec(raw).map_err(|e| ProviderError::new(ErrorKind::UnsupportedInput, e.to_string()))?;
        let mut workflow = ApiWorkflow::parse(&bytes, Some(catalog)).map_err(comfy_error)?;
        let empty = Map::new();
        let values = job.params.get("values").and_then(Value::as_object).unwrap_or(&empty);
        for (target, value) in values {
            let (node, input) = split_target(target)?;
            workflow.set_input(catalog, node, input, value.clone()).map_err(comfy_error)?;
        }
        for input in &job.inputs {
            let JobInput::Media { role, mime, bytes } = input else { continue };
            let (node, name) = split_target(role)?;
            let ext = mime.rsplit('/').next().unwrap_or("bin");
            let filename = format!("nodecraft-{}.{ext}", &prompt_id_for(&format!("{}:{role}", job.idempotency_key))[..8]);
            let upload = self.server.upload_image(&filename, bytes, UploadKind::Input, true).map_err(comfy_error)?;
            let reply = self.send_ok(upload, 64 * 1024).await?;
            let stored = comfy::parse_upload_response(&reply.body).map_err(comfy_error)?;
            let name_on_server = if stored.subfolder.is_empty() { stored.filename } else { format!("{}/{}", stored.subfolder, stored.filename) };
            workflow.set_input(catalog, node, name, Value::String(name_on_server)).map_err(comfy_error)?;
        }
        workflow.check(catalog).into_result().map_err(comfy_error)?;
        Ok(workflow)
    }

    async fn history(&self, prompt_id: &str) -> Result<Option<HistoryEntry>, ProviderError> {
        let response = self.send_ok(self.server.history(prompt_id).map_err(comfy_error)?, 64 * 1024 * 1024).await?;
        HistoryEntry::parse(&response.body, prompt_id).map_err(comfy_error)
    }

    async fn queue_state(&self) -> Result<QueueState, ProviderError> {
        let response = self.send_ok(self.server.queue().map_err(comfy_error)?, 64 * 1024 * 1024).await?;
        QueueState::parse(&response.body).map_err(comfy_error)
    }

    /// Downloads a finished prompt's output files with the server's auth.
    async fn download(&self, files: &[FileRef]) -> Result<Vec<Output>, ProviderError> {
        let mut outputs = Vec::new();
        for file in files.iter().filter(|f| f.kind == "output") {
            let response = self.send_ok(self.server.view(file).map_err(comfy_error)?, self.max_output_bytes).await?;
            let checked = crate::check_media(&response.body, None)?;
            outputs.push(Output::Bytes { mime: checked.mime.to_owned(), bytes: Arc::from(checked.bytes) });
        }
        Ok(outputs)
    }
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

impl Provider for ComfyProvider {
    fn id(&self) -> &str {
        "comfyui"
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            let prompt_id = prompt_id_for(&job.idempotency_key);
            // A retried submit whose first attempt reached the server: don't queue it again.
            if self.queue_state().await?.contains(&prompt_id) || self.history(&prompt_id).await?.is_some() {
                return Ok(Submission::Queued(handle_for(&prompt_id)));
            }
            let catalog = self.catalog().await?;
            let workflow = self.prepare(job, &catalog).await?;
            let mut request = PromptRequest::new(workflow, self.client_id.clone());
            request.prompt_id = Some(prompt_id.clone());
            let response = self.send(self.server.prompt(&request).map_err(comfy_error)?, 4 * 1024 * 1024).await?;
            let queued = comfy::parse_prompt_response(response.status, &response.body).map_err(comfy_error)?;
            // Older servers ignore a client-chosen id: follow the one they assigned.
            Ok(Submission::Queued(handle_for(&queued.prompt_id)))
        })
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            let id = handle.remote_id.as_str();
            if let Some(entry) = self.history(id).await? {
                if entry.interrupted {
                    return Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Cancelled, "the prompt was interrupted on the server")));
                }
                if let Some(error) = entry.error {
                    return Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, error)));
                }
                if entry.completed {
                    let files: Vec<FileRef> = entry.outputs.into_iter().flat_map(|(_, files)| files).collect();
                    return Ok(JobProgress::Done(self.download(&files).await?));
                }
            }
            let queue = self.queue_state().await?;
            if queue.running.iter().any(|p| p == id) {
                return Ok(JobProgress::Running { fraction: None, message: None, partial: None });
            }
            match queue.pending.iter().position(|p| p == id) {
                Some(i) => Ok(JobProgress::Queued { position: u32::try_from(i.saturating_add(1)).ok() }),
                // Neither queued, running nor in history: the server restarted or dropped it.
                None => Err(ProviderError::new(ErrorKind::NotFound, "the server no longer knows this prompt (was it restarted?)")),
            }
        })
    }

    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let id = handle.remote_id.clone();
            let queue = self.queue_state().await?;
            if queue.pending.contains(&id) {
                self.send_ok(self.server.delete_from_queue(&[id]).map_err(comfy_error)?, 64 * 1024).await?;
            } else if queue.running.contains(&id) {
                // Interrupt only this prompt, never someone else's on a shared server.
                self.send_ok(self.server.interrupt(Some(&id)).map_err(comfy_error)?, 64 * 1024).await?;
            }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OBJECT_INFO: &str = r#"{
      "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
      "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["CONDITIONING"]},
      "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true}
    }"#;

    /// A scripted ComfyUI server: answers by path, records requests.
    struct FakeServer {
        seen: Mutex<Vec<HttpRequest>>,
        history: Mutex<String>,
        queue: Mutex<String>,
        object_info_calls: AtomicUsize,
    }

    impl FakeServer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(vec![]),
                history: Mutex::new("{}".into()),
                queue: Mutex::new(r#"{"queue_running":[],"queue_pending":[]}"#.into()),
                object_info_calls: AtomicUsize::new(0),
            })
        }
        fn paths(&self) -> Vec<String> {
            self.seen.lock().unwrap().iter().map(|r| r.url.split("8188").nth(1).unwrap_or_default().to_owned()).collect()
        }
    }

    impl Http for FakeServer {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            let path = request.url.split("8188").nth(1).unwrap_or_default().to_owned();
            let body: String = match path.split('?').next().unwrap_or_default() {
                "/object_info" => {
                    self.object_info_calls.fetch_add(1, Ordering::SeqCst);
                    OBJECT_INFO.into()
                }
                "/queue" => self.queue.lock().unwrap().clone(),
                "/upload/image" => r#"{"name":"nodecraft-x.png","subfolder":"","type":"input"}"#.into(),
                "/prompt" => {
                    let v: Value = serde_json::from_slice(&request.body).unwrap();
                    format!(r#"{{"prompt_id":"{}","number":1,"node_errors":{{}}}}"#, v["prompt_id"].as_str().unwrap())
                }
                "/view" => "\u{89}PNG\r\n\u{1a}\nimage".into(),
                "/interrupt" => "{}".into(),
                p if p.starts_with("/history/") => self.history.lock().unwrap().clone(),
                _ => "{}".into(),
            };
            let bytes = if path.starts_with("/view") { b"\x89PNG\r\n\x1a\nimage".to_vec() } else { body.into_bytes() };
            self.seen.lock().unwrap().push(request);
            Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body: bytes }) })
        }
    }

    fn provider(http: Arc<FakeServer>) -> ComfyProvider {
        ComfyProvider::new("http://gpu-box:8188", Some(("X-Comfy-Key".into(), "secret".into())), http, Arc::new(|| 0)).unwrap()
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
            idempotency_key: "nodecraft:run:1".into(),
        }
    }

    #[test]
    fn submit_uploads_inputs_applies_values_and_queues_with_a_derived_id() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let submitted = block_on(p.submit(&job())).unwrap();
        let id = prompt_id_for("nodecraft:run:1");
        assert_eq!(submitted, Submission::Queued(JobHandle { remote_id: id.clone(), data: Value::Null }));
        assert_eq!(id.len(), 36);
        let seen = server.seen.lock().unwrap();
        let prompt = seen.iter().find(|r| r.url.ends_with("/prompt")).unwrap();
        let body: Value = serde_json::from_slice(&prompt.body).unwrap();
        assert_eq!(body["prompt"]["2"]["inputs"]["text"], "a red fox");
        assert_eq!(body["prompt"]["1"]["inputs"]["image"], "nodecraft-x.png", "the uploaded file is used");
        assert_eq!(body["prompt_id"], id.as_str());
        assert!(prompt.secret_headers.contains(&"x-comfy-key".to_string()), "the auth header is a secret header");
        assert!(seen.iter().any(|r| r.url.ends_with("/upload/image")));
    }

    #[test]
    fn a_retried_submit_does_not_queue_twice() {
        let server = FakeServer::new();
        *server.queue.lock().unwrap() = format!(r#"{{"queue_running":[],"queue_pending":[[1,"{}",{{}},{{}},[]]]}}"#, prompt_id_for("nodecraft:run:1"));
        let p = provider(server.clone());
        assert!(matches!(block_on(p.submit(&job())).unwrap(), Submission::Queued(_)));
        assert!(!server.paths().iter().any(|p| p == "/prompt"), "already queued: nothing posted");
    }

    #[test]
    fn missing_nodes_and_bad_targets_are_clear_errors() {
        let p = provider(FakeServer::new());
        let mut j = job();
        j.params.insert("workflow".into(), serde_json::json!({"1": {"class_type": "FancyUpscale", "inputs": {}}}));
        j.inputs.clear();
        j.params.remove("values");
        let e = block_on(p.submit(&j)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput);
        assert!(e.message.contains("FancyUpscale"), "{e}");
        let mut j = job();
        j.params.insert("values".into(), serde_json::json!({"nodot": 1}));
        assert!(block_on(p.submit(&j)).unwrap_err().message.contains("<node>.<input>"));
    }

    #[test]
    fn poll_reports_queue_progress_and_downloads_outputs() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        let handle = handle_for("abc");
        *server.queue.lock().unwrap() = r#"{"queue_running":[[0,"other"]],"queue_pending":[[1,"x"],[2,"abc"]]}"#.into();
        assert_eq!(block_on(p.poll(&handle)).unwrap(), JobProgress::Queued { position: Some(2) });
        *server.queue.lock().unwrap() = r#"{"queue_running":[[0,"abc"]],"queue_pending":[]}"#.into();
        assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { .. }));
        *server.history.lock().unwrap() =
            r#"{"abc":{"outputs":{"3":{"images":[{"filename":"NodeCraft_00001_.png","subfolder":"","type":"output"},{"filename":"p.png","subfolder":"","type":"temp"}]}},"status":{"status_str":"success","completed":true,"messages":[]}}}"#.into();
        let JobProgress::Done(outputs) = block_on(p.poll(&handle)).unwrap() else { panic!("not done") };
        assert_eq!(outputs.len(), 1, "previews (temp) are not outputs");
        assert!(matches!(&outputs[0], Output::Bytes { mime, .. } if mime == "image/png"));
        let views = server.paths().into_iter().filter(|p| p.starts_with("/view")).count();
        assert_eq!(views, 1);
    }

    #[test]
    fn poll_failures_and_vanished_prompts() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        *server.history.lock().unwrap() = r#"{"abc":{"outputs":{},"status":{"status_str":"error","completed":false,"messages":[["execution_error",{"node_id":"3","exception_message":"CUDA out of memory"}]]}}}"#.into();
        assert!(matches!(block_on(p.poll(&handle_for("abc"))).unwrap(), JobProgress::Failed(e) if e.message.contains("CUDA")));
        *server.history.lock().unwrap() = "{}".into();
        assert_eq!(block_on(p.poll(&handle_for("gone"))).unwrap_err().kind, ErrorKind::NotFound);
    }

    #[test]
    fn cancel_deletes_pending_and_interrupts_only_this_prompt() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        *server.queue.lock().unwrap() = r#"{"queue_running":[[0,"run"]],"queue_pending":[[1,"wait"]]}"#.into();
        block_on(p.cancel(&handle_for("wait"))).unwrap();
        block_on(p.cancel(&handle_for("run"))).unwrap();
        let seen = server.seen.lock().unwrap();
        let del = seen.iter().find(|r| r.url.ends_with("/queue") && r.method == HttpMethod::Post).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&del.body).unwrap(), serde_json::json!({"delete": ["wait"]}));
        let int = seen.iter().find(|r| r.url.ends_with("/interrupt")).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&int.body).unwrap(), serde_json::json!({"prompt_id": "run"}));
    }

    #[test]
    fn the_catalog_is_cached() {
        let server = FakeServer::new();
        let p = provider(server.clone());
        block_on(p.catalog()).unwrap();
        block_on(p.catalog()).unwrap();
        assert_eq!(server.object_info_calls.load(Ordering::SeqCst), 1);
        assert!(!format!("{p:?}").contains("secret"));
    }
}
