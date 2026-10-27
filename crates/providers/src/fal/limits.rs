use super::*;
const MIB: usize = 1024 * 1024;
/// Lower-only operation payload bounds, not allocator/RSS or codec workspace bounds.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub input_bytes: usize,
    pub text_bytes: usize,
    pub request_bytes: usize,
    /// Most bytes of training images (#167), packed into one zip.
    pub dataset_bytes: usize,
    /// Most bytes in a training request: the zip in base64, plus the JSON.
    pub training_request_bytes: usize,
    pub response_bytes: usize,
    pub metadata_bytes: usize,
    pub output_bytes: usize,
    pub total_output_bytes: usize,
    pub output_width: u32,
    pub output_height: u32,
    pub total_pixels: u64,
    pub codec_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self::GROK_HARD
    }
}
impl Limits {
    /// Baseline output bounds for images, audio, meshes and the existing non-Ray/non-LTX models.
    pub const HARD: Self = Self {
        input_bytes: MIB,
        text_bytes: 16 * 1024,
        // Two 1 MiB inputs (a portrait and speech, or two frames) in base64, plus the JSON.
        request_bytes: 4 * MIB,
        // Twelve 1 MiB images and their zip headers.
        dataset_bytes: 12 * MIB + 64 * 1024,
        // That zip in base64, plus the JSON.
        training_request_bytes: 17 * MIB,
        response_bytes: 2 * MIB,
        metadata_bytes: 256 * 1024,
        output_bytes: 16 * MIB,
        total_output_bytes: 48 * MIB,
        output_width: 4096,
        output_height: 4096,
        total_pixels: 16_777_216,
        codec_bytes: 128 * MIB as u64,
    };
    /// Ray and LTX results are video clips. Keep their download and aggregate bounds aligned;
    /// other models are clamped back to HARD in `for_model`.
    pub const VIDEO_HARD: Self = Self { output_bytes: 256 * MIB, total_output_bytes: 256 * MIB, ..Self::HARD };
    /// Five 1 MiB edit references encoded into one request, and ten bounded image results.
    pub const GROK_HARD: Self = Self { request_bytes: 8 * MIB, total_pixels: 10 * Self::HARD.total_pixels, ..Self::VIDEO_HARD };
    pub(super) fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::GROK_HARD;
        if self.input_bytes > h.input_bytes
            || self.text_bytes > h.text_bytes
            || self.request_bytes > h.request_bytes
            || self.dataset_bytes > h.dataset_bytes
            || self.training_request_bytes > h.training_request_bytes
            || self.response_bytes > h.response_bytes
            || self.metadata_bytes > h.metadata_bytes
            || self.output_bytes > h.output_bytes
            || self.total_output_bytes > h.total_output_bytes
            || self.output_width > h.output_width
            || self.output_height > h.output_height
            || self.total_pixels > h.total_pixels
            || self.codec_bytes > h.codec_bytes
        {
            return Err(invalid("fal limits may only be lowered"));
        }
        Ok(self)
    }
    pub(super) fn for_model(self, model: Model) -> Self {
        let grok_images = matches!(model, Model::GrokImage | Model::GrokEdit);
        let large_video = matches!(model, Model::GrokText | Model::GrokFrames | Model::RayText | Model::RayFrames | Model::LtxText | Model::LtxFrames);
        Self {
            request_bytes: self.request_bytes.min(if model == Model::GrokEdit { Self::GROK_HARD.request_bytes } else { Self::HARD.request_bytes }),
            output_bytes: self.output_bytes.min(if large_video { Self::VIDEO_HARD.output_bytes } else { Self::HARD.output_bytes }),
            total_output_bytes: self.total_output_bytes.min(if grok_images {
                10 * Self::HARD.output_bytes
            } else if large_video {
                Self::VIDEO_HARD.total_output_bytes
            } else {
                Self::HARD.total_output_bytes
            }),
            total_pixels: self.total_pixels.min(if grok_images { Self::GROK_HARD.total_pixels } else { Self::HARD.total_pixels }),
            ..self
        }
    }
    pub(super) fn image(self, input: bool) -> nodecraft_media::Limits {
        nodecraft_media::Limits {
            max_input_bytes: if input { self.input_bytes } else { self.output_bytes } as u64,
            max_width: if input { 1024 } else { self.output_width },
            max_height: if input { 1024 } else { self.output_height },
            max_alloc: self.codec_bytes,
        }
    }
}
pub(super) fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), ProviderError> {
    v.try_reserve_exact(n).map_err(|_| malformed("fal allocation refused"))
}
