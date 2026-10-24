//! Veo 3.1 on Google Cloud (#742): text, a first frame (and optionally a last one) or up to three
//! subject references to a video with sound, on the Gemini Enterprise Agent Platform (formerly
//! Vertex AI) in `us-central1`. Starting the long-running operation is the paid step; the operation
//! is then fetched until it is done, and the video comes back inline in the last reply (no Cloud
//! Storage bucket is involved).
mod request;
mod response;
#[cfg(test)]
mod tests;
use super::{ServiceAccount, Tokens};
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext,
};
use std::sync::Arc;

pub const GENERATE: &str = "veo-3.1-generate-001";
pub const FAST: &str = "veo-3.1-fast-generate-001";
pub const LITE: &str = "veo-3.1-lite-generate-001";
/// The one region Google lists for Veo 3.1.
const LOCATION: &str = "us-central1";
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
    /// JSON replies, the last of which carries the video as base64.
    pub response_bytes: usize,
    /// The video.
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { prompt_chars: 8_000, image_bytes: 20 * MIB, total_image_bytes: 60 * MIB, response_bytes: 192 * MIB, output_bytes: 128 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.image_bytes > h.image_bytes
            || self.total_image_bytes > h.total_image_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
        {
            return Err(invalid("Veo limits may only be lowered"));
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
    let message = if paid { "Google Cloud transport failed; the video may have been started and billed" } else { "Google Cloud transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}

/// Fixed-origin adapter. Constructing it reads the stored account and sends nothing.
pub struct Veo {
    tokens: Tokens,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Veo {
    /// The adapter for the service account stored as `secret` (see [`super::compact`]).
    pub fn new(secret: &Secret, http: Arc<dyn Http>, clock: super::Clock) -> Result<Self, ProviderError> {
        Ok(Self { tokens: Tokens::new(ServiceAccount::from_secret(secret)?, http.clone(), clock), http, limits: Limits::HARD })
    }
    pub fn with_limits(secret: &Secret, http: Arc<dyn Http>, clock: super::Clock, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { limits: limits.validate()?, ..Self::new(secret, http, clock)? })
    }
    fn url(&self, model: &str, method: &str) -> String {
        let project = self.tokens.account().project();
        format!("https://{LOCATION}-aiplatform.googleapis.com/v1/projects/{project}/locations/{LOCATION}/publishers/google/models/{model}:{method}")
    }
    /// Sends `request` with an access token. A token the API refuses is dropped, so the next
    /// attempt asks for a fresh one.
    async fn send(&self, request: HttpRequest, bearer: &str, paid: bool) -> Result<HttpResponse, ProviderError> {
        let request = request
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("Authorization", format!("Bearer {bearer}"))
            .max_response_bytes(self.limits.response_bytes);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, paid))?;
        if reply.status == 401 {
            self.tokens.forget();
        }
        response::check(&reply, self.limits, paid, self.tokens.account().project())?;
        Ok(reply)
    }
    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let body = request::prepare(job, self.limits)?;
        let start = HttpRequest::post_json(self.url(&job.model, "predictLongRunning"), &body)?.timeout_ms(120_000);
        drop(body);
        // The token exchange is free; a refused one leaves nothing started.
        let bearer = self.tokens.bearer().await?;
        // Everything local is done; from here a lost reply may hide a started, billed video.
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.send(start, &bearer, true).await?;
        let name = response::started(&reply.body, &job.model)?;
        Ok(Submission::Queued(JobHandle { remote_id: name, data: serde_json::json!({"v": 1}) }))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        if handle.data.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid("Veo handle record refused"));
        }
        let model = response::operation_model(&handle.remote_id)?;
        let fetch =
            HttpRequest::post_json(self.url(model, "fetchPredictOperation"), &serde_json::json!({"operationName": handle.remote_id}))?.timeout_ms(300_000);
        let bearer = self.tokens.bearer().await?;
        let reply = self.send(fetch, &bearer, false).await?;
        Ok(match response::operation(&reply.body, self.limits)? {
            response::Operation::Running => JobProgress::Running { fraction: None, message: Some("Veo is generating the video".into()), partial: None },
            response::Operation::Done(bytes) => {
                JobProgress::Done(Completed { outputs: vec![Output::Bytes { mime: MP4.into(), bytes }], usage: Default::default() })
            }
            response::Operation::Failed(error) => JobProgress::Failed(error),
        })
    }
}
impl Provider for Veo {
    fn id(&self) -> &str {
        "google-veo"
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
    /// Google documents no way to stop a Veo operation.
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("a Veo video can't be cancelled once it has started; it is billed")) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        10_000
    }
}
