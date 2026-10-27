use super::*;

/// Client payload and codec bounds, not promises about the service or process RSS.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub input_bytes: usize,
    pub request_bytes: usize,
    pub response_bytes: usize,
    /// One result only: this is both the individual and aggregate download bound.
    pub output_bytes: usize,
    pub output_width: u32,
    pub output_height: u32,
    pub total_pixels: u64,
    pub codec_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
impl Limits {
    pub const HARD: Self = Self {
        input_bytes: 1024 * 1024,
        request_bytes: 2 * 1024 * 1024,
        response_bytes: 256 * 1024,
        output_bytes: 256 * 1024 * 1024,
        output_width: 4096,
        output_height: 4096,
        total_pixels: 16_777_216,
        codec_bytes: 128 * 1024 * 1024,
    };
    pub(super) fn validate(self) -> Result<Self, ProviderError> {
        let hard = Self::HARD;
        if self.input_bytes > hard.input_bytes
            || self.request_bytes > hard.request_bytes
            || self.response_bytes > hard.response_bytes
            || self.output_bytes > hard.output_bytes
            || self.output_width > hard.output_width
            || self.output_height > hard.output_height
            || self.total_pixels > hard.total_pixels
            || self.codec_bytes > hard.codec_bytes
        {
            return Err(invalid("Magnific limits may only be lowered"));
        }
        Ok(self)
    }
    pub(super) fn image(self, input: bool) -> nodecraft_media::Limits {
        nodecraft_media::Limits {
            max_input_bytes: if input { self.input_bytes } else { self.output_bytes } as u64,
            max_width: if input { 1024 } else { self.output_width },
            max_height: if input { 1024 } else { self.output_height },
            max_alloc: self.codec_bytes,
        }
    }
    /// A restarted adapter never relaxes either the original job or its current host's bounds.
    pub(super) fn intersect(self, other: Self) -> Self {
        Self {
            input_bytes: self.input_bytes.min(other.input_bytes),
            request_bytes: self.request_bytes.min(other.request_bytes),
            response_bytes: self.response_bytes.min(other.response_bytes),
            output_bytes: self.output_bytes.min(other.output_bytes),
            output_width: self.output_width.min(other.output_width),
            output_height: self.output_height.min(other.output_height),
            total_pixels: self.total_pixels.min(other.total_pixels),
            codec_bytes: self.codec_bytes.min(other.codec_bytes),
        }
    }
}
