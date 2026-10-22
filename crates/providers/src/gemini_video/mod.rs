//! Gemini Omni Flash video (#17): text, a first frame (and optionally a last one) or up to three
//! subject references to a video with sound. Veo on the Gemini API was shut down on 2026-10-22, so
//! Omni Flash is Google's video model there. The one Interactions request is the paid step and
//! runs synchronously; it asks for the video by file reference, which is polled on the Files API
//! until it is ready and downloaded with the key. A reply that carries the video inline finishes
//! at once.
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

pub const OMNI_FLASH: &str = "gemini-omni-1.1-flash";
const ORIGIN: &str = "https://generativelanguage.googleapis.com";
const API: &str = "https://generativelanguage.googleapis.com/v1beta";
const MIB: usize = 1024 * 1024;
const MP4: &str = "video/mp4";

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The prompt, in characters.
    pub prompt_chars: usize,
    /// Each image, before encoding.
    pub image_bytes: usize,
    /// All images together, before encoding.
    pub total_image_bytes: usize,
    /// JSON replies, including one that carries the video inline as base64.
    pub response_bytes: usize,
    /// The video.
    pub output_bytes: usize,
    /// How long the synchronous generate request may take.
    pub request_timeout_ms: u64,
}
impl Limits {
    pub const HARD: Self = Self {
        prompt_chars: 32_000,
        image_bytes: 15 * MIB,
        total_image_bytes: 45 * MIB,
        response_bytes: 96 * MIB,
        output_bytes: 256 * MIB,
        request_timeout_ms: 900_000,
    };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.image_bytes > h.image_bytes
            || self.total_image_bytes > h.total_image_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
            || self.request_timeout_ms > h.request_timeout_ms
        {
            return Err(invalid("Gemini video limits may only be lowered"));
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
    let message = if paid { "Gemini transport failed; the video may have been generated and billed" } else { "Gemini transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}
/// A file id as it may appear in a URL path: the Files API names files `files/<id>` with a short
/// lowercase id, so only the characters a path segment carries safely are accepted.
fn file_id(s: &str, key: &Secret) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') || s.contains(key.expose()) {
        return Err(malformed("Gemini file id refused"));
    }
    Ok(())
}

/// Fixed-origin adapter. Constructing it reads no credential and sends nothing.
pub struct GeminiVideo {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl GeminiVideo {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    async fn send(&self, request: HttpRequest, paid: bool) -> Result<HttpResponse, ProviderError> {
        let request =
            request.header("User-Agent", crate::USER_AGENT).secret_header("x-goog-api-key", self.key.expose()).max_response_bytes(self.limits.response_bytes);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, paid))?;
        response::check(&reply, self.limits, paid)?;
        Ok(reply)
    }
    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let body = request::prepare(job, self.limits)?;
        let create = HttpRequest::post_json(format!("{API}/interactions"), &body)?.timeout_ms(self.limits.request_timeout_ms);
        drop(body);
        // Everything local is done; from here a lost reply may hide a generated, billed video.
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.send(create, true).await?;
        match response::created(&reply.body, self.limits, &self.key)? {
            response::Created::Inline(bytes, usage) => Ok(Submission::Finished(Completed { outputs: vec![Output::Bytes { mime: MP4.into(), bytes }], usage })),
            response::Created::File(id, usage) => Ok(Submission::Queued(JobHandle { remote_id: id, data: handle_data(usage) })),
        }
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        file_id(&handle.remote_id, &self.key)?;
        let usage = handle_usage(&handle.data)?;
        let request = HttpRequest::get(format!("{API}/files/{}", handle.remote_id)).timeout_ms(30_000);
        let reply = self.send(request, false).await?;
        match response::file_state(&reply.body)? {
            response::FileState::Active => {
                drop(reply);
                let bytes = self.download(&handle.remote_id).await?;
                Ok(JobProgress::Done(Completed { outputs: vec![Output::Bytes { mime: MP4.into(), bytes }], usage }))
            }
            response::FileState::Processing => {
                Ok(JobProgress::Running { fraction: None, message: Some("Gemini is preparing the video".into()), partial: None })
            }
            response::FileState::Failed => {
                Ok(JobProgress::Failed(ProviderError::new(ErrorKind::JobFailed, "Gemini couldn't prepare the video file; it was billed")))
            }
        }
    }
    /// The file's bytes, with the key. Google may redirect the download to its storage; the
    /// transport follows that and drops the key on leaving the origin.
    async fn download(&self, id: &str) -> Result<Arc<[u8]>, ProviderError> {
        let request = HttpRequest::get(format!("{API}/files/{id}:download?alt=media"))
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("x-goog-api-key", self.key.expose())
            .max_response_bytes(self.limits.output_bytes)
            .timeout_ms(300_000);
        let reply = self.http.send(request).await.map_err(|e| match e.kind {
            ErrorKind::Malformed => malformed("the Gemini video download was refused (over the size limit or a bad redirect); it was billed"),
            _ => transport(e, false),
        })?;
        if !reply.is_success() {
            // Otherwise retryable: the file stays for two days, so a billed video is never given
            // up on one failed download.
            let (kind, why) = match reply.status {
                401 | 403 => (ErrorKind::Auth, "the key was refused"),
                404 | 410 => (ErrorKind::NotFound, "the file is gone; Google keeps files for two days"),
                _ => (ErrorKind::Server, "the video was billed"),
            };
            return Err(ProviderError::new(kind, format!("the Gemini video download failed (HTTP {}); {why}", reply.status)));
        }
        response::video(reply.body, self.limits)
    }
}
/// The handle record: a version and the generate request's token counts, so the finished job
/// reports what it used.
fn handle_data(usage: Usage) -> serde_json::Value {
    serde_json::json!({"v": 1, "input_tokens": usage.input_tokens, "output_tokens": usage.output_tokens})
}
fn handle_usage(data: &serde_json::Value) -> Result<Usage, ProviderError> {
    if data.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(invalid("Gemini video handle record refused"));
    }
    let count = |name: &str| data.get(name).and_then(serde_json::Value::as_u64);
    Ok(Usage { input_tokens: count("input_tokens"), output_tokens: count("output_tokens"), ..Usage::default() })
}
impl Provider for GeminiVideo {
    fn id(&self) -> &str {
        "gemini-video"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(j, None))
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; create marks uncertainty before the paid request.
        context.arm();
        Box::pin(self.create(j, Some(context)))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(h))
    }
    /// The video exists once the generate request returns; there is nothing left to cancel.
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("a Gemini video is generated and billed before it can be cancelled")) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        5_000
    }
}
