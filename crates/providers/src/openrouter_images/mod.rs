//! OpenRouter's Images API (`POST /api/v1/images`), checked against OpenRouter's image generation
//! guide on 2026-11-06: one synchronous request per job, the image inline as base64, the charge in
//! `usage.cost` (USD). A failed or cancelled generation answers 502 and is not billed. There is no
//! idempotency key, so an uncertain submit is never retried automatically.
//!
//! Which models exist, and which settings each takes, is read at run time from OpenRouter's model
//! list (the engine's live catalog), so this adapter accepts any model id OpenRouter could name
//! and passes `resolution`, `aspect_ratio` and `seed` through after checking their shape. The
//! catalog only offers the values the model's own listing names.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission};
use std::sync::Arc;

/// The adapter id, which catalog models name as their provider.
pub const ID: &str = "openrouter-images";
const API: &str = "https://openrouter.ai/api/v1/images";
const MIB: usize = 1024 * 1024;

/// Operation limits. They can only be lowered (`with_limits` refuses an increase).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Longest prompt, in bytes and in characters.
    pub prompt_bytes: usize,
    pub prompt_chars: usize,
    /// Most reference images, the largest one, and all of them together.
    pub references: usize,
    pub input_bytes: usize,
    pub total_input_bytes: usize,
    /// The request body, the response body, and one decoded output image.
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub output_bytes: usize,
    /// Largest output side and pixel count, and the decoder's allocation cap.
    pub axis: u32,
    pub pixels: u64,
    pub codec_bytes: u64,
}

impl Limits {
    pub const HARD: Self = Self {
        prompt_bytes: 128_000,
        prompt_chars: 32_000,
        references: 16,
        input_bytes: 8 * MIB,
        total_input_bytes: 48 * MIB,
        request_bytes: 66 * MIB,
        // One 4K image inline as base64, with room for its JSON.
        response_bytes: 48 * MIB,
        output_bytes: 32 * MIB,
        axis: 8192,
        pixels: 8192 * 8192,
        codec_bytes: 512 * MIB as u64,
    };

    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_bytes > h.prompt_bytes
            || self.prompt_chars > h.prompt_chars
            || self.references > h.references
            || self.input_bytes > h.input_bytes
            || self.total_input_bytes > h.total_input_bytes
            || self.request_bytes > h.request_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
            || self.axis > h.axis
            || self.pixels > h.pixels
            || self.codec_bytes > h.codec_bytes
        {
            return Err(invalid("OpenRouter image limits may only be lowered"));
        }
        Ok(self)
    }

    fn image(self, input: bool) -> nodecraft_media::Limits {
        nodecraft_media::Limits {
            max_input_bytes: if input { self.input_bytes } else { self.output_bytes } as u64,
            max_width: self.axis,
            max_height: self.axis,
            max_alloc: self.codec_bytes,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}

fn malformed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}

/// OpenRouter's image models. Constructing it never reads credentials or calls HTTP.
pub struct OpenRouterImages {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}

impl OpenRouterImages {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }

    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }

    async fn perform(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Completed, ProviderError> {
        let body = request::body(job, self.limits)?;
        let request = request::http(API, body, &self.key, self.limits);
        // Everything above is local: nothing was sent. From here a lost reply may have been billed.
        if let Some(context) = context {
            context.unknown();
        }
        let reply =
            self.http.send_no_redirect(request).await.map_err(|error| {
                ProviderError::new(error.kind, format!("OpenRouter image request failed; it may still have been billed: {}", error.message))
            })?;
        response::parse(&reply, self.limits)
    }
}

impl Provider for OpenRouterImages {
    fn id(&self) -> &str {
        ID
    }

    fn retry_ambiguous_submit(&self) -> bool {
        false
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(job, None).await.map(Submission::Finished) })
    }

    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.perform(job, Some(context)).await.map(Submission::Finished) })
    }

    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "OpenRouter image requests finish at submit; there is nothing to poll")) })
    }

    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("OpenRouter can't cancel an image request once it is sent")) })
    }
}
