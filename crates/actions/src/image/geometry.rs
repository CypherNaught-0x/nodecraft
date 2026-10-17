use super::color::Pixel;
use super::params::*;
use super::validate::{add, dimension};
use super::{CpuExecutor, ImageError as E, ImageInputs, ImageLimits, ImageRequest, Raster};
#[derive(Clone, Copy, Debug)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Plan {
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub rect: Rect,
    pub overlay: Option<Rect>,
    sx: f64,
    sy: f64,
}
impl Plan {
    pub fn resample_cost(self, r: Resampling) -> u64 {
        match r {
            Resampling::Nearest => 1,
            Resampling::Bilinear => 4,
            Resampling::Area => {
                let axis = |s: f64| if s > 1. { s.ceil() as u64 + 1 } else { 2 };
                axis(self.sx) * axis(self.sy)
            }
        }
    }
}
fn size(v: f64) -> Result<u32, E> {
    if !v.is_finite() || v < 1. || v > 16384. { Err(E::Dimensions) } else { Ok(v as u32) }
}
fn ratio(r: Ratio, w: u32, h: u32) -> (u64, u64) {
    match r {
        Ratio::Source {} => (w.into(), h.into()),
        Ratio::Square {} => (1, 1),
        Ratio::Landscape4_3 {} => (4, 3),
        Ratio::Portrait3_4 {} => (3, 4),
        Ratio::Landscape16_9 {} => (16, 9),
        Ratio::Portrait9_16 {} => (9, 16),
        Ratio::Custom { numerator, denominator } => (numerator.into(), denominator.into()),
    }
}
fn aspect_size(w: u32, h: u32, n: u64, d: u64, pad: bool) -> Result<(u32, u32), E> {
    let wd = u64::from(w).checked_mul(d).ok_or(E::Dimensions)?;
    let hn = u64::from(h).checked_mul(n).ok_or(E::Dimensions)?;
    let divide = |v: u64, by: u64| -> Result<u32, E> {
        if by == 0 {
            return Err(E::Dimensions);
        }
        let v = if pad { v.div_ceil(by) } else { v / by };
        u32::try_from(v.max(1)).map_err(|_| E::Dimensions)
    };
    if wd > hn {
        if pad { Ok((w, divide(wd, n)?)) } else { Ok((divide(hn, d)?, h)) }
    } else if pad {
        Ok((divide(hn, d)?, h))
    } else {
        Ok((w, divide(wd, n)?))
    }
}
fn crop_ratio(w: u32, h: u32, n: u64, d: u64, a: Point) -> Result<Rect, E> {
    let (cw, ch) = aspect_size(w, h, n, d, false)?;
    Ok(Rect { x: (f64::from(w - cw) * a.x).round(), y: (f64::from(h - ch) * a.y).round(), width: cw.into(), height: ch.into() })
}
pub(super) fn layout(req: &ImageRequest, w: u32, h: u32, over: Option<(u32, u32)>) -> Result<Plan, E> {
    let (fw, fh) = (f64::from(w), f64::from(h));
    let mut p =
        Plan { width: w, height: h, bytes: u64::from(w) * u64::from(h) * 4, rect: Rect { x: 0., y: 0., width: fw, height: fh }, overlay: None, sx: 1., sy: 1. };
    match req {
        ImageRequest::Crop { options } => {
            p.rect = match *options {
                Crop::Full {} => p.rect,
                Crop::Pixels { x, y, width, height } => {
                    dimension(width, height)?;
                    if x.checked_add(width).is_none_or(|n| n > w) || y.checked_add(height).is_none_or(|n| n > h) {
                        return Err(E::Parameter("image.crop.rect"));
                    }
                    Rect { x: x.into(), y: y.into(), width: width.into(), height: height.into() }
                }
                Crop::Normalized { x, y, width, height } => {
                    let x0 = (x * fw).floor();
                    let y0 = (y * fh).floor();
                    Rect { x: x0, y: y0, width: ((x + width) * fw).ceil() - x0, height: ((y + height) * fh).ceil() - y0 }
                }
            };
            p.width = size(p.rect.width)?;
            p.height = size(p.rect.height)?
        }
        ImageRequest::Aspect { options: a } => {
            let (n, d) = ratio(a.ratio, w, h);
            match a.mode {
                AspectMode::Crop {} => {
                    p.rect = crop_ratio(w, h, n, d, a.anchor)?;
                    p.width = size(p.rect.width)?;
                    p.height = size(p.rect.height)?
                }
                AspectMode::Pad { .. } => {
                    (p.width, p.height) = aspect_size(w, h, n, d, true)?;
                    p.rect.x = (f64::from(p.width - w) * a.anchor.x).round();
                    p.rect.y = (f64::from(p.height - h) * a.anchor.y).round();
                }
            }
        }
        ImageRequest::Resize { options: r } => {
            match r.size {
                ResizeSize::Scale { factor } => {
                    p.width = size((fw * factor).round().max(1.))?;
                    p.height = size((fh * factor).round().max(1.))?
                }
                ResizeSize::Exact { width, height, fit, anchor } => {
                    p.width = width;
                    p.height = height;
                    match fit {
                        Fit::Stretch => {}
                        Fit::Contain => {
                            let scale = (f64::from(width) / fw).min(f64::from(height) / fh);
                            p.width = size((fw * scale).round().max(1.))?;
                            p.height = size((fh * scale).round().max(1.))?
                        }
                        Fit::Cover => p.rect = crop_ratio(w, h, width.into(), height.into(), anchor)?,
                    }
                }
            }
            p.sx = p.rect.width / f64::from(p.width);
            p.sy = p.rect.height / f64::from(p.height)
        }
        ImageRequest::Rotate { options: r } => {
            if r.expand {
                let (sn, cs) = angle(r.angle);
                p.width = size((fw * cs.abs() + fh * sn.abs()).ceil().max(1.))?;
                p.height = size((fw * sn.abs() + fh * cs.abs()).ceil().max(1.))?
            }
        }
        ImageRequest::Overlay { options: o } => {
            let (ow, oh) = over.ok_or(E::InputRoles)?;
            let width = match o.size {
                OverlaySize::Source {} => f64::from(ow),
                OverlaySize::RelativeWidth { fraction } => (fw * fraction).round().max(1.),
                OverlaySize::Pixels { width } => f64::from(width),
            };
            let height = (width * f64::from(oh) / f64::from(ow)).round().max(1.);
            dimension(size(width)?, size(height)?)?;
            let a = anchor(o.anchor);
            p.overlay =
                Some(Rect { x: ((fw - width) * a.x).round() + f64::from(o.offset_x), y: ((fh - height) * a.y).round() + f64::from(o.offset_y), width, height });
            p.sx = f64::from(ow) / width;
            p.sy = f64::from(oh) / height
        }
        _ => {}
    }
    dimension(p.width, p.height)?;
    p.bytes = u64::from(p.width) * u64::from(p.height) * 4;
    Ok(p)
}
fn anchor(a: Anchor) -> Point {
    let (x, y) = match a {
        Anchor::Center {} => (0.5, 0.5),
        Anchor::TopLeft {} => (0., 0.),
        Anchor::Top {} => (0.5, 0.),
        Anchor::TopRight {} => (1., 0.),
        Anchor::Left {} => (0., 0.5),
        Anchor::Right {} => (1., 0.5),
        Anchor::BottomLeft {} => (0., 1.),
        Anchor::Bottom {} => (0.5, 1.),
        Anchor::BottomRight {} => (1., 1.),
        Anchor::Custom { point } => return point,
    };
    Point { x, y }
}
pub(super) fn angle(a: f64) -> (f64, f64) {
    let a = a.rem_euclid(360.);
    if a == 0. {
        (0., 1.)
    } else if a == 90. {
        (1., 0.)
    } else if a == 180. {
        (0., -1.)
    } else if a == 270. {
        (-1., 0.)
    } else {
        a.to_radians().sin_cos()
    }
}
pub(super) fn pixel(src: &Raster, x: u32, y: u32) -> Pixel {
    if x >= src.width() || y >= src.height() {
        return Pixel::default();
    }
    let i = (u64::from(y) * u64::from(src.width()) + u64::from(x)) * 4;
    usize::try_from(i).ok().and_then(|n| src.pixels().get(n..n + 4)).map(Pixel::from_bytes).unwrap_or_default().premult()
}
fn at(src: &Raster, x: i64, y: i64, clamp: bool) -> Pixel {
    if clamp {
        pixel(src, x.clamp(0, i64::from(src.width()) - 1) as u32, y.clamp(0, i64::from(src.height()) - 1) as u32)
    } else if x < 0 || y < 0 {
        Pixel::default()
    } else {
        pixel(src, x as u32, y as u32)
    }
}
pub(super) fn sample(src: &Raster, x: f64, y: f64, method: Resampling, clamp: bool) -> Pixel {
    if method == Resampling::Nearest {
        return at(src, x.round() as i64, y.round() as i64, clamp);
    }
    let (ix, iy) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    at(src, ix, iy, clamp).mix(at(src, ix + 1, iy, clamp), fx).mix(at(src, ix, iy + 1, clamp).mix(at(src, ix + 1, iy + 1, clamp), fx), fy)
}
fn axis(start: f64, extent: f64) -> (i64, i64) {
    if extent > 1. {
        (start.floor() as i64, (start + extent).ceil() as i64 - 1)
    } else {
        let center = start + extent / 2. - 0.5;
        (center.floor() as i64, center.floor() as i64 + 1)
    }
}
fn weight(i: i64, start: f64, extent: f64) -> f64 {
    if extent > 1. {
        ((start + extent).min(i as f64 + 1.) - start.max(i as f64)).max(0.) / extent
    } else {
        let center = start + extent / 2. - 0.5;
        (1. - (i as f64 - center).abs()).max(0.)
    }
}
pub(super) fn resized(src: &Raster, rect: Rect, x: f64, y: f64, w: f64, h: f64, method: Resampling) -> Pixel {
    let (sx, sy) = (rect.width / w, rect.height / h);
    let (startx, starty) = (rect.x + x * sx, rect.y + y * sy);
    if method != Resampling::Area {
        return sample(src, startx + sx / 2. - 0.5, starty + sy / 2. - 0.5, method, true);
    }
    let (x0, x1) = axis(startx, sx);
    let (y0, y1) = axis(starty, sy);
    let mut sum = Pixel::default();
    for iy in y0..=y1 {
        let wy = weight(iy, starty, sy);
        for ix in x0..=x1 {
            sum = sum.plus(at(src, ix, iy, true).scale(wy * weight(ix, startx, sx)))
        }
    }
    sum
}
pub(super) fn render(req: &ImageRequest, inputs: ImageInputs<'_>, p: &Plan, x: u32, y: u32) -> Pixel {
    let src = inputs.source;
    match req {
        ImageRequest::Flip { axis } => pixel(
            src,
            if matches!(axis, Axis::Horizontal | Axis::Both) { src.width() - 1 - x } else { x },
            if matches!(axis, Axis::Vertical | Axis::Both) { src.height() - 1 - y } else { y },
        ),
        ImageRequest::Crop { .. } => pixel(src, x + p.rect.x as u32, y + p.rect.y as u32),
        ImageRequest::Resize { options } => resized(src, p.rect, x.into(), y.into(), p.width.into(), p.height.into(), options.resampling),
        ImageRequest::Rotate { options: r } => {
            let (sn, cs) = angle(r.angle);
            let dx = f64::from(x) + 0.5 - f64::from(p.width) / 2.;
            let dy = f64::from(y) + 0.5 - f64::from(p.height) / 2.;
            let sx = cs * dx + sn * dy + f64::from(src.width()) / 2. - 0.5;
            let sy = -sn * dx + cs * dy + f64::from(src.height()) / 2. - 0.5;
            sample(src, sx, sy, r.resampling, false).over(Pixel::from_color(r.background).premult())
        }
        ImageRequest::Overlay { options: o } => {
            let base = pixel(src, x, y);
            let Some(over) = inputs.overlay else { return base };
            let Some(r) = p.overlay else { return base };
            let (dx, dy) = (f64::from(x) - r.x, f64::from(y) - r.y);
            if dx < 0. || dy < 0. || dx >= r.width || dy >= r.height {
                return base;
            }
            resized(over, Rect { x: 0., y: 0., width: over.width().into(), height: over.height().into() }, dx, dy, r.width, r.height, o.resampling)
                .scale(o.opacity)
                .over(base)
        }
        _ => pixel(src, x, y),
    }
}
pub(super) fn aspect(src: &Raster, a: Aspect, p: &Plan, exec: &mut CpuExecutor, l: ImageLimits) -> Result<Raster, E> {
    let mut background = None;
    if let AspectMode::Pad { padding: Padding::Blurred { sigma } } = a.mode {
        let rect = crop_ratio(src.width(), src.height(), p.width.into(), p.height.into(), Point::default())?;
        let mut data = super::raster::buffer(p.bytes as usize)?;
        exec.pixels(&mut data, p.width, |x, y| {
            resized(src, rect, x.into(), y.into(), p.width.into(), p.height.into(), Resampling::Bilinear).straight().bytes()
        });
        let scaled = Raster::output(p.width, p.height, data, l)?;
        let reduced = ImageLimits { working_bytes: l.working_bytes.checked_sub(src.capacity() as u64).ok_or(E::ScratchLimit)?, ..l };
        background = Some(super::blur::apply(&scaled, &Blur::Gaussian { sigma }, exec, reduced)?);
    }
    let mut data = super::raster::buffer(p.bytes as usize)?;
    if add(data.capacity() as u64, background.as_ref().map_or(0, |b| b.capacity() as u64))? > l.working_bytes {
        return Err(E::ScratchLimit);
    }
    exec.pixels(&mut data, p.width, |x, y| {
        if matches!(a.mode, AspectMode::Crop {}) {
            return raw(src, i64::from(x) + p.rect.x as i64, i64::from(y) + p.rect.y as i64);
        }
        match a.mode {
            AspectMode::Crop {} => pixel(src, x + p.rect.x as u32, y + p.rect.y as u32),
            AspectMode::Pad { padding } => {
                let bg = match padding {
                    Padding::Solid { color } => Pixel::from_color(color).premult(),
                    Padding::Blurred { .. } => background.as_ref().map(|b| pixel(b, x, y)).unwrap_or_default(),
                };
                let dx = i64::from(x) - p.rect.x as i64;
                let dy = i64::from(y) - p.rect.y as i64;
                at(src, dx, dy, false).over(bg)
            }
        }
        .straight()
        .bytes()
    });
    Raster::output(p.width, p.height, data, l)
}

pub(super) fn identity(req: &ImageRequest, src: &Raster, p: &Plan) -> bool {
    match req {
        ImageRequest::Grade { options } => *options == Grade::default(),
        ImageRequest::Filter { options } => options.intensity == 0. || matches!(options.preset, Preset::None {}),
        ImageRequest::Tint { options } => options.intensity == 0. || options.color.a == 0,
        ImageRequest::Crop { options: Crop::Full {} } => true,
        ImageRequest::Resize { .. } => {
            p.width == src.width()
                && p.height == src.height()
                && p.rect.x == 0.
                && p.rect.y == 0.
                && p.rect.width == f64::from(src.width())
                && p.rect.height == f64::from(src.height())
        }
        ImageRequest::Aspect { options: Aspect { ratio: Ratio::Source {}, mode: AspectMode::Crop {}, .. } } => true,
        ImageRequest::Overlay { options } => options.opacity == 0.,
        _ => false,
    }
}
fn raw(src: &Raster, x: i64, y: i64) -> [u8; 4] {
    if x < 0 || y < 0 || x >= i64::from(src.width()) || y >= i64::from(src.height()) {
        return [0; 4];
    }
    let n = (y as usize * src.width() as usize + x as usize) * 4;
    src.pixels().get(n..n + 4).and_then(|v| v.try_into().ok()).unwrap_or([0; 4])
}
pub(super) fn exact(req: &ImageRequest, src: &Raster, p: &Plan, x: u32, y: u32) -> Option<[u8; 4]> {
    Some(match req {
        ImageRequest::Flip { axis } => raw(
            src,
            i64::from(if matches!(axis, Axis::Horizontal | Axis::Both) { src.width() - 1 - x } else { x }),
            i64::from(if matches!(axis, Axis::Vertical | Axis::Both) { src.height() - 1 - y } else { y }),
        ),
        ImageRequest::Crop { .. } => raw(src, i64::from(x) + p.rect.x as i64, i64::from(y) + p.rect.y as i64),
        ImageRequest::Resize { options: Resize { resampling: Resampling::Nearest, .. } } => {
            let sx = p.rect.x + (f64::from(x) + 0.5) * p.rect.width / f64::from(p.width) - 0.5;
            let sy = p.rect.y + (f64::from(y) + 0.5) * p.rect.height / f64::from(p.height) - 0.5;
            raw(src, (sx.round() as i64).clamp(0, i64::from(src.width()) - 1), (sy.round() as i64).clamp(0, i64::from(src.height()) - 1))
        }
        ImageRequest::Rotate { options: r } if r.background.a == 0 && r.angle.rem_euclid(90.) == 0. => {
            let (sn, cs) = angle(r.angle);
            let dx = f64::from(x) + 0.5 - f64::from(p.width) / 2.;
            let dy = f64::from(y) + 0.5 - f64::from(p.height) / 2.;
            let sx = cs * dx + sn * dy + f64::from(src.width()) / 2. - 0.5;
            let sy = -sn * dx + cs * dy + f64::from(src.height()) / 2. - 0.5;
            if r.resampling != Resampling::Nearest && (sx.fract() != 0. || sy.fract() != 0.) {
                return None;
            }
            raw(src, sx.round() as i64, sy.round() as i64)
        }
        _ => return None,
    })
}
