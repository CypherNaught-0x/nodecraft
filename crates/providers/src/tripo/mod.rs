//! Tripo 3D generation (#22): text, one image or up to four views in, one textured GLB model out.
//! Images upload first (free); creating the task is the paid step and is never replayed
//! automatically. Tripo has no cancellation endpoint, and its result links expire after minutes,
//! so a finished task downloads at once.
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
};
use std::sync::Arc;

pub const TEXT: &str = "text_to_model";
pub const IMAGE: &str = "image_to_model";
pub const MULTIVIEW: &str = "multiview_to_model";
const API: &str = "https://api.tripo3d.ai/v2/openapi";
const MIB: usize = 1024 * 1024;
const GLB: &str = "model/gltf-binary";

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Tripo's documented caps: 1024 characters of prompt, 255 of negative prompt.
    pub prompt_chars: usize,
    pub negative_chars: usize,
    /// Each uploaded image (Tripo takes up to 20 MB).
    pub input_bytes: usize,
    /// JSON replies (task receipts and status).
    pub response_bytes: usize,
    /// The downloaded model.
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { prompt_chars: 1024, negative_chars: 255, input_bytes: 20 * MIB, response_bytes: MIB, output_bytes: 256 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.negative_chars > h.negative_chars
            || self.input_bytes > h.input_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
        {
            return Err(invalid("Tripo limits may only be lowered"));
        }
        Ok(self)
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn transport(e: ProviderError, paid: bool) -> ProviderError {
    let message = if paid { "Tripo transport failed; the task may have been created and billed" } else { "Tripo transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}
/// A task id as Tripo issues them (a UUID); anything else is refused before it reaches a URL.
fn task_id(s: &str, key: &Secret) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || s.contains(key.expose()) {
        return Err(malformed("Tripo task id refused"));
    }
    Ok(())
}

/// Fixed-origin queue adapter. Constructing it reads no credential and sends nothing.
pub struct Tripo {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Tripo {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    fn auth(&self, r: HttpRequest) -> HttpRequest {
        r.secret_header("Authorization", format!("Bearer {}", self.key.expose())).max_response_bytes(self.limits.response_bytes)
    }
    async fn send(&self, request: HttpRequest, paid: bool) -> Result<HttpResponse, ProviderError> {
        let reply = self.http.send_no_redirect(self.auth(request)).await.map_err(|e| transport(e, paid))?;
        response::check(&reply, self.limits, &self.key)?;
        Ok(reply)
    }
    async fn create(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        // Uploads are free: a failure here proves no task was created.
        let mut tokens = Vec::new();
        tokens.try_reserve_exact(prepared.images.len()).map_err(|_| malformed("Tripo bounded allocation failed"))?;
        for image in &prepared.images {
            let upload =
                HttpRequest::post_multipart(format!("{API}/upload"), &[], &[("file", image.file_name(), image.mime, image.bytes)])?.timeout_ms(120_000);
            let reply = self.send(upload, false).await?;
            tokens.push(response::upload_token(&reply.body, &self.key)?);
        }
        let body = prepared.body(&tokens)?;
        let create = HttpRequest::post_json(format!("{API}/task"), &body)?.timeout_ms(60_000);
        // Everything local is done; from here a lost reply may hide a created, billed task.
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.send(create, true).await?;
        let id = response::created(&reply.body, &self.key)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::json!({"v": 1, "type": prepared.task}) }))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        task_id(&handle.remote_id, &self.key)?;
        let task = handle.data.get("type").and_then(serde_json::Value::as_str).filter(|t| [TEXT, IMAGE, MULTIVIEW].contains(t));
        if handle.data.get("v").and_then(serde_json::Value::as_u64) != Some(1) || task.is_none() {
            return Err(invalid("Tripo handle record refused"));
        }
        let reply = self.send(HttpRequest::get(format!("{API}/task/{}", handle.remote_id)).timeout_ms(30_000), false).await?;
        match response::status(&reply.body, &handle.remote_id, &self.key)? {
            response::Status::Done(url) => {
                drop(reply);
                self.download(&url).await.map(|bytes| JobProgress::Done(Completed::new(vec![Output::Bytes { mime: GLB.into(), bytes }])))
            }
            response::Status::Progress(progress) => Ok(progress),
        }
    }
    /// The result link is pre-signed on Tripo's storage: fetched without the key.
    async fn download(&self, url: &str) -> Result<Arc<[u8]>, ProviderError> {
        let request = HttpRequest::get(url).max_response_bytes(self.limits.output_bytes).timeout_ms(300_000);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, false))?;
        if !reply.is_success() {
            let mut error = ProviderError::from_status(reply.status, b"");
            error.message = format!("the Tripo model download failed (HTTP {}); the task was billed", reply.status);
            return Err(error);
        }
        if reply.body.len() > self.limits.output_bytes {
            return Err(malformed("the Tripo model exceeds the download limit"));
        }
        response::glb(&reply.body)?;
        Ok(Arc::from(reply.body))
    }
}
impl Provider for Tripo {
    fn id(&self) -> &str {
        "tripo"
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
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Tripo can't cancel a task; it runs to the end and is billed")) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        3_000
    }
}
