//! Gemini image models on Google Cloud (#47): text, and up to fourteen reference images, to
//! images, through Vertex AI's `generateContent` on the global endpoint (the Gemini 3 image models
//! are served there only). One synchronous request each, the images inline in the reply. Signed
//! in with the same service account as Veo and Cloud TTS. No automatic paid replay. See
//! docs/providers.md.
mod request;
mod response;
#[cfg(test)]
mod tests;
use super::{ServiceAccount, Tokens};
use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use std::sync::Arc;

/// Gemini 3.1 Flash Image (Nano Banana 2).
pub const FLASH: &str = "gemini-3.1-flash-image";
/// Gemini 3 Pro Image (Nano Banana Pro).
pub const PRO: &str = "gemini-3-pro-image";
/// Gemini 3.1 Flash-Lite Image: 1K only.
pub const FLASH_LITE: &str = "gemini-3.1-flash-lite-image";
/// Gemini Nano Banana 2.1.
pub const NANO_BANANA_2_1: &str = "gemini-nano-banana-2.1";
/// Gemini 2.5 Flash Image: deprecated by Google, retiring 2027-03-15.
pub const FLASH_2_5: &str = "gemini-2.5-flash-image";
const MIB: usize = 1024 * 1024;

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The prompt, in characters.
    pub prompt_chars: usize,
    /// Each reference image. Google takes inline files up to 7 MB.
    pub image_bytes: usize,
    /// All reference images together, before encoding.
    pub total_image_bytes: usize,
    /// The JSON reply, which carries the images as base64.
    pub response_bytes: usize,
    /// Each image the reply carries, decoded.
    pub output_bytes: usize,
    /// All images the reply carries, decoded.
    pub total_output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self {
        prompt_chars: 32_000,
        image_bytes: 7 * 1_000_000,
        total_image_bytes: 48 * MIB,
        response_bytes: 96 * MIB,
        output_bytes: 32 * MIB,
        total_output_bytes: 64 * MIB,
    };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.image_bytes > h.image_bytes
            || self.total_image_bytes > h.total_image_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
            || self.total_output_bytes > h.total_output_bytes
        {
            return Err(invalid("Gemini image limits may only be lowered"));
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

/// Fixed-origin adapter. Constructing it reads the stored account and sends nothing.
pub struct GoogleGeminiImages {
    tokens: Tokens,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl GoogleGeminiImages {
    /// The adapter for the service account stored as `secret` (see [`super::compact`]).
    pub fn new(secret: &Secret, http: Arc<dyn Http>, clock: super::Clock) -> Result<Self, ProviderError> {
        Ok(Self { tokens: Tokens::new(ServiceAccount::from_secret(secret)?, http.clone(), clock), http, limits: Limits::HARD })
    }
    pub fn with_limits(secret: &Secret, http: Arc<dyn Http>, clock: super::Clock, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { limits: limits.validate()?, ..Self::new(secret, http, clock)? })
    }
    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let body = request::prepare(job, self.limits)?;
        let project = self.tokens.account().project();
        let url = format!("https://aiplatform.googleapis.com/v1/projects/{project}/locations/global/publishers/google/models/{}:generateContent", job.model);
        // The token exchange is free; a refused one leaves nothing sent.
        let bearer = self.tokens.bearer().await?;
        let req = crate::HttpRequest::post_json(&url, &body)?
            .timeout_ms(600_000)
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("Authorization", format!("Bearer {bearer}"))
            .max_response_bytes(self.limits.response_bytes);
        // Everything local is done; from here a lost reply may hide billed images, unless Google
        // answers with a refusal it documents as made before generation.
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(req).await.map_err(|e| ProviderError {
            kind: e.kind,
            message: "Google Cloud transport failed; the image may have been made and billed".into(),
            retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000),
        })?;
        if reply.status == 401 {
            self.tokens.forget();
        }
        if let Some(context) = context
            && response::refused(&reply)
        {
            context.refused();
        }
        response::parse(&reply, self.limits, project, &bearer)
    }
}
impl Provider for GoogleGeminiImages {
    fn id(&self) -> &str {
        "google-gemini-images"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(j, None).await.map(Submission::Finished) })
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.perform(j, Some(context)).await.map(Submission::Finished) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "Gemini image requests are synchronous and have no resumable handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Gemini image requests can't be cancelled once sent")) })
    }
}
