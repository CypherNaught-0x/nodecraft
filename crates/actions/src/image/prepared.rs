//! Shared admission and exact decisions for alternate image executors.
use super::params::*;
mod geometry;
/// Separate exact source axes, owned and immutable while borrowed by an executor.
pub type PreparedAxes = (Vec<u32>, Vec<u32>);
use super::{ImageError as E, ImageInputs, ImageLimits, ImageRequest, Raster};

/// Immutable geometry computed by the CPU reference's f64/integer layout rules.
#[derive(Debug, Clone, Copy)]
pub struct PreparedLayout {
    pub width: u32,
    pub height: u32,
    pub source_rect: [f64; 4],
    pub overlay_rect: Option<[f64; 4]>,
}
/// Borrow-bound proof: request, raster roles and layout cannot be replaced after admission.
pub struct PreparedImageOperation<'a> {
    request: &'a ImageRequest,
    inputs: ImageInputs<'a>,
    plan: super::geometry::Plan,
    limits: ImageLimits,
}
/// Original-image coverage and exact alpha decisions. No final RGB is rendered on the CPU.
pub struct PreparedMatcher {
    values: Vec<[u32; 2]>,
    visits: u64,
}
impl PreparedMatcher {
    pub fn visits(&self) -> u64 {
        self.visits
    }
    pub fn values(&self) -> &[[u32; 2]] {
        &self.values
    }
    pub fn capacity_bytes(&self) -> u64 {
        self.values.capacity() as u64 * 8
    }
}
impl ImageRequest {
    pub fn prepare<'a>(&'a self, inputs: ImageInputs<'a>, limits: ImageLimits) -> Result<PreparedImageOperation<'a>, E> {
        Ok(PreparedImageOperation { request: self, inputs, plan: super::validate::plan(self, inputs, limits)?, limits })
    }
}
impl PreparedImageOperation<'_> {
    pub fn request(&self) -> &ImageRequest {
        self.request
    }
    pub fn inputs(&self) -> ImageInputs<'_> {
        self.inputs
    }
    pub fn reference_working_bytes(&self) -> Result<u64, E> {
        let input = (self.inputs.source.capacity() as u64).checked_add(self.inputs.overlay.map_or(0, |r| r.capacity() as u64)).ok_or(E::ScratchLimit)?;
        let scratch = super::validate::cost(self.request, &self.plan, self.plan.bytes)?.1;
        let output = if let ImageRequest::Duplicate { count } = self.request { u64::from(*count) * 64 } else { self.plan.bytes };
        input.checked_add(output).and_then(|v| v.checked_add(scratch)).ok_or(E::ScratchLimit)
    }
    pub fn reference_work(&self) -> Result<u64, E> {
        Ok(super::validate::cost(self.request, &self.plan, self.plan.bytes)?.0)
    }
    pub fn limits(&self) -> ImageLimits {
        self.limits
    }
    pub fn layout(&self) -> PreparedLayout {
        let p = self.plan;
        let rect = |r: super::geometry::Rect| [r.x, r.y, r.width, r.height];
        PreparedLayout { width: p.width, height: p.height, source_rect: rect(p.rect), overlay_rect: p.overlay.map(rect) }
    }
    pub fn identity(&self) -> bool {
        if super::geometry::identity(self.request, self.inputs.source, &self.plan) {
            return true;
        }
        match self.request {
            ImageRequest::Blur { options } => match *options {
                Blur::Gaussian { sigma } | Blur::TiltShift { sigma, .. } | Blur::TargetColor { sigma, .. } => sigma == 0.,
                Blur::Box { radius } | Blur::Bilateral { radius, .. } => radius == 0,
                Blur::Motion { length, .. } => length == 0.,
                Blur::Radial { amount, .. } => amount == 0.,
                Blur::Bokeh { radius, .. } => radius == 0.,
            },
            _ => false,
        }
    }
    /// Reserve alternate-executor storage/work in addition to the reference lifetime envelope.
    /// This conservative envelope can reject GPU layouts that fit the CPU's row-ring scratch.
    pub fn admit_extra(&self, bytes: u64, visits: u64) -> Result<(), E> {
        let limits = ImageLimits {
            working_bytes: self.limits.working_bytes.checked_sub(bytes).ok_or(E::ScratchLimit)?,
            work: self.limits.work.checked_sub(visits).ok_or(E::WorkLimit)?,
            ..self.limits
        };
        let input = (self.inputs.source.capacity() as u64).checked_add(self.inputs.overlay.map_or(0, |r| r.capacity() as u64)).ok_or(E::ScratchLimit)?;
        super::validate::admit(self.request, &self.plan, input, limits)
    }
    /// Exact original-image matcher semantics; callers must reserve all upload/device copies too.
    pub fn prepare_matcher(&self) -> Result<Option<PreparedMatcher>, E> {
        let matcher = match self.request {
            ImageRequest::Key { options } => options.matcher,
            ImageRequest::ReplaceColor { options } => options.matcher,
            ImageRequest::Blur { options: Blur::TargetColor { matcher, .. } } => *matcher,
            _ => return Ok(None),
        };
        let count = u64::from(self.inputs.source.width()) * u64::from(self.inputs.source.height());
        self.admit_extra(count.checked_mul(8).ok_or(E::ScratchLimit)?, count)?;
        let mut values = Vec::new();
        let n = usize::try_from(count).map_err(|_| E::Allocation)?;
        values.try_reserve_exact(n).map_err(|_| E::Allocation)?;
        self.admit_extra((values.capacity() as u64).checked_mul(8).ok_or(E::ScratchLimit)?, count)?;
        let mut visits = 0u64;
        for y in 0..self.inputs.source.height() {
            for x in 0..self.inputs.source.width() {
                visits = visits.checked_add(1).ok_or(E::WorkLimit)?;
                let p = super::geometry::pixel(self.inputs.source, x, y).straight();
                let coverage = super::color::mask(p, matcher);
                let (mix, alpha) = match self.request {
                    ImageRequest::Key { options } => {
                        let m = if options.mode == KeyMode::KeepMatched { coverage } else { 1. - coverage };
                        (m, (p.a * m * 255.).clamp(0., 255.).round() as u32)
                    }
                    ImageRequest::ReplaceColor { options } => {
                        let a = if options.replace_alpha { p.a * (1. - coverage) + f64::from(options.replacement.a) / 255. * coverage } else { p.a };
                        (coverage, (a * 255.).clamp(0., 255.).round() as u32)
                    }
                    ImageRequest::Blur { options: Blur::TargetColor { selection, .. } } => {
                        (if *selection == Selection::Matched { coverage } else { 1. - coverage }, u32::MAX)
                    }
                    _ => return Err(E::InputRoles),
                };
                values.push([(mix as f32).to_bits(), alpha]);
            }
        }
        Ok(Some(PreparedMatcher { values, visits }))
    }
    /// Coordinates use the exact CPU nearest rounding/clamping, including negative half ties.
    pub fn nearest_resize_axes(&self) -> Result<Option<PreparedAxes>, E> {
        if !matches!(self.request, ImageRequest::Resize { options: Resize { resampling: Resampling::Nearest, .. } }) {
            return Ok(None);
        }
        let count = u64::from(self.plan.width) + u64::from(self.plan.height);
        self.admit_extra(count.checked_mul(4).ok_or(E::ScratchLimit)?, count)?;
        let axis = |length: u32, start: f64, extent: f64, source: u32| -> Result<Vec<u32>, E> {
            let mut out = Vec::new();
            out.try_reserve_exact(length as usize).map_err(|_| E::Allocation)?;
            for v in 0..length {
                let s = start + (f64::from(v) + 0.5) * extent / f64::from(length) - 0.5;
                out.push((s.round() as i64).clamp(0, i64::from(source) - 1) as u32);
            }
            Ok(out)
        };
        let x = axis(self.plan.width, self.plan.rect.x, self.plan.rect.width, self.inputs.source.width())?;
        let y = axis(self.plan.height, self.plan.rect.y, self.plan.rect.height, self.inputs.source.height())?;
        self.admit_extra(((x.capacity() as u64) + (y.capacity() as u64)).checked_mul(4).ok_or(E::ScratchLimit)?, count)?;
        Ok(Some((x, y)))
    }
    pub fn finish(&self, pixels: Vec<u8>) -> Result<Raster, E> {
        super::raster::Raster::output(self.plan.width, self.plan.height, pixels, self.limits)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CpuExecutor, ImageResult};
    use super::*;
    #[test]
    fn exact_key_alpha_and_rgb_roundtrip() {
        let limits = ImageLimits::default();
        let mut bytes = Vec::new();
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                bytes.extend_from_slice(&[channel, channel, channel, alpha]);
            }
        }
        let source = Raster::new(256, 256, bytes, limits).unwrap();
        let req = ImageRequest::Key { options: Key { matcher: Matcher { target: Color::GREEN, tolerance: 0., softness: 0. }, mode: KeyMode::RemoveMatched } };
        let prepared = req.prepare(ImageInputs::single(&source), limits).unwrap();
        let plane = prepared.prepare_matcher().unwrap().unwrap();
        let ImageResult::Image(cpu) = req.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), limits).unwrap() else { panic!("image") };
        for ((src, decision), out) in source.pixels().as_chunks::<4>().0.iter().zip(plane.values()).zip(cpu.pixels().as_chunks::<4>().0.iter()) {
            assert_eq!(decision[1] as u8, out[3]);
            if decision[1] > 0 {
                assert_eq!(&src[..3], &out[..3]);
            } else {
                assert_eq!(out, &[0; 4]);
            }
        }
    }
    #[test]
    fn shared_admission_rejects_extra_before_decisions() {
        let limits = ImageLimits::default();
        let source = Raster::new(1, 1, vec![255; 4], limits).unwrap();
        let req = ImageRequest::Key { options: Key::default() };
        let prep = req.prepare(ImageInputs::single(&source), limits).unwrap();
        assert_eq!(prep.admit_extra(limits.working_bytes, 0), Err(E::ScratchLimit));
        assert_eq!(prep.admit_extra(0, limits.work), Err(E::WorkLimit));
        assert!(prep.prepare_matcher().unwrap().is_some());
    }
}
