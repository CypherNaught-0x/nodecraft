use super::geometry::{Plan, layout};
use super::params::*;
use super::{ImageError as E, ImageInputs, ImageLimits, ImageRequest};
pub(super) fn range(v: f64, lo: f64, hi: f64, id: &'static str) -> Result<(), E> {
    if v.is_finite() && (lo..=hi).contains(&v) { Ok(()) } else { Err(E::Parameter(id)) }
}
pub(super) fn sigma(v: f64) -> Result<(), E> {
    range(v, 0., 64., "image.blur.sigma")?;
    if v > 0. && v < 1. / 256. { Err(E::Parameter("image.blur.sigma")) } else { Ok(()) }
}
fn point(p: Point) -> Result<(), E> {
    range(p.x, 0., 1., "image.position.x")?;
    range(p.y, 0., 1., "image.position.y")
}
fn matcher(m: Matcher) -> Result<(), E> {
    opaque(m.target)?;
    range(m.tolerance, 0., 1., "image.matcher.tolerance")?;
    range(m.softness, 0., 1., "image.matcher.softness")
}
fn opaque(c: Color) -> Result<(), E> {
    if c.a == 255 { Ok(()) } else { Err(E::Parameter("image.color.alpha")) }
}
fn samples(n: u32) -> Result<(), E> {
    if (1..=129).contains(&n) { Ok(()) } else { Err(E::Parameter("image.blur.samples")) }
}
pub(super) fn parameters(req: &ImageRequest) -> Result<(), E> {
    match req {
        ImageRequest::Grade { options: g } => {
            range(g.warmth, -1., 1., "image.grade.warmth")?;
            range(g.contrast, 0., 4., "image.grade.contrast")?;
            range(g.saturation, 0., 4., "image.grade.saturation")?;
            range(g.brightness, -4., 4., "image.grade.brightness")
        }
        ImageRequest::Filter { options: f } => {
            range(f.intensity, 0., 1., "image.filter.intensity")?;
            if let Preset::Duotone { shadow, highlight } = f.preset {
                opaque(shadow)?;
                opaque(highlight)?
            }
            Ok(())
        }
        ImageRequest::Tint { options: t } => range(t.intensity, 0., 1., "image.tint.intensity"),
        ImageRequest::Key { options: k } => matcher(k.matcher),
        ImageRequest::ReplaceColor { options: r } => matcher(r.matcher),
        ImageRequest::Aspect { options: a } => {
            point(a.anchor)?;
            if let Ratio::Custom { numerator, denominator } = a.ratio
                && (!(1..=16384).contains(&numerator) || !(1..=16384).contains(&denominator))
            {
                return Err(E::Parameter("image.aspect.ratio"));
            }
            if let AspectMode::Pad { padding: Padding::Blurred { sigma: s } } = a.mode {
                sigma(s)?
            }
            Ok(())
        }
        ImageRequest::Rotate { options: r } => {
            range(r.angle, -360., 360., "image.rotate.angle")?;
            if r.resampling == Resampling::Area {
                return Err(E::Parameter("image.rotate.resampling"));
            }
            Ok(())
        }
        ImageRequest::Crop { options: c } => {
            if let Crop::Normalized { x, y, width, height } = *c {
                range(x, 0., 1., "image.crop.x")?;
                range(y, 0., 1., "image.crop.y")?;
                range(width, f64::MIN_POSITIVE, 1., "image.crop.width")?;
                range(height, f64::MIN_POSITIVE, 1., "image.crop.height")?;
                if x + width > 1. || y + height > 1. {
                    return Err(E::Parameter("image.crop.rect"));
                }
            }
            Ok(())
        }
        ImageRequest::Resize { options: r } => {
            match r.size {
                ResizeSize::Scale { factor } => range(factor, 1. / 64., 64., "image.resize.factor")?,
                ResizeSize::Exact { width, height, anchor, .. } => {
                    dimension(width, height)?;
                    point(anchor)?
                }
            }
            Ok(())
        }
        ImageRequest::Overlay { options: o } => {
            range(o.opacity, 0., 1., "image.overlay.opacity")?;
            if o.offset_x.unsigned_abs() > 16384 || o.offset_y.unsigned_abs() > 16384 {
                return Err(E::Parameter("image.overlay.offset"));
            }
            match o.size {
                OverlaySize::RelativeWidth { fraction } => range(fraction, 1. / 1024., 4., "image.overlay.fraction")?,
                OverlaySize::Pixels { width } => dimension(width, 1)?,
                OverlaySize::Source {} => {}
            }
            if let Anchor::Custom { point: p } = o.anchor {
                point(p)?
            }
            Ok(())
        }
        ImageRequest::Blur { options: b } => {
            match *b {
                Blur::Gaussian { sigma: s } => sigma(s)?,
                Blur::Box { radius } => {
                    if radius > 128 {
                        return Err(E::Parameter("image.blur.radius"));
                    }
                }
                Blur::Motion { length, angle, samples: n } => {
                    range(length, 0., 256., "image.blur.length")?;
                    range(angle, -360., 360., "image.blur.angle")?;
                    samples(n)?
                }
                Blur::Radial { center, amount, samples: n, .. } => {
                    point(center)?;
                    range(amount, 0., 1., "image.blur.amount")?;
                    samples(n)?
                }
                Blur::Bilateral { radius, spatial_sigma, range_sigma } => {
                    if radius > 8 {
                        return Err(E::Parameter("image.blur.radius"));
                    }
                    range(spatial_sigma, 1. / 256., 8., "image.blur.spatial_sigma")?;
                    range(range_sigma, 1. / 65536., 1., "image.blur.range_sigma")?
                }
                Blur::Bokeh { radius, samples: n, .. } => {
                    range(radius, 0., 64., "image.blur.radius")?;
                    samples(n)?
                }
                Blur::TiltShift { sigma: s, focus, angle, band_width, falloff } => {
                    sigma(s)?;
                    point(focus)?;
                    range(angle, -180., 180., "image.blur.angle")?;
                    range(band_width, 0., 1., "image.blur.band_width")?;
                    range(falloff, 1. / 65536., 1., "image.blur.falloff")?
                }
                Blur::TargetColor { sigma: s, matcher: m, .. } => {
                    sigma(s)?;
                    matcher(m)?
                }
            }
            Ok(())
        }
        ImageRequest::Duplicate { count } => {
            if (1..=256).contains(count) {
                Ok(())
            } else {
                Err(E::Parameter("image.duplicate.count"))
            }
        }
        ImageRequest::Flip { .. } => Ok(()),
    }
}
pub(super) fn dimension(w: u32, h: u32) -> Result<(), E> {
    if w == 0 || h == 0 || w > 16384 || h > 16384 { Err(E::Dimensions) } else { Ok(()) }
}
pub(super) fn add(a: u64, b: u64) -> Result<u64, E> {
    a.checked_add(b).ok_or(E::ScratchLimit)
}
pub(super) fn mul(a: u64, b: u64) -> Result<u64, E> {
    a.checked_mul(b).ok_or(E::WorkLimit)
}
pub(super) fn plan(req: &ImageRequest, inputs: ImageInputs<'_>, limits: ImageLimits) -> Result<Plan, E> {
    limits.validate()?;
    parameters(req)?;
    if matches!(req, ImageRequest::Overlay { .. }) != inputs.overlay.is_some() {
        return Err(E::InputRoles);
    }
    let source = inputs.source;
    let mut retained = source.capacity() as u64;
    limits.image_bytes(source.width(), source.height())?;
    if let Some(over) = inputs.overlay {
        limits.image_bytes(over.width(), over.height())?;
        retained = add(retained, over.capacity() as u64)?
    }
    if retained > limits.input_bytes {
        return Err(E::InputLimit);
    }
    let p = layout(req, source.width(), source.height(), inputs.overlay.map(|i| (i.width(), i.height())))?;
    admit(req, &p, retained, limits)?;
    Ok(p)
}
pub(super) fn admit(req: &ImageRequest, p: &Plan, input: u64, l: ImageLimits) -> Result<(), E> {
    if input > l.input_bytes {
        return Err(E::InputLimit);
    }
    let bytes = l.image_bytes(p.width, p.height)?;
    let count = if let ImageRequest::Duplicate { count } = req { *count } else { 1 };
    if count > l.outputs || mul(bytes, u64::from(count))? > l.output_bytes {
        return Err(E::OutputLimit);
    }
    let pixels = bytes / 4;
    let mut work = pixels;
    let mut scratch = 4096;
    match req {
        ImageRequest::Blur { options } => {
            let (w, s) = super::blur::cost(p.width, p.height, options)?;
            work = add(work, w)?;
            scratch = add(scratch, s)?
        }
        ImageRequest::Resize { options } => work = mul(pixels, p.resample_cost(options.resampling))?,
        ImageRequest::Rotate { options } => work = mul(pixels, if options.resampling == Resampling::Nearest { 2 } else { 5 })?,
        ImageRequest::Overlay { options } => work = mul(pixels, 1 + p.resample_cost(options.resampling))?,
        ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { sigma } }, .. } } => {
            let (w, s) = super::blur::cost(p.width, p.height, &Blur::Gaussian { sigma: *sigma })?;
            work = add(w, mul(pixels, 6)?)?;
            scratch = add(scratch, add(s, bytes)?)?;
        }
        _ => {}
    }
    if work > l.work {
        return Err(E::WorkLimit);
    }
    // Account input retention, output, all kernel scratch and collection/Arc metadata together.
    let output = if matches!(req, ImageRequest::Duplicate { .. }) { u64::from(count) * 64 } else { bytes };
    if add(add(input, output)?, scratch)? > l.working_bytes {
        return Err(E::ScratchLimit);
    }
    Ok(())
}
