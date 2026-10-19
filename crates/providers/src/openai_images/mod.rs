//! Direct Images API, validated against official OpenAI documentation on 2026-10-18.
//! No server idempotency, remote cancellation, numeric quote or builtin browser execution is promised.
//! See `docs/providers.md` for the intentionally bounded capability and accounting contract.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission};
use std::sync::Arc;

pub const SUNBURST: &str = "gpt-image-2.5-sunburst-2026-09-08";
pub const FLARE: &str = "gpt-image-2.5-flare-2026-09-08";
const ORIGIN: &str = "https://api.openai.com/v1";
const MIB: usize = 1024 * 1024;

/// Operation-owned extents, not allocator RSS, transport internals or host concurrency limits.
/// Public fields allow smaller fixture/host limits; `with_limits` rejects every increase.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub prompt_bytes: usize,
    pub prompt_chars: usize,
    pub metadata_bytes: usize,
    pub input_bytes: usize,
    pub total_input_bytes: usize,
    pub request_bytes: usize,
    pub output_bytes: usize,
    pub total_output_bytes: usize,
    pub response_bytes: usize,
    pub width: u32,
    pub height: u32,
    pub pixels: u64,
    pub total_pixels: u64,
    pub codec_bytes: u64,
}
impl Limits {
    pub const HARD: Self = Self {
        prompt_bytes: 128_000,
        prompt_chars: 32_000,
        metadata_bytes: 256 * 1024,
        input_bytes: 8 * MIB,
        total_input_bytes: 48 * MIB,
        request_bytes: 64 * MIB,
        output_bytes: 8 * MIB,
        total_output_bytes: 48 * MIB,
        response_bytes: 65 * MIB,
        width: 4096,
        height: 4096,
        pixels: 16_777_216,
        total_pixels: 32_000_000,
        codec_bytes: 128 * MIB as u64,
    };
    fn validate(&self) -> Result<(), ProviderError> {
        let h = Self::HARD;
        if self.prompt_bytes > h.prompt_bytes
            || self.prompt_chars > h.prompt_chars
            || self.metadata_bytes > h.metadata_bytes
            || self.input_bytes > h.input_bytes
            || self.total_input_bytes > h.total_input_bytes
            || self.request_bytes > h.request_bytes
            || self.output_bytes > h.output_bytes
            || self.total_output_bytes > h.total_output_bytes
            || self.response_bytes > h.response_bytes
            || self.width > h.width
            || self.height > h.height
            || self.pixels > h.pixels
            || self.total_pixels > h.total_pixels
            || self.codec_bytes > h.codec_bytes
        {
            return Err(invalid("image limits may only be lowered"));
        }
        Ok(())
    }
    fn image(&self, input: bool) -> nodecraft_media::Limits {
        nodecraft_media::Limits {
            max_input_bytes: if input { self.input_bytes as u64 } else { self.output_bytes as u64 },
            max_width: self.width,
            max_height: self.height,
            max_alloc: self.codec_bytes,
        }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}
fn reserve<T>(v: &mut Vec<T>, count: usize) -> Result<(), ProviderError> {
    v.try_reserve_exact(count).map_err(|_| malformed("image buffer could not be allocated"))
}
fn pixels(meta: &nodecraft_media::ImageMeta, limits: &Limits) -> Result<u64, ProviderError> {
    let n = u64::from(meta.width).checked_mul(u64::from(meta.height)).ok_or_else(|| invalid("image dimensions overflow"))?;
    if meta.width == 0 || meta.height == 0 || meta.width > limits.width || meta.height > limits.height || n > limits.pixels {
        return Err(invalid("image dimensions exceed the import limits"));
    }
    Ok(n)
}

pub struct OpenAiImages {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl OpenAiImages {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        limits.validate()?;
        Ok(Self { key, http, limits })
    }
    async fn perform(&self, job: &ProviderJob) -> Result<Completed, ProviderError> {
        let prepared = request::prepare(job, &self.limits)?;
        let request = prepared.encode(&self.key, &self.limits)?;
        let reply = self.http.send_no_redirect(request).await?;
        response::parse(reply, prepared.n, prepared.format, prepared.background, &self.limits)
    }
}
impl Provider for OpenAiImages {
    fn id(&self) -> &str {
        "openai-images"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // Deliberately retain Provider's conservative legacy delivery entry: invocation taints
        // before constructing this future. The new transport operation is not a no-send proof.
        Box::pin(async move { self.perform(job).await.map(Submission::Finished) })
    }
    fn poll<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "image requests finish at submit; no resumable handle exists")) })
    }
    fn cancel<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("the Images API cannot confirm cancellation of a sent synchronous request")) })
    }
}

#[cfg(test)]
thread_local! { static COUNTERS: std::cell::Cell<(usize, usize, usize, usize)> = const { std::cell::Cell::new((0,0,0,0)) }; }
fn observe(_stage: u8) {
    #[cfg(test)]
    COUNTERS.with(|c| {
        let (a, b, d, e) = c.get();
        c.set(match _stage {
            0 => (a + 1, b, d, e),
            1 => (a, b + 1, d, e),
            2 => (a, b, d + 1, e),
            _ => (a, b, d, e + 1),
        });
    });
}
