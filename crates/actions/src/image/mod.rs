//! Original bounded SDR CPU image actions. See docs/image-actions.md for the normative contract.
mod blur;
mod color;
mod decode;
mod executor;
mod geometry;
pub mod params;
mod prepared;
mod raster;
pub use prepared::{PreparedAxes, PreparedImageOperation, PreparedLayout, PreparedMatcher};
mod validate;
use color::Pixel;
pub use executor::CpuExecutor;
pub use params::ImageRequest;
pub(crate) use raster::buffer;
pub use raster::{Raster, RasterView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    InvalidLimits,
    RequestLimit,
    RequestSyntax,
    Parameter(&'static str),
    BufferShape,
    Dimensions,
    InputLimit,
    OutputLimit,
    ScratchLimit,
    WorkLimit,
    Allocation,
    InputRoles,
    UnsupportedColorModel,
    UnsupportedFormat,
    DecodeLimit,
    Decode,
    Executor,
    Cancelled,
}
impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parameter(id) => write!(f, "invalid image parameter: {id}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "invalid image resource limits",
                Self::RequestLimit => "image request exceeds 65536 bytes",
                Self::RequestSyntax => "image request does not match the typed parameter contract",
                Self::BufferShape => "image pixels do not match dimensions",
                Self::Dimensions => "image dimensions exceed limits",
                Self::InputLimit => "image input budget exceeded",
                Self::OutputLimit => "image output budget exceeded",
                Self::ScratchLimit => "image working-memory budget exceeded",
                Self::WorkLimit => "image work budget exceeded; reduce dimensions or effect strength",
                Self::Allocation => "image buffer allocation failed",
                Self::InputRoles => "image input roles do not match the operation",
                Self::UnsupportedColorModel => "unsupported image color model",
                Self::UnsupportedFormat => "unsupported encoded image format",
                Self::DecodeLimit => "image decoder resource budget exceeded; reduce input dimensions",
                Self::Decode => "image could not be decoded",
                Self::Executor => "image worker pool could not be created",
                Self::Cancelled => "image operation was cancelled",
                Self::Parameter(_) => "invalid image parameter",
            }),
        }
    }
}
impl std::error::Error for ImageError {}
/// Per-operation ceilings. Defaults are also hard maxima; zero can disable a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    pub edge: u32,
    pub pixels: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub working_bytes: u64,
    pub work: u64,
    pub outputs: u32,
    pub encoded_bytes: u64,
}
impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            edge: 16384,
            pixels: 16_777_216,
            input_bytes: 128 * 1024 * 1024,
            output_bytes: 256 * 1024 * 1024,
            working_bytes: 256 * 1024 * 1024,
            work: 1_000_000_000,
            outputs: 256,
            encoded_bytes: 64 * 1024 * 1024,
        }
    }
}
impl ImageLimits {
    pub fn validate(self) -> Result<(), ImageError> {
        let h = Self::default();
        if self.edge > h.edge
            || self.pixels > h.pixels
            || self.input_bytes > h.input_bytes
            || self.output_bytes > h.output_bytes
            || self.working_bytes > h.working_bytes
            || self.work > h.work
            || self.outputs > h.outputs
            || self.encoded_bytes > h.encoded_bytes
        {
            Err(ImageError::InvalidLimits)
        } else {
            Ok(())
        }
    }
    pub(super) fn image_bytes(self, w: u32, h: u32) -> Result<u64, ImageError> {
        let p = u64::from(w) * u64::from(h);
        if w == 0 || h == 0 || w > self.edge || h > self.edge || p > self.pixels {
            return Err(ImageError::Dimensions);
        }
        p.checked_mul(4).ok_or(ImageError::Dimensions)
    }
}
#[derive(Clone, Copy)]
pub struct ImageInputs<'a> {
    pub source: &'a Raster,
    pub overlay: Option<&'a Raster>,
}
impl<'a> ImageInputs<'a> {
    pub fn single(source: &'a Raster) -> Self {
        Self { source, overlay: None }
    }
}
#[derive(Debug, Clone)]
pub enum ImageResult {
    Image(Raster),
    Images(Vec<Raster>),
}
impl ImageRequest {
    pub fn execute(&self, inputs: ImageInputs<'_>, executor: &mut CpuExecutor, limits: ImageLimits) -> Result<ImageResult, ImageError> {
        executor.check_cancelled()?;
        let plan = validate::plan(self, inputs, limits)?;
        executor.admit_work(validate::cost(self, &plan, plan.bytes)?.0)?;
        if geometry::identity(self, inputs.source, &plan) {
            return Ok(ImageResult::Image(inputs.source.clone()));
        }
        if let Self::Duplicate { count } = self {
            let mut copies = Vec::new();
            copies.try_reserve_exact(*count as usize).map_err(|_| ImageError::Allocation)?;
            for _ in 0..*count {
                executor.check_cancelled()?;
                copies.push(inputs.source.clone())
            }
            return Ok(ImageResult::Images(copies));
        }
        let image = match self {
            Self::Blur { options } => blur::apply(inputs.source, options, executor, limits)?,
            Self::Aspect { options } => geometry::aspect(inputs.source, *options, &plan, executor, limits)?,
            _ => {
                let mut out = raster::buffer(usize::try_from(plan.bytes).map_err(|_| ImageError::OutputLimit)?)?;
                executor.pixels(&mut out, plan.width, |x, y| {
                    let source = || geometry::pixel(inputs.source, x, y).straight();
                    let v = match self {
                        Self::Grade { options } => color::grade(source(), *options),
                        Self::Filter { options } => color::filter(source(), options),
                        Self::Tint { options } => color::tint(source(), *options),
                        Self::Key { options } => {
                            let p = source();
                            let m = color::mask(p, options.matcher);
                            Pixel {
                                a: p.a
                                    * match options.mode {
                                        params::KeyMode::RemoveMatched => 1. - m,
                                        params::KeyMode::KeepMatched => m,
                                    },
                                ..p
                            }
                        }
                        Self::ReplaceColor { options } => {
                            let p = source();
                            let m = color::mask(p, options.matcher);
                            let q = Pixel::from_color(options.replacement);
                            Pixel { a: if options.replace_alpha { p.a * (1. - m) + q.a * m } else { p.a }, ..p.mix(q, m) }
                        }
                        _ => {
                            return geometry::exact(self, inputs.source, &plan, x, y)
                                .unwrap_or_else(|| geometry::render(self, inputs, &plan, x, y).straight().bytes());
                        }
                    };
                    v.bytes()
                })?;
                Raster::output(plan.width, plan.height, out, limits)?
            }
        };
        Ok(ImageResult::Image(image))
    }
}

#[cfg(test)]
mod tests;

impl ImageRequest {
    /// Bound the wire envelope before serde; errors never echo raw JSON or parser messages.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ImageError> {
        if bytes.len() > 65_536 {
            return Err(ImageError::RequestLimit);
        }
        serde_json::from_slice(bytes).map_err(|_| ImageError::RequestSyntax)
    }
    /// Admit the operation before normalizing borrowed views. Original slices remain charged
    /// through copying and execution; unknown backing-owner capacities remain caller-owned.
    pub fn execute_views(
        &self,
        source: RasterView<'_>,
        overlay: Option<RasterView<'_>>,
        executor: &mut CpuExecutor,
        l: ImageLimits,
    ) -> Result<ImageResult, ImageError> {
        l.validate()?;
        validate::parameters(self)?;
        if matches!(self, Self::Overlay { .. }) != overlay.is_some() {
            return Err(ImageError::InputRoles);
        }
        let source = RasterView::new(source.width, source.height, source.pixels, l)?;
        let overlay = overlay.map(|v| RasterView::new(v.width, v.height, v.pixels, l)).transpose()?;
        let borrowed = validate::add(source.pixels.len() as u64, overlay.map_or(0, |v| v.pixels.len() as u64))?;
        if borrowed > l.input_bytes {
            return Err(ImageError::InputLimit);
        }
        let remaining = ImageLimits { working_bytes: l.working_bytes.checked_sub(borrowed).ok_or(ImageError::ScratchLimit)?, ..l };
        let plan = geometry::layout(self, source.width, source.height, overlay.map(|v| (v.width, v.height)))?;
        validate::admit(self, &plan, borrowed, remaining)?;
        let first = Raster::copy_from(source.width, source.height, source.pixels, l)?;
        let second = overlay.map(|v| Raster::copy_from(v.width, v.height, v.pixels, l)).transpose()?;
        self.execute(ImageInputs { source: &first, overlay: second.as_ref() }, executor, remaining)
    }
}

impl ImageRequest {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Grade { .. } => "image.grade",
            Self::Filter { .. } => "image.filter",
            Self::Tint { .. } => "image.tint",
            Self::Key { .. } => "image.key",
            Self::ReplaceColor { .. } => "image.replace_color",
            Self::Aspect { .. } => "image.aspect",
            Self::Rotate { .. } => "image.rotate",
            Self::Flip { .. } => "image.flip",
            Self::Blur { .. } => "image.blur",
            Self::Crop { .. } => "image.crop",
            Self::Resize { .. } => "image.resize",
            Self::Overlay { .. } => "image.overlay",
            Self::Duplicate { .. } => "image.duplicate",
        }
    }
}
