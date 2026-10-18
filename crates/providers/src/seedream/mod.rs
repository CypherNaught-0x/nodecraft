//! Dated Seedream generation and ordered-reference editing; no automatic paid replay.
//! Codec-private/transport/allocator memory is additional to the logical phase ledger.
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission};
use std::sync::Arc;
pub const MODEL: &str = "seedream-4-5-251128";
const API: &str = "https://ark.ap-southeast.bytepluses.com/api/v3/images/generations";
const MIB: usize = 1024 * 1024;
/// Lower-only operation payload/pixel limits, not an aggregate codec or process RSS guarantee.
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
    pub axis: u32,
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
        response_bytes: 13 * MIB,
        output_bytes: 8 * MIB,
        axis: 16384,
        pixels: 16777216,
        total_pixels: 32000000,
        codec_bytes: 128 * MIB as u64,
    };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_bytes > h.prompt_bytes
            || self.prompt_chars > h.prompt_chars
            || self.metadata_bytes > h.metadata_bytes
            || self.input_bytes > h.input_bytes
            || self.total_input_bytes > h.total_input_bytes
            || self.request_bytes > h.request_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
            || self.axis > h.axis
            || self.pixels > h.pixels
            || self.total_pixels > h.total_pixels
            || self.codec_bytes > h.codec_bytes
        {
            return Err(invalid("Seedream limits may only be lowered"));
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
fn invalid(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), ProviderError> {
    v.try_reserve_exact(n).map_err(|_| malformed("Seedream bounded allocation failed"))
}
fn geometry(w: u32, h: u32, l: Limits, input: bool) -> Result<u64, ProviderError> {
    let pixels = u64::from(w).checked_mul(u64::from(h)).ok_or_else(|| invalid("Seedream geometry overflow"))?;
    if w == 0
        || h == 0
        || w > l.axis
        || h > l.axis
        || pixels > l.pixels
        || u64::from(w) > 16 * u64::from(h)
        || u64::from(h) > 16 * u64::from(w)
        || (input && (w <= 14 || h <= 14))
    {
        return Err(invalid("Seedream image geometry exceeds supported bounds"));
    }
    Ok(pixels)
}
/// Fixed-origin synchronous adapter. Constructing a provider never reads credentials or calls HTTP.
pub struct Seedream {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Seedream {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    async fn perform(&self, job: &ProviderJob) -> Result<Completed, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let req = prepared.encode(&self.key, self.limits)?;
        let reply = self
            .http
            .send_no_redirect(req)
            .await
            .map_err(|e| ProviderError::new(e.kind, "Seedream transport failed; processing or billing may have occurred"))?;
        response::parse(reply, self.limits)
    }
}
impl Provider for Seedream {
    fn id(&self) -> &str {
        "seedream"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(j).await.map(Submission::Finished) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "Seedream synchronous requests have no resumable handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Seedream cannot confirm cancellation of a sent synchronous request")) })
    }
}
#[cfg(test)]
thread_local! {static COUNTERS:std::cell::Cell<[usize;4]>=const{std::cell::Cell::new([0;4])};}
fn observe(_n: usize) {
    #[cfg(test)]
    COUNTERS.with(|c| {
        let mut v = c.get();
        if let Some(n) = v.get_mut(_n) {
            *n += 1;
        }
        c.set(v);
    });
}

#[cfg(all(test, feature = "http", not(target_arch = "wasm32")))]
mod native_tests;
