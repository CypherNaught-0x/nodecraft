use super::color::{Pixel, mask};
use super::geometry::{pixel, sample};
use super::params::*;
use super::validate::{add, mul};
use super::{CpuExecutor, ImageError as E, ImageInputs, ImageLimits, ImageRequest, Raster};
fn support(s: f64) -> u32 {
    (s * 3.).ceil() as u32
}
fn sigma(b: &Blur) -> Option<f64> {
    match *b {
        Blur::Gaussian { sigma } | Blur::TiltShift { sigma, .. } | Blur::TargetColor { sigma, .. } => Some(sigma),
        _ => None,
    }
}
pub(super) fn cost(w: u32, h: u32, b: &Blur) -> Result<(u64, u64), E> {
    let p = u64::from(w) * u64::from(h);
    if let Some(s) = sigma(b) {
        let r = u64::from(support(s));
        let taps = 2 * r + 1;
        let rows = taps.min(u64::from(h));
        return Ok((add(mul(p, 2 * taps)?, p)?, add(mul(mul(rows, u64::from(w))?, 16)?, 4096)?));
    }
    Ok(match *b {
        Blur::Box { radius } => {
            let taps = 2 * u64::from(radius) + 1;
            let work = add(mul(p, 8)?, mul(u64::from(w) + u64::from(h), taps)?)?;
            let scratch = add(mul(mul(taps.min(h.into()), w.into())?, 16)?, mul(w.into(), 32)?)?;
            (work, scratch)
        }
        Blur::Motion { samples, .. } | Blur::Radial { samples, .. } | Blur::Bokeh { samples, .. } => (mul(p, u64::from(samples) * 4)?, 4096),
        Blur::Bilateral { radius, .. } => (mul(p, (2 * u64::from(radius) + 1).pow(2))?, 4096),
        _ => (p, 4096),
    })
}
fn identity(b: &Blur) -> bool {
    match *b {
        Blur::Gaussian { sigma } | Blur::TiltShift { sigma, .. } | Blur::TargetColor { sigma, .. } => sigma == 0.,
        Blur::Box { radius } | Blur::Bilateral { radius, .. } => radius == 0,
        Blur::Motion { length, .. } => length == 0.,
        Blur::Radial { amount, .. } => amount == 0.,
        Blur::Bokeh { radius, .. } => radius == 0.,
    }
}
fn float_buffer(count: usize) -> Result<Vec<[f32; 4]>, E> {
    let mut v = Vec::new();
    v.try_reserve_exact(count).map_err(|_| E::Allocation)?;
    if v.capacity() != count {
        return Err(E::Allocation);
    }
    v.resize(count, [0.; 4]);
    Ok(v)
}
pub(super) fn apply(src: &Raster, b: &Blur, exec: &mut CpuExecutor, l: ImageLimits) -> Result<Raster, E> {
    exec.check_cancelled()?;
    super::validate::plan(&ImageRequest::Blur { options: b.clone() }, ImageInputs::single(src), l)?;
    if identity(b) {
        return Ok(src.clone());
    }
    if let Some(s) = sigma(b) {
        return separable(src, b, support(s), Some(s), exec, l);
    }
    if let Blur::Box { radius } = *b {
        return separable(src, b, radius, None, exec, l);
    }
    let bytes = l.image_bytes(src.width(), src.height())?;
    let mut out = super::raster::buffer(bytes as usize)?;
    exec.pixels(&mut out, src.width(), |x, y| {
        let (fx, fy) = (f64::from(x), f64::from(y));
        let mut sum = Pixel::default();
        let mut total = 0.;
        match *b {
            Blur::Bilateral { radius, spatial_sigma, range_sigma } => {
                let center = pixel(src, x, y).straight();
                let r = i64::from(radius);
                for dy in -r..=r {
                    for dx in -r..=r {
                        let px = sample(src, fx + dx as f64, fy + dy as f64, Resampling::Nearest, true);
                        let guide = px.straight();
                        // Ratios in f64 avoid squaring a narrowed tiny sigma into zero.
                        let spatial = (dx as f64 / spatial_sigma).powi(2) + (dy as f64 / spatial_sigma).powi(2);
                        let range = ((guide.r - center.r) / range_sigma).powi(2)
                            + ((guide.g - center.g) / range_sigma).powi(2)
                            + ((guide.b - center.b) / range_sigma).powi(2);
                        let weight = if dx == 0 && dy == 0 { 1. } else { (-0.5 * (spatial + range)).exp() };
                        sum = sum.plus(px.scale(weight));
                        total += weight;
                    }
                }
            }
            Blur::Motion { length, angle, samples } => {
                let (sn, cs) = super::geometry::angle(angle);
                for i in 0..samples {
                    let t = if samples == 1 { 0. } else { f64::from(i) / f64::from(samples - 1) - 0.5 };
                    sum = sum.plus(sample(src, fx + t * length * cs, fy + t * length * sn, Resampling::Bilinear, true));
                }
                total = f64::from(samples)
            }
            Blur::Radial { mode, center, amount, samples } => {
                let cx = center.x * f64::from(src.width()) - 0.5;
                let cy = center.y * f64::from(src.height()) - 0.5;
                for i in 0..samples {
                    let t = if samples == 1 { 0.5 } else { f64::from(i) / f64::from(samples - 1) };
                    let (sx, sy) = match mode {
                        RadialMode::Spin => {
                            let (sn, cs) = ((2. * t - 1.) * 90. * amount).to_radians().sin_cos();
                            (cx + (fx - cx) * cs - (fy - cy) * sn, cy + (fx - cx) * sn + (fy - cy) * cs)
                        }
                        RadialMode::Zoom => {
                            let factor = 1. - amount * (1. - t);
                            (cx + (fx - cx) * factor, cy + (fy - cy) * factor)
                        }
                    };
                    sum = sum.plus(sample(src, sx, sy, Resampling::Bilinear, true));
                }
                total = f64::from(samples)
            }
            Blur::Bokeh { radius, samples, aperture } => {
                sum = pixel(src, x, y);
                for i in 1..samples {
                    let theta = f64::from(i) * std::f64::consts::PI * (3. - 5f64.sqrt());
                    let mut r = ((f64::from(i) - 0.5) / f64::from(samples - 1)).sqrt();
                    if aperture == Aperture::Hexagon {
                        let sector = std::f64::consts::PI / 3.;
                        r *= (std::f64::consts::PI / 6.).cos() / (theta.rem_euclid(sector) - sector / 2.).cos()
                    }
                    let (sn, cs) = theta.sin_cos();
                    sum = sum.plus(sample(src, fx + radius * r * cs, fy + radius * r * sn, Resampling::Bilinear, true));
                }
                total = f64::from(samples)
            }
            _ => {}
        }
        if total > 0. { sum.scale(1. / total).straight().bytes() } else { pixel(src, x, y).straight().bytes() }
    })?;
    Raster::output(src.width(), src.height(), out, l)
}
fn post(src: &Raster, b: &Blur, x: u32, y: u32, blurred: Pixel) -> Pixel {
    match *b {
        Blur::TiltShift { focus, angle, band_width, falloff, .. } => {
            let (sn, cs) = angle.to_radians().sin_cos();
            let dx = f64::from(x) + 0.5 - focus.x * f64::from(src.width());
            let dy = f64::from(y) + 0.5 - focus.y * f64::from(src.height());
            let distance = (-sn * dx + cs * dy).abs() / f64::from(src.width().min(src.height()));
            let t = ((distance - band_width / 2.) / falloff).clamp(0., 1.);
            pixel(src, x, y).mix(blurred, t)
        }
        Blur::TargetColor { matcher, selection, .. } => {
            let original = pixel(src, x, y);
            let m = mask(original.straight(), matcher);
            original.mix(blurred, if selection == Selection::Matched { m } else { 1. - m })
        }
        _ => blurred,
    }
}
fn separable(src: &Raster, b: &Blur, r: u32, s: Option<f64>, exec: &mut CpuExecutor, l: ImageLimits) -> Result<Raster, E> {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let taps = 2 * r as usize + 1;
    let rows = taps.min(h);
    let mut ring = float_buffer(rows.checked_mul(w).ok_or(E::ScratchLimit)?)?;
    let mut out = super::raster::buffer(l.image_bytes(src.width(), src.height())? as usize)?;
    let mut weights = Vec::new();
    weights.try_reserve_exact(taps).map_err(|_| E::Allocation)?;
    if weights.capacity() != taps {
        return Err(E::Allocation);
    }
    let mut norm = 0.;
    for i in 0..taps {
        let distance = i as f64 - f64::from(r);
        let weight = if let Some(s) = s { if distance == 0. { 1. } else { (-0.5 * (distance / s).powi(2)).exp() } } else { 1. };
        weights.push(weight);
        norm += weight
    }
    for v in &mut weights {
        *v /= norm;
    }
    let fill = |row: usize, ring: &mut [[f32; 4]]| -> Result<(), E> {
        exec.check_cancelled()?;
        let start = (row % rows) * w;
        let Some(dest) = ring.get_mut(start..start + w) else { return Err(E::BufferShape) };
        if s.is_some() {
            for (x, dst) in dest.iter_mut().enumerate() {
                if x % 256 == 0 {
                    exec.check_cancelled()?;
                }
                let mut sum = Pixel::default();
                for (i, weight) in weights.iter().copied().enumerate() {
                    sum = sum.plus(sample(src, x as f64 + i as f64 - f64::from(r), row as f64, Resampling::Nearest, true).scale(weight))
                }
                *dst = sum.stored();
            }
        } else {
            let mut sum = Pixel::default();
            for dx in -(i64::from(r))..=i64::from(r) {
                sum = sum.plus(sample(src, dx as f64, row as f64, Resampling::Nearest, true))
            }
            for (x, dst) in dest.iter_mut().enumerate() {
                if x % 256 == 0 {
                    exec.check_cancelled()?;
                }
                *dst = sum.scale(1. / taps as f64).stored();
                sum = sum
                    .plus(sample(src, x as f64 + f64::from(r) + 1., row as f64, Resampling::Nearest, true))
                    .plus(sample(src, x as f64 - f64::from(r), row as f64, Resampling::Nearest, true).scale(-1.));
            }
        }
        exec.check_cancelled()
    };
    let initial = (r as usize).min(h - 1);
    for row in 0..=initial {
        fill(row, &mut ring)?
    }
    let mut loaded = initial;
    let mut columns = Vec::new();
    if s.is_none() {
        columns.try_reserve_exact(w).map_err(|_| E::Allocation)?;
        if columns.capacity() != w {
            return Err(E::Allocation);
        }
        columns.resize(w, Pixel::default());
        for dy in -(i64::from(r))..=i64::from(r) {
            exec.check_cancelled()?;
            let row = dy.clamp(0, h as i64 - 1) as usize;
            for (x, c) in columns.iter_mut().enumerate() {
                if let Some(px) = ring.get((row % rows) * w + x) {
                    *c = c.plus(Pixel::load(*px))
                }
            }
        }
    }
    for y in 0..h {
        exec.check_cancelled()?;
        let need = (y + r as usize).min(h - 1);
        while loaded < need {
            loaded += 1;
            fill(loaded, &mut ring)?
        }
        let Some(dst) = out.get_mut(y * w * 4..(y + 1) * w * 4) else { return Err(E::BufferShape) };
        exec.pixels(dst, src.width(), |x, _| {
            let mut sum = Pixel::default();
            if s.is_some() {
                for (i, weight) in weights.iter().copied().enumerate() {
                    let row = (y as i64 + i as i64 - i64::from(r)).clamp(0, h as i64 - 1) as usize;
                    if let Some(px) = ring.get((row % rows) * w + x as usize) {
                        sum = sum.plus(Pixel::load(*px).scale(weight))
                    }
                }
            } else {
                sum = columns.get(x as usize).copied().unwrap_or_default().scale(1. / taps as f64)
            }
            post(src, b, x, y as u32, sum).straight().bytes()
        })?;
        if s.is_none() && y + 1 < h {
            let old = (y as i64 - i64::from(r)).clamp(0, h as i64 - 1) as usize;
            let new = (y + 1 + r as usize).min(h - 1);
            // Subtract the outgoing row before its ring slot can be reused.
            for (x, c) in columns.iter_mut().enumerate() {
                if let Some(px) = ring.get((old % rows) * w + x) {
                    *c = c.plus(Pixel::load(*px).scale(-1.))
                }
            }
            if new > loaded {
                loaded = new;
                fill(new, &mut ring)?
            }
            for (x, c) in columns.iter_mut().enumerate() {
                if let Some(px) = ring.get((new % rows) * w + x) {
                    *c = c.plus(Pixel::load(*px))
                }
            }
        }
    }
    Raster::output(src.width(), src.height(), out, l)
}
