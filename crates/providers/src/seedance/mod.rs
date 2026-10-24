//! Seedance video on BytePlus ModelArk (#18): text, a first frame (and optionally a last one) or
//! reference images to a video with sound. Creating the task is the paid step and is never
//! replayed: ModelArk has no idempotency key. BytePlus bills only a video that succeeds; the task
//! is polled, and the finished video's link (valid for 24 hours) is fetched without the key.
//! A queued task can be cancelled; a running one can't.
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext, Usage,
};
use std::sync::Arc;

pub const V2_5: &str = "dreamina-seedance-2-5-260628";
pub const V2_0: &str = "dreamina-seedance-2-0-260128";
pub const V2_0_FAST: &str = "dreamina-seedance-2-0-fast-260128";
pub const V2_0_MINI: &str = "dreamina-seedance-2-0-mini-260615";
const API: &str = "https://ark.ap-southeast.bytepluses.com/api/v3/contents/generations/tasks";
const MIB: usize = 1024 * 1024;
const MP4: &str = "video/mp4";

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The prompt, in characters. BytePlus sets no hard cap; it recommends far fewer.
    pub prompt_chars: usize,
    /// Each image must be under this (BytePlus takes under 30 MB).
    pub image_bytes: usize,
    /// All images together, before encoding: base64 grows them by a third, and the request body
    /// must stay under 64 MB.
    pub total_image_bytes: usize,
    /// JSON replies (the task receipt and status).
    pub response_bytes: usize,
    /// The video.
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { prompt_chars: 8_000, image_bytes: 30_000_000, total_image_bytes: 45 * MIB, response_bytes: MIB, output_bytes: 512 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.image_bytes > h.image_bytes
            || self.total_image_bytes > h.total_image_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
        {
            return Err(invalid("Seedance limits may only be lowered"));
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
    let message = if paid { "Seedance transport failed; the task may have been created and billed once it succeeds" } else { "Seedance transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}
/// A task id as ModelArk issues them (`cgt-20250101…-abcd`); anything else is refused before it
/// reaches a URL.
fn task_id(s: &str, key: &Secret) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') || s.contains(key.expose()) {
        return Err(malformed("Seedance task id refused"));
    }
    Ok(())
}

/// Fixed-origin task adapter. Constructing it reads no credential and sends nothing.
pub struct Seedance {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Seedance {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    async fn send(&self, request: HttpRequest, paid: bool) -> Result<HttpResponse, ProviderError> {
        let request = request
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("Authorization", format!("Bearer {}", self.key.expose()))
            .max_response_bytes(self.limits.response_bytes);
        self.http.send_no_redirect(request).await.map_err(|e| transport(e, paid))
    }
    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let body = request::prepare(job, self.limits)?;
        let create = HttpRequest::post_json(API, &body)?.timeout_ms(120_000);
        drop(body);
        // Everything local is done; from here a lost reply may hide a created task that is billed
        // when it succeeds, unless ModelArk answers with a documented refusal.
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.send(create, true).await?;
        if let Some(context) = context
            && reply.body.len() <= self.limits.response_bytes
            && response::refused_create(&reply)
        {
            context.refused();
        }
        response::check(&reply, self.limits, &self.key, true)?;
        let id = response::created(&reply.body, &self.key)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::json!({"v": 1, "model": job.model}) }))
    }
    fn handle(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        task_id(&handle.remote_id, &self.key)?;
        let model = handle.data.get("model").and_then(serde_json::Value::as_str);
        if handle.data.get("v").and_then(serde_json::Value::as_u64) != Some(1) || !model.is_some_and(|m| request::model(m).is_some()) {
            return Err(invalid("Seedance handle record refused"));
        }
        Ok(())
    }
    async fn task(&self, handle: &JobHandle) -> Result<response::Status, ProviderError> {
        self.handle(handle)?;
        let reply = self.send(HttpRequest::get(format!("{API}/{}", handle.remote_id)).timeout_ms(30_000), false).await?;
        response::check(&reply, self.limits, &self.key, false)?;
        response::status(&reply.body, &handle.remote_id, &self.key)
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        match self.task(handle).await? {
            response::Status::Done { url, tokens } => {
                let bytes = self.download(&url).await?;
                let usage = Usage { output_tokens: tokens, ..Usage::default() };
                Ok(JobProgress::Done(Completed { outputs: vec![Output::Bytes { mime: MP4.into(), bytes }], usage }))
            }
            response::Status::Progress(progress) => Ok(progress),
        }
    }
    /// The result link is pre-signed on BytePlus storage: fetched without the key.
    async fn download(&self, url: &str) -> Result<Arc<[u8]>, ProviderError> {
        let request = HttpRequest::get(url).header("User-Agent", crate::USER_AGENT).max_response_bytes(self.limits.output_bytes).timeout_ms(300_000);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, false))?;
        if !reply.is_success() {
            // Retryable whatever the status: the task record keeps the link for 24 hours, and the
            // next poll reads it again, so a billed video is never given up on one failed fetch.
            let mut error = ProviderError::new(ErrorKind::Server, format!("the Seedance video download failed (HTTP {}); the video was billed", reply.status));
            error.retry_after_ms = ProviderError::from_status(reply.status, b"").retry_after_ms;
            return Err(error);
        }
        response::video(reply.body, self.limits)
    }
    /// Only a queued task can be cancelled. Deleting a finished task erases its record, and with
    /// it a billed video's link, so the task is read first and only a queued one is deleted.
    async fn stop(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        match self.task(handle).await? {
            response::Status::Progress(JobProgress::Queued { .. }) => {}
            response::Status::Progress(JobProgress::Failed(_)) => return Ok(()),
            response::Status::Done { .. } => return Err(invalid("the Seedance video is already made and billed; there is nothing to cancel")),
            response::Status::Progress(_) => {
                return Err(invalid("Seedance can't cancel a running task; it runs to the end and is billed only if it succeeds"));
            }
        }
        let delete = HttpRequest { method: crate::HttpMethod::Delete, ..HttpRequest::get(format!("{API}/{}", handle.remote_id)) }.timeout_ms(30_000);
        let reply = self.send(delete, false).await?;
        response::check(&reply, self.limits, &self.key, false)
    }
}
impl Provider for Seedance {
    fn id(&self) -> &str {
        "seedance"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(j, None))
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; create marks the paid request before it sends it.
        context.arm();
        Box::pin(self.create(j, Some(context)))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(h))
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(self.stop(h))
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        5_000
    }
}
