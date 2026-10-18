//! Bounded, stateless Gemini Interactions images. See docs/providers.md for host limitations.
mod request;
mod response;
#[cfg(test)]
mod tests;
mod wire;
use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use std::sync::Arc;
pub const NANO: &str = "gemini-nano-banana-2.1";
pub const LITE: &str = "gemini-3.1-flash-lite-image";
pub const PRO: &str = "gemini-3-pro-image";
const MIB: usize = 1024 * 1024;
const MAX_REFERENCE_IMAGES: usize = 14;
const MAX_INPUT_ITEMS: usize = 1 + MAX_REFERENCE_IMAGES;
// Largest selected documented 4K shape: 4800 × 3584 (image guide, checked 2026-10-18).
const MAX_DOCUMENTED_4K_PIXELS: u64 = 4800 * 3584;
const DEFAULT_AGGREGATE_PIXELS: u64 = 2 * MAX_DOCUMENTED_4K_PIXELS;
// Local service/body-transfer allowance, not a measured SLA or RunLimits deadline.
const REQUEST_TIMEOUT_MS: u64 = 300_000;
const ASPECTS: &[&str] = &["1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9"];
/// Local operation limits, not service maxima, allocator RSS or concurrent host limits.
/// Every field can only be lowered by with_limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub prompt_bytes: usize,
    pub prompt_chars: usize,
    pub metadata_bytes: usize,
    pub input_bytes: usize,
    pub total_input_bytes: usize,
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub output_bytes: usize,
    pub total_output_bytes: usize,
    pub encoded_bytes: usize,
    pub text_bytes: usize,
    pub steps: usize,
    pub contents: usize,
    pub images: usize,
    pub depth: usize,
    pub members: usize,
    pub values: usize,
    pub strings: usize,
    pub width: u32,
    pub height: u32,
    pub pixels: u64,
    pub total_pixels: u64,
    pub codec_bytes: u64,
}
impl Limits {
    pub const HARD: Self = Self {
        prompt_bytes: 128000,
        prompt_chars: 32000,
        metadata_bytes: 256 * 1024,
        input_bytes: 8 * MIB,
        total_input_bytes: 48 * MIB,
        request_bytes: 64 * MIB,
        response_bytes: 65 * MIB,
        output_bytes: 8 * MIB,
        total_output_bytes: 48 * MIB,
        encoded_bytes: 64 * MIB,
        text_bytes: 64 * 1024,
        steps: 64,
        contents: 256,
        images: 16,
        depth: 16,
        members: 8192,
        values: 16384,
        strings: 4096,
        width: 8192,
        height: 8192,
        pixels: MAX_DOCUMENTED_4K_PIXELS,
        total_pixels: DEFAULT_AGGREGATE_PIXELS,
        codec_bytes: 256 * MIB as u64,
    };
    fn validate(&self) -> Result<(), ProviderError> {
        let h = Self::HARD;
        macro_rules! check {($($field:ident),*)=>{if false $(||self.$field>h.$field)* {return Err(invalid("Gemini limits may only be lowered"));}}}
        check!(
            prompt_bytes,
            prompt_chars,
            metadata_bytes,
            input_bytes,
            total_input_bytes,
            request_bytes,
            response_bytes,
            output_bytes,
            total_output_bytes,
            encoded_bytes,
            text_bytes,
            steps,
            contents,
            images,
            depth,
            members,
            values,
            strings,
            width,
            height,
            pixels,
            total_pixels,
            codec_bytes
        );
        Ok(())
    }
    fn image(&self, input: bool) -> nodecraft_media::Limits {
        nodecraft_media::Limits {
            max_input_bytes: if input { self.input_bytes } else { self.output_bytes } as u64,
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
fn invalid(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), ProviderError> {
    v.try_reserve_exact(n).map_err(|_| malformed("Gemini buffer allocation refused"))
}
fn add(total: &mut usize, n: usize, cap: usize) -> Result<(), ProviderError> {
    *total = total.checked_add(n).filter(|v| *v <= cap).ok_or_else(|| malformed("Gemini aggregate limit exceeded"))?;
    Ok(())
}
fn pixels(m: &nodecraft_media::ImageMeta, l: &Limits) -> Result<u64, ProviderError> {
    let n = u64::from(m.width).checked_mul(u64::from(m.height)).ok_or_else(|| malformed("Gemini pixel extent overflow"))?;
    if m.width == 0 || m.height == 0 || m.width > l.width || m.height > l.height || n > l.pixels {
        Err(malformed("Gemini image dimensions exceed limits"))
    } else {
        Ok(n)
    }
}
pub struct GeminiImages {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl GeminiImages {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        limits.validate()?;
        Ok(Self { key, http, limits })
    }
    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let prepared = request::prepare(job, &self.limits)?;
        let request = prepared.encode(&self.key, &self.limits)?;
        // The no-redirect trait does not provide affirmative delivery evidence. Taint BEFORE its
        // future constructor, even for transports which perform effects synchronously there.
        if let Some(c) = context {
            c.unknown();
        }
        let response = self.http.send_no_redirect(request).await?;
        response::parse(response, &self.limits)
    }
}
impl Provider for GeminiImages {
    fn id(&self) -> &str {
        "gemini-images"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(job, None).await.map(Submission::Finished) })
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.perform(job, Some(context)).await.map(Submission::Finished) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "Gemini images finish at submit; no resumable handle exists")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("a sent synchronous Gemini image request cannot confirm remote cancellation")) })
    }
}
#[cfg(test)]
thread_local! {static COPIES:std::cell::Cell<(usize,usize)>=const{std::cell::Cell::new((0,0))};}
fn observe(_body: bool) {
    #[cfg(test)]
    COPIES.with(|c| {
        let (a, b) = c.get();
        c.set(if _body { (a + 1, b) } else { (a, b + 1) });
    });
}
