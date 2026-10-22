//! Kling (#19): text to video, a start frame (and optionally an end frame) to video, up to four
//! reference images to video, and text to image, on Kling's task API. Images travel inline as
//! base64 in the one create request, which is the paid step and is never replayed automatically;
//! it carries an `external_task_id` derived from the job's idempotency key, so Kling refuses a
//! second task for the same job. Results are links on Kling's storage, downloaded at once without
//! the key.
//!
//! The key is either a Kling API key, sent as a bearer token, or an access key and secret key
//! joined by a colon, which sign a short-lived HS256 token for each request.
mod auth;
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
};
use std::sync::Arc;

const API: &str = "https://api-singapore.klingai.com";
const MIB: usize = 1024 * 1024;
const MP4: &str = "video/mp4";

/// What a Kling model makes and which of its options it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Model {
    pub(super) name: &'static str,
    pub(super) image: bool,
    /// Standard or Professional mode is a choice; models without it run in their only mode.
    pub(super) modes: bool,
    /// Takes an end frame after the start frame (in Professional mode).
    pub(super) end_frame: bool,
    /// Takes up to four reference images (Kling's multi-image to video).
    pub(super) references: bool,
    /// Takes `cfg_scale`; Kling's 2.x video models don't.
    pub(super) cfg_scale: bool,
}
pub(super) const MODELS: [Model; 4] = [
    Model { name: "kling-v2-5-turbo", image: false, modes: true, end_frame: true, references: false, cfg_scale: false },
    Model { name: "kling-v2-1-master", image: false, modes: false, end_frame: false, references: false, cfg_scale: false },
    Model { name: "kling-v1-6", image: false, modes: true, end_frame: true, references: true, cfg_scale: true },
    Model { name: "kling-v2-1", image: true, modes: false, end_frame: false, references: false, cfg_scale: false },
];
/// The task kinds, each its own create path and the path its tasks are read back from.
pub(super) const PATHS: [&str; 4] = ["videos/text2video", "videos/image2video", "videos/multi-image2video", "images/generations"];

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Kling's cap on a prompt and a negative prompt: 2,500 characters.
    pub prompt_chars: usize,
    /// Each image, before encoding: Kling takes up to 10 MB.
    pub image_bytes: usize,
    /// All of a job's images together, before encoding.
    pub total_image_bytes: usize,
    /// JSON replies (task receipts and status).
    pub response_bytes: usize,
    /// The downloaded video or image.
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { prompt_chars: 2_500, image_bytes: 10 * MIB, total_image_bytes: 40 * MIB, response_bytes: MIB, output_bytes: 512 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.image_bytes > h.image_bytes
            || self.total_image_bytes > h.total_image_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
        {
            return Err(invalid("Kling limits may only be lowered"));
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
    let message = if paid { "Kling transport failed; the task may have been created and billed" } else { "Kling transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}
/// A task id as it may appear in a URL path. Kling's ids are long decimal strings; only the
/// characters a path segment carries safely are accepted.
fn task_id(s: &str, key: &auth::Key) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') || key.appears_in(s) {
        return Err(malformed("Kling task id refused"));
    }
    Ok(())
}
/// Seconds since the Unix epoch, for the signed token; `None` when the clock is before 1970.
fn system_seconds() -> Option<u64> {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Fixed-origin task adapter. Constructing it reads no credential and sends nothing.
pub struct Kling {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
    /// Wall-clock seconds for the access-key token; a test fixes it.
    clock: fn() -> Option<u64>,
}
impl Kling {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD, clock: system_seconds }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()?, clock: system_seconds })
    }
    #[cfg(test)]
    fn with_clock(key: Secret, http: Arc<dyn Http>, clock: fn() -> Option<u64>) -> Self {
        Self { key, http, limits: Limits::HARD, clock }
    }
    /// Sends one request signed with `bearer`, worked out before any paid mark so a clock that
    /// can't sign never counts as a sent create. `refusal` is the paid create's scope: a
    /// documented refusal of it proves no task was made.
    async fn send(
        &self,
        request: HttpRequest,
        key: &auth::Key<'_>,
        bearer: &str,
        paid: bool,
        refusal: Option<&crate::SubmitContext>,
    ) -> Result<HttpResponse, ProviderError> {
        let request = request
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("Authorization", format!("Bearer {bearer}"))
            .max_response_bytes(self.limits.response_bytes);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, paid))?;
        if let Some(context) = refusal
            && response::refused_create(&reply, self.limits)
        {
            context.refused();
        }
        response::check(&reply, self.limits, key, paid)?;
        Ok(reply)
    }
    /// The task named by a handle this adapter issued; a forged one never reaches a URL.
    fn task<'h>(&self, handle: &'h JobHandle) -> Result<(&'h str, bool), ProviderError> {
        task_id(&handle.remote_id, &auth::Key::parse(&self.key)?)?;
        let path = handle.data.get("path").and_then(serde_json::Value::as_str).filter(|p| PATHS.contains(p));
        match (handle.data.get("v").and_then(serde_json::Value::as_u64), path) {
            (Some(1), Some(path)) => Ok((path, path.starts_with("images/"))),
            _ => Err(invalid("Kling handle record refused")),
        }
    }
    async fn create(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let key = auth::Key::parse(&self.key)?;
        let bearer = key.bearer(self.clock)?;
        let prepared = request::prepare(job, self.limits)?;
        let create = HttpRequest::post_json(format!("{API}/v1/{}", prepared.path), &prepared.body)?.timeout_ms(120_000);
        drop(prepared.body);
        // Everything local is done; from here a lost reply may hide a created, billed task. The
        // create is this scope's only request, so a documented refusal can clear the mark.
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.send(create, &key, &bearer, true, context).await?;
        let id = response::created(&reply.body, &key)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::json!({"v": 1, "path": prepared.path}) }))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        let (path, image) = self.task(handle)?;
        let key = auth::Key::parse(&self.key)?;
        let bearer = key.bearer(self.clock)?;
        let request = HttpRequest::get(format!("{API}/v1/{path}/{}", handle.remote_id)).timeout_ms(30_000);
        let reply = self.send(request, &key, &bearer, false, None).await?;
        match response::status(&reply.body, &handle.remote_id, image, &key)? {
            response::Status::Done(url) => {
                drop(reply);
                let (mime, bytes) = self.download(&url, image).await?;
                Ok(JobProgress::Done(Completed::new(vec![Output::Bytes { mime, bytes }])))
            }
            response::Status::Progress(progress) => Ok(progress),
        }
    }
    /// The result link is on Kling's storage: fetched without the key.
    async fn download(&self, url: &str, image: bool) -> Result<(String, Arc<[u8]>), ProviderError> {
        let request = HttpRequest::get(url).header("User-Agent", crate::USER_AGENT).max_response_bytes(self.limits.output_bytes).timeout_ms(300_000);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, false))?;
        if !reply.is_success() {
            // Retryable whatever the status: the next poll reads the link again, so a billed
            // result is never given up on one failed fetch.
            let mut error = ProviderError::new(ErrorKind::Server, format!("the Kling result download failed (HTTP {}); the task was billed", reply.status));
            error.retry_after_ms = ProviderError::from_status(reply.status, b"").retry_after_ms;
            return Err(error);
        }
        response::result(reply.body, image, self.limits)
    }
}
impl Provider for Kling {
    fn id(&self) -> &str {
        "kling"
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
    /// Kling's task API has no cancel: a task runs to the end and is billed.
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Kling can't cancel a task; it runs to the end and is billed")) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        5_000
    }
}
