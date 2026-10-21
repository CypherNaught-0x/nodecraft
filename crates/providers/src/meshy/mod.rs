//! Meshy 3D generation (#23): text, one image or up to four views to a model, and retexturing or
//! remeshing a wired model. Every input travels inline in the one task request, which is the paid
//! step and is never replayed automatically. Results are pre-signed links on Meshy's storage,
//! downloaded at once without the key.
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
};
use std::sync::Arc;

pub const TEXT: &str = "text-to-3d";
pub const IMAGE: &str = "image-to-3d";
pub const MULTI_IMAGE: &str = "multi-image-to-3d";
pub const RETEXTURE: &str = "retexture";
pub const REMESH: &str = "remesh";
const TASKS: [&str; 5] = [TEXT, IMAGE, MULTI_IMAGE, RETEXTURE, REMESH];
const API: &str = "https://api.meshy.ai/openapi";
const MIB: usize = 1024 * 1024;
const GLB: &str = "model/gltf-binary";

/// Where a task type lives: Text to 3D is on Meshy's v2 API, the rest on v1.
fn endpoint(task: &str) -> String {
    let version = if task == TEXT { "v2" } else { "v1" };
    format!("{API}/{version}/{task}")
}

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Meshy's documented cap on a prompt (and a texture prompt): 800 characters.
    pub prompt_chars: usize,
    /// Each image, before encoding: Meshy takes up to 20 MB, and base64 grows it by a third.
    pub image_bytes: usize,
    /// A wired model, before encoding: Meshy takes data URIs up to 100 MB.
    pub model_bytes: usize,
    /// JSON replies (task receipts and status).
    pub response_bytes: usize,
    /// The downloaded model.
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { prompt_chars: 800, image_bytes: 15 * MIB, model_bytes: 64 * MIB, response_bytes: MIB, output_bytes: 256 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.image_bytes > h.image_bytes
            || self.model_bytes > h.model_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
        {
            return Err(invalid("Meshy limits may only be lowered"));
        }
        Ok(self)
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(s: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn transport(e: ProviderError, paid: bool) -> ProviderError {
    let message = if paid { "Meshy transport failed; the task may have been created and billed" } else { "Meshy transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}
/// A task id as it may appear in a URL path; Meshy documents ids as opaque, so only the
/// characters a path segment carries safely are accepted.
fn task_id(s: &str, key: &Secret) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') || s.contains(key.expose()) {
        return Err(malformed("Meshy task id refused"));
    }
    Ok(())
}

/// Fixed-origin queue adapter. Constructing it reads no credential and sends nothing.
pub struct Meshy {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Meshy {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    async fn exchange(&self, request: HttpRequest, paid: bool) -> Result<HttpResponse, ProviderError> {
        let request = request.secret_header("Authorization", format!("Bearer {}", self.key.expose())).max_response_bytes(self.limits.response_bytes);
        self.http.send_no_redirect(request).await.map_err(|e| transport(e, paid))
    }
    async fn send(&self, request: HttpRequest, paid: bool) -> Result<HttpResponse, ProviderError> {
        let reply = self.exchange(request, paid).await?;
        response::check(&reply, self.limits, &self.key)?;
        Ok(reply)
    }
    /// The task named by a handle this adapter issued; a forged one never reaches a URL.
    fn task<'h>(&self, handle: &'h JobHandle) -> Result<&'h str, ProviderError> {
        task_id(&handle.remote_id, &self.key)?;
        let task = handle.data.get("type").and_then(serde_json::Value::as_str).filter(|t| TASKS.contains(t));
        match (handle.data.get("v").and_then(serde_json::Value::as_u64), task) {
            (Some(1), Some(task)) => Ok(task),
            _ => Err(invalid("Meshy handle record refused")),
        }
    }
    async fn create(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let create = HttpRequest::post_json(endpoint(prepared.task), &prepared.body)?.timeout_ms(300_000);
        drop(prepared.body);
        // Everything local is done; from here a lost reply may hide a created, billed task.
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.send(create, true).await?;
        let id = response::created(&reply.body, &self.key)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::json!({"v": 1, "type": prepared.task}) }))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        let task = self.task(handle)?;
        let request = HttpRequest::get(format!("{}/{}", endpoint(task), handle.remote_id)).timeout_ms(30_000);
        let reply = self.send(request, false).await?;
        match response::status(&reply.body, &handle.remote_id, &self.key)? {
            response::Status::Done(url) => {
                drop(reply);
                self.download(&url).await.map(|bytes| JobProgress::Done(Completed::new(vec![Output::Bytes { mime: GLB.into(), bytes }])))
            }
            response::Status::Progress(progress) => Ok(progress),
        }
    }
    /// The result link is pre-signed on Meshy's storage: fetched without the key.
    async fn download(&self, url: &str) -> Result<Arc<[u8]>, ProviderError> {
        let request = HttpRequest::get(url).max_response_bytes(self.limits.output_bytes).timeout_ms(300_000);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, false))?;
        if !reply.is_success() {
            // Retryable whatever the status: the next poll hands out a fresh link, so a billed
            // model is never given up on a stale one.
            let mut error = ProviderError::new(ErrorKind::Server, format!("the Meshy model download failed (HTTP {}); the task was billed", reply.status));
            error.retry_after_ms = ProviderError::from_status(reply.status, b"").retry_after_ms;
            return Err(error);
        }
        if reply.body.len() > self.limits.output_bytes {
            return Err(malformed("the Meshy model exceeds the download limit"));
        }
        crate::model_file::glb(&reply.body, "Meshy")?;
        Ok(Arc::from(reply.body))
    }
}
impl Provider for Meshy {
    fn id(&self) -> &str {
        "meshy"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(j, None))
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; create marks uncertainty before the paid request.
        context.arm();
        Box::pin(self.create(j, Some(context)))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(h))
    }
    /// Meshy's DELETE erases a task and its models, and nothing documents a refund or a refusal
    /// for a running task, so a cancel could lose a billed model: it is unsupported.
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Meshy can't cancel a task; it runs to the end and is billed")) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        3_000
    }
}
