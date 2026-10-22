//! Cloud Text-to-Speech on Google Cloud (#21): the classic voice families (Chirp 3: HD, Neural2,
//! WaveNet, Standard and Studio), priced by the characters sent, and the Gemini-TTS models, which
//! take a style prompt and up to two named speakers. One synchronous `text:synthesize` request
//! each, the MP3 inline in the reply. No automatic paid replay. See docs/providers.md.
mod request;
mod response;
#[cfg(test)]
mod tests;
use super::{ServiceAccount, Tokens};
use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use std::sync::Arc;

/// The classic families, by the name segment their voices carry (`en-US-Neural2-F`). Each is a
/// model of its own because each has its own price per character.
pub const CHIRP3_HD: &str = "chirp3-hd";
pub const NEURAL2: &str = "neural2";
pub const WAVENET: &str = "wavenet";
pub const STANDARD: &str = "standard";
pub const STUDIO: &str = "studio";
/// The Gemini-TTS models, by the model name the API takes.
pub const GEMINI_FLASH: &str = "gemini-2.5-flash-tts";
pub const GEMINI_FLASH_LITE: &str = "gemini-2.5-flash-lite-preview-tts";
pub const GEMINI_PRO: &str = "gemini-2.5-pro-tts";
pub const GEMINI_3_1_FLASH: &str = "gemini-3.1-flash-tts-preview";
const URL: &str = "https://texttospeech.googleapis.com/v1/text:synthesize";
const MIB: usize = 1024 * 1024;
const MP3: &str = "audio/mpeg";

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The JSON reply, which carries the audio as base64.
    pub response_bytes: usize,
    /// The audio. Gemini-TTS stops near 655 seconds, about 2.6 MB of 32 kbps MP3.
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { response_bytes: 24 * MIB, output_bytes: 16 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.response_bytes > h.response_bytes || self.output_bytes > h.output_bytes {
            return Err(invalid("Google TTS limits may only be lowered"));
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
pub struct GoogleTts {
    tokens: Tokens,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl GoogleTts {
    /// The adapter for the service account stored as `secret` (see [`super::compact`]).
    pub fn new(secret: &Secret, http: Arc<dyn Http>, clock: super::Clock) -> Result<Self, ProviderError> {
        Ok(Self { tokens: Tokens::new(ServiceAccount::from_secret(secret)?, http.clone(), clock), http, limits: Limits::HARD })
    }
    pub fn with_limits(secret: &Secret, http: Arc<dyn Http>, clock: super::Clock, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { limits: limits.validate()?, ..Self::new(secret, http, clock)? })
    }
    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let prepared = request::prepare(job)?;
        let project = self.tokens.account().project();
        // The token exchange is free; a refused one leaves nothing sent.
        let bearer = self.tokens.bearer().await?;
        let req = crate::HttpRequest::post_json(URL, &prepared.body)?
            .timeout_ms(300_000)
            .header("User-Agent", crate::USER_AGENT)
            .header("x-goog-user-project", project)
            .secret_header("Authorization", format!("Bearer {bearer}"))
            .max_response_bytes(self.limits.response_bytes);
        // Everything local is done; from here a lost reply may hide billed speech, unless Google
        // answers with a refusal it documents as made before synthesis.
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(req).await.map_err(|e| ProviderError {
            kind: e.kind,
            message: "Google Cloud transport failed; the speech may have been made and billed".into(),
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
        response::parse(&reply, self.limits, project, &bearer, prepared.characters)
    }
}
impl Provider for GoogleTts {
    fn id(&self) -> &str {
        "google-tts"
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
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "Google TTS synchronous requests have no resumable handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Google TTS cannot cancel a sent synchronous request")) })
    }
}
