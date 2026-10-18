//! Fixed-kernel preparation; geometry and matcher decisions stay action-owned.
use super::stripe::StripePlan;
use super::*;
use nodecraft_actions::image::PreparedImageOperation;
#[derive(Clone)]
pub(super) struct Requirements {
    pub decisions: u64,
    pub visits: u64,
    pub scratch: u64,
    pub stages: u32,
    pub coefficient_visits: u64,
    pub stripes: Option<StripePlan>,
    pub extra_bytes: u64,
    pub extra_work: u64,
    pub packing_words: u64,
    pub float_bytes: u64,
    pub cover_bytes: u64,
    pub candidates: u32,
}
pub(super) struct Packed {
    pub params: [u32; 1024],
    pub stages: Vec<[u32; 8]>,
    pub decisions: Vec<u8>,
    pub visits: u64,
    pub coefficient_visits: u64,
    pub decision_words: u64,
}
fn add(a: u64, b: u64) -> Result<u64, GpuImageError> {
    a.checked_add(b).ok_or(GpuImageError::Resource)
}
fn mul(a: u64, b: u64) -> Result<u64, GpuImageError> {
    a.checked_mul(b).ok_or(GpuImageError::Resource)
}
fn base_requirements(p: &PreparedImageOperation<'_>) -> Result<Requirements, GpuImageError> {
    let l = p.layout();
    let pixels = mul(l.width.into(), l.height.into())?;
    let source = p.inputs().source;
    let source_pixels = mul(source.width().into(), source.height().into())?;
    let mut r = Requirements {
        decisions: 8,
        visits: 0,
        scratch: 16,
        stages: 1,
        coefficient_visits: 0,
        stripes: None,
        extra_bytes: 0,
        extra_work: 0,
        packing_words: 0,
        float_bytes: 16,
        cover_bytes: 4,
        candidates: 0,
    };
    match p.request() {
        ImageRequest::Key { .. } | ImageRequest::ReplaceColor { .. } | ImageRequest::Blur { options: Blur::TargetColor { .. } } => {
            r.decisions = mul(source_pixels, 8)?;
            r.visits = source_pixels;
        }
        ImageRequest::Resize { options: Resize { resampling: Resampling::Nearest, .. } } => {
            r.visits = add(l.width.into(), l.height.into())?;
            r.decisions = mul(r.visits, 4)?;
        }
        ImageRequest::Rotate { options } if options.resampling == Resampling::Nearest || (options.angle.rem_euclid(90.) == 0. && options.background.a == 0) => {
            r.visits = pixels;
            r.decisions = mul(pixels, 4)?;
        }
        ImageRequest::Overlay { options: Overlay { resampling: Resampling::Nearest, .. } } => {
            if let Some(rect) = l.overlay_rect {
                r.visits = add(rect[2] as u64, rect[3] as u64)?;
                r.decisions = mul(r.visits, 4)?;
            }
        }
        _ => {}
    }
    match p.request() {
        ImageRequest::Blur { options: Blur::Gaussian { .. } | Blur::Box { .. } | Blur::TiltShift { .. } | Blur::TargetColor { .. } } => {
            r.scratch = mul(pixels, 16)?;
            r.stages = 2;
        }
        ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { .. } }, .. } } => {
            r.scratch = mul(pixels, 20)?;
            r.stages = 4;
        }
        _ => {}
    }
    let gaussian_work = |sigma: f64| 2 * (2 * (sigma * 3.).ceil() as u64 + 1);
    r.coefficient_visits = match p.request() {
        ImageRequest::Blur { options } => match *options {
            Blur::Gaussian { sigma } | Blur::TiltShift { sigma, .. } | Blur::TargetColor { sigma, .. } => gaussian_work(sigma),
            Blur::Motion { samples, .. } | Blur::Radial { samples, .. } => u64::from(samples),
            Blur::Bokeh { samples, .. } => u64::from(samples - 1),
            Blur::Bilateral { radius, .. } => u64::from(2 * radius + 1).pow(2),
            Blur::Box { .. } => 0,
        },
        ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { sigma } }, .. } } => gaussian_work(*sigma),
        _ => 0,
    };
    r.visits = add(r.visits, r.coefficient_visits)?;
    r.decisions = r.decisions.max(8);
    Ok(r)
}
fn put(p: &mut [u32; 1024], i: usize, v: f64) {
    if let Some(slot) = p.get_mut(i) {
        *slot = (v as f32).to_bits();
    }
}
fn color(p: &mut [u32; 1024], i: usize, c: Color) {
    for (j, v) in [c.r, c.g, c.b, c.a].into_iter().enumerate() {
        put(p, i + j, f64::from(v) / 255.);
    }
}
fn rect(p: &mut [u32; 1024], i: usize, r: [f64; 4]) {
    for (j, v) in r.into_iter().enumerate() {
        put(p, i + j, v);
    }
}
fn resampling(r: Resampling) -> u32 {
    match r {
        Resampling::Nearest => 0,
        Resampling::Bilinear => 1,
        Resampling::Area => 2,
    }
}
fn gaussian(p: &mut [u32; 1024], sigma: f64) -> u64 {
    let radius = (sigma * 3.).ceil() as u32;
    p[31] = radius;
    let taps = 2 * radius + 1;
    let mut total = 0.;
    for i in 0..taps {
        let d = f64::from(i) - f64::from(radius);
        let w = if d == 0. { 1. } else { (-0.5 * (d / sigma).powi(2)).exp() };
        put(p, 64 + i as usize, w);
        total += w;
    }
    // Compute normalization in f64 rather than summing narrowed coefficients.
    for i in 0..taps {
        let d = f64::from(i) - f64::from(radius);
        let w = if d == 0. { 1. } else { (-0.5 * (d / sigma).powi(2)).exp() };
        put(p, 64 + i as usize, w / total);
    }
    u64::from(taps) * 2
}
pub(super) fn pack(p: &PreparedImageOperation<'_>, r: &Requirements) -> Result<Packed, GpuImageError> {
    let mut params = [0u32; 1024];
    let l = p.layout();
    let inputs = p.inputs();
    params[0] = l.width;
    params[1] = l.height;
    params[2] = inputs.source.width();
    params[3] = inputs.source.height();
    rect(&mut params, 24, l.source_rect);
    let mut data = Vec::new();
    data.try_reserve_exact(usize::try_from(r.decisions).map_err(|_| GpuImageError::Resource)?).map_err(|_| GpuImageError::Resource)?;
    if data.capacity() as u64 != r.decisions {
        return Err(GpuImageError::Resource);
    }
    let mut visits = 0u64;
    let mut decision_words = 0u64;
    let mut coefficient_visits = 0u64;
    if let Some(m) = p.prepare_matcher()? {
        visits = m.visits();
        for pair in m.values() {
            for v in pair {
                data.extend_from_slice(&v.to_le_bytes());
                decision_words = decision_words.saturating_add(1);
            }
        }
    }
    match p.request() {
        ImageRequest::Key { .. } => params[4] = 0,
        ImageRequest::ReplaceColor { options } => {
            params[4] = 1;
            color(&mut params, 8, options.replacement);
        }
        ImageRequest::Grade { options } => {
            params[4] = 2;
            for (i, v) in [options.warmth, options.brightness, options.contrast, options.saturation].into_iter().enumerate() {
                put(&mut params, 8 + i, v);
            }
        }
        ImageRequest::Filter { options } => {
            params[4] = 3;
            params[5] = match options.preset {
                Preset::None {} => 0,
                Preset::Grayscale {} => 1,
                Preset::Sepia {} => 2,
                Preset::Noir {} => 3,
                Preset::Duotone { shadow, highlight } => {
                    color(&mut params, 12, shadow);
                    color(&mut params, 16, highlight);
                    4
                }
            };
            put(&mut params, 8, options.intensity);
        }
        ImageRequest::Tint { options } => {
            params[4] = 4;
            params[5] = match options.blend {
                Blend::Normal => 0,
                Blend::Multiply => 1,
                Blend::Screen => 2,
                Blend::Overlay => 3,
                Blend::Darken => 4,
                Blend::Lighten => 5,
                Blend::Difference => 6,
            };
            color(&mut params, 8, options.color);
            put(&mut params, 12, options.intensity);
        }
        ImageRequest::Aspect { options } => {
            params[4] = 6;
            match options.mode {
                AspectMode::Crop {} => params[5] = 0,
                AspectMode::Pad { padding } => {
                    rect(&mut params, 28, l.source_rect);
                    match padding {
                        Padding::Solid { color: c } => {
                            params[5] = 1;
                            color(&mut params, 12, c);
                        }
                        Padding::Blurred { sigma } => {
                            params[5] = 2;
                            rect(&mut params, 24, p.background_cover_rect()?);
                            coefficient_visits += gaussian(&mut params, sigma);
                        }
                    }
                }
            }
        }
        ImageRequest::Rotate { options } => {
            params[4] = 7;
            params[5] = resampling(options.resampling);
            let (sn, cs) = p.rotation_trig().ok_or(GpuImageError::Resource)?;
            put(&mut params, 8, sn);
            put(&mut params, 9, cs);
            color(&mut params, 12, options.background);
            if let Some(indices) = p.nearest_rotation_indices()? {
                params[7] = 1;
                params[32] = u32::from(options.background.a == 0 && options.angle.rem_euclid(90.) == 0.);
                visits = indices.len() as u64;
                for v in indices {
                    data.extend_from_slice(&v.to_le_bytes());
                    decision_words = decision_words.saturating_add(1);
                }
            }
        }
        ImageRequest::Flip { axis } => {
            params[4] = 8;
            params[5] = match axis {
                Axis::Horizontal => 0,
                Axis::Vertical => 1,
                Axis::Both => 2,
            };
        }
        ImageRequest::Crop { .. } => params[4] = 9,
        ImageRequest::Resize { options } => {
            params[4] = 10;
            params[5] = resampling(options.resampling);
            if let Some((x, y)) = p.nearest_resize_axes()? {
                visits = (x.len() + y.len()) as u64;
                for v in x.into_iter().chain(y) {
                    data.extend_from_slice(&v.to_le_bytes());
                    decision_words = decision_words.saturating_add(1);
                }
            }
        }
        ImageRequest::Overlay { options } => {
            params[4] = 11;
            params[5] = resampling(options.resampling);
            put(&mut params, 8, options.opacity);
            let over = inputs.overlay.ok_or(GpuImageError::Resource)?;
            params[18] = over.width();
            params[19] = over.height();
            let bounds = l.overlay_rect.ok_or(GpuImageError::Resource)?;
            rect(&mut params, 28, bounds);
            if let Some((x, y)) = p.nearest_overlay_axes()? {
                visits = (x.len() + y.len()) as u64;
                for v in x.into_iter().chain(y) {
                    data.extend_from_slice(&v.to_le_bytes());
                    decision_words = decision_words.saturating_add(1);
                }
            }
        }
        ImageRequest::Blur { options } => {
            params[4] = 5;
            match *options {
                Blur::Gaussian { sigma } => {
                    params[5] = 0;
                    coefficient_visits += gaussian(&mut params, sigma);
                }
                Blur::Box { radius } => {
                    params[5] = 1;
                    params[31] = radius;
                }
                Blur::Motion { length, angle, samples } => {
                    params[5] = 2;
                    params[30] = samples;
                    let (sn, cs) = angle.to_radians().sin_cos();
                    for i in 0..samples {
                        coefficient_visits += 1;
                        let t = if samples == 1 { 0. } else { f64::from(i) / f64::from(samples - 1) - 0.5 };
                        put(&mut params, 64 + 2 * i as usize, t * length * cs);
                        put(&mut params, 65 + 2 * i as usize, t * length * sn);
                    }
                }
                Blur::Radial { mode, center, amount, samples } => {
                    params[5] = if mode == RadialMode::Spin { 3 } else { 4 };
                    params[30] = samples;
                    put(&mut params, 8, center.x * f64::from(inputs.source.width()) - 0.5);
                    put(&mut params, 9, center.y * f64::from(inputs.source.height()) - 0.5);
                    for i in 0..samples {
                        coefficient_visits += 1;
                        let t = if samples == 1 { 0.5 } else { f64::from(i) / f64::from(samples - 1) };
                        if mode == RadialMode::Spin {
                            let (sn, cs) = ((2. * t - 1.) * 90. * amount).to_radians().sin_cos();
                            put(&mut params, 64 + 2 * i as usize, sn);
                            put(&mut params, 65 + 2 * i as usize, cs);
                        } else {
                            put(&mut params, 64 + i as usize, 1. - amount * (1. - t));
                        }
                    }
                }
                Blur::Bilateral { radius, spatial_sigma, range_sigma } => {
                    params[5] = 5;
                    params[31] = radius;
                    put(&mut params, 8, range_sigma);
                    let taps = 2 * radius + 1;
                    for y in 0..taps {
                        for x in 0..taps {
                            coefficient_visits += 1;
                            let dx = f64::from(x) - f64::from(radius);
                            let dy = f64::from(y) - f64::from(radius);
                            put(&mut params, 64 + (y * taps + x) as usize, -0.5 * ((dx / spatial_sigma).powi(2) + (dy / spatial_sigma).powi(2)));
                        }
                    }
                }
                Blur::Bokeh { radius, samples, aperture } => {
                    params[5] = 6;
                    params[30] = samples;
                    for i in 1..samples {
                        coefficient_visits += 1;
                        let theta = f64::from(i) * std::f64::consts::PI * (3. - 5f64.sqrt());
                        let mut r = ((f64::from(i) - 0.5) / f64::from(samples - 1)).sqrt();
                        if aperture == Aperture::Hexagon {
                            let sector = std::f64::consts::PI / 3.;
                            r *= (std::f64::consts::PI / 6.).cos() / (theta.rem_euclid(sector) - sector / 2.).cos();
                        }
                        let (sn, cs) = theta.sin_cos();
                        put(&mut params, 64 + 2 * i as usize, radius * r * cs);
                        put(&mut params, 65 + 2 * i as usize, radius * r * sn);
                    }
                }
                Blur::TiltShift { sigma, focus, angle, band_width, falloff } => {
                    params[5] = 7;
                    coefficient_visits += gaussian(&mut params, sigma);
                    put(&mut params, 8, focus.x * f64::from(inputs.source.width()));
                    put(&mut params, 9, focus.y * f64::from(inputs.source.height()));
                    let (sn, cs) = angle.to_radians().sin_cos();
                    put(&mut params, 10, sn);
                    put(&mut params, 11, cs);
                    put(&mut params, 12, band_width);
                    put(&mut params, 13, falloff);
                }
                Blur::TargetColor { sigma, .. } => {
                    params[5] = 8;
                    coefficient_visits += gaussian(&mut params, sigma);
                }
            }
        }
        ImageRequest::Duplicate { .. } => return Err(GpuImageError::Resource),
    }
    if data.len() as u64 > r.decisions {
        return Err(GpuImageError::Resource);
    }
    data.resize(r.decisions as usize, 0);
    let mut stages = Vec::new();
    stages.try_reserve_exact(r.stages as usize).map_err(|_| GpuImageError::Resource)?;
    if stages.capacity() != r.stages as usize {
        return Err(GpuImageError::Resource);
    }
    let mut stage = |phase, start, rows, halo_start, halo_rows, dispatch_rows| stages.push([phase, start, rows, halo_start, halo_rows, dispatch_rows, 0, 0]);
    if let Some(stripes) = r.stripes {
        if params[4] == 6 {
            stage(0, 0, l.height, 0, l.height, l.height);
        }
        for i in 0..stripes.count {
            let s = stripes.stripe(i)?;
            let phase = if params[4] == 6 { 1 } else { 0 };
            stage(phase, s.start, s.rows, s.halo_start, s.halo_rows, s.halo_rows);
            stage(phase + 1, s.start, s.rows, s.halo_start, s.halo_rows, s.rows);
        }
        if params[4] == 6 {
            stage(3, 0, l.height, 0, l.height, l.height);
        }
    } else {
        stage(0, 0, l.height, 0, l.height, l.height);
    }
    if stages.len() != r.stages as usize {
        return Err(GpuImageError::Resource);
    }
    if coefficient_visits != r.coefficient_visits {
        return Err(GpuImageError::Resource);
    }
    Ok(Packed { params, stages, decisions: data, visits, coefficient_visits, decision_words })
}

// Additional hierarchical vector folds are charged beyond the shared per-sample reference
// work. Bounds include partial/fractional endpoint taps and unconditional tail folds.
fn area_extra_work(p: &PreparedImageOperation<'_>) -> Result<u64, GpuImageError> {
    let l = p.layout();
    let inputs = p.inputs();
    let scale = match p.request() {
        ImageRequest::Resize { options: Resize { resampling: Resampling::Area, .. } } => {
            Some((l.source_rect[2] / f64::from(l.width), l.source_rect[3] / f64::from(l.height)))
        }
        ImageRequest::Overlay { options: Overlay { resampling: Resampling::Area, .. } } => {
            let over = inputs.overlay.ok_or(GpuImageError::Resource)?;
            let r = l.overlay_rect.ok_or(GpuImageError::Resource)?;
            Some((f64::from(over.width()) / r[2], f64::from(over.height()) / r[3]))
        }
        _ => None,
    };
    let Some((sx, sy)) = scale else { return Ok(0) };
    let axis = |s: f64| if s > 1. { s.ceil() as u64 + 1 } else { 2 };
    let nx = axis(sx);
    let ny = axis(sy);
    let row_folds = mul(ny, add(nx / 32, 1)?)?;
    let final_folds = add(ny / 32, 2)?;
    mul(mul(u64::from(l.width), u64::from(l.height))?, add(add(row_folds, ny)?, final_folds)?)
}
/// Joint bounded search: no pixel/plane/parameter allocation before a whole candidate fits.
pub(super) fn select(p: &PreparedImageOperation<'_>, device: &wgpu::Limits, max_block: u32) -> Result<Requirements, GpuImageError> {
    let base = base_requirements(p)?;
    let layout = p.layout();
    let width = u64::from(layout.width);
    let height = u64::from(layout.height);
    let pixels = mul(width, height)?;
    let compound = matches!(p.request(), ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { .. } }, .. } });
    let radius = match p.request() {
        ImageRequest::Blur { options: Blur::Gaussian { sigma } | Blur::TiltShift { sigma, .. } | Blur::TargetColor { sigma, .. } } => {
            Some((sigma * 3.).ceil() as u32)
        }
        ImageRequest::Blur { options: Blur::Box { radius } } => Some(*radius),
        ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { sigma } }, .. } } => Some((sigma * 3.).ceil() as u32),
        _ => None,
    };
    let reference = p.reference_work()?;
    let mut candidates = 0u32;
    let limit = if radius.is_some() { max_block.clamp(1, 128).min(layout.height) } else { 1 };
    for block in (1..=limit).rev() {
        candidates += 1;
        let mut r = base.clone();
        r.candidates = candidates;
        r.stripes = radius.map(|radius| StripePlan::new(layout.height, block, radius)).transpose()?;
        r.stages = if let Some(s) = r.stripes {
            s.count.checked_mul(2).and_then(|n| n.checked_add(if compound { 2 } else { 0 })).ok_or(GpuImageError::Resource)?
        } else {
            1
        };
        r.float_bytes = if let Some(s) = r.stripes { s.float_bytes(layout.width)? } else { 16 };
        r.cover_bytes = if compound { mul(pixels, 4)? } else { 4 };
        r.scratch = add(r.float_bytes, r.cover_bytes)?;
        let decisions = if base.visits > base.coefficient_visits { base.decisions / 4 } else { 0 };
        r.packing_words = add(add(1024, mul(u64::from(r.stages), 8)?)?, decisions)?;
        let kernel = if let Some(s) = r.stripes {
            let taps = u64::from(2 * s.radius + 1);
            if matches!(p.request(), ImageRequest::Blur { options: Blur::Box { .. } }) {
                add(add(add(mul(s.sum_rows, add(mul(width, 2)?, taps)?)?, mul(pixels, 2)?)?, mul(mul(u64::from(s.count), width)?, taps)?)?, pixels)?
            } else {
                add(reference, mul(mul(s.sum_rows.checked_sub(height).ok_or(GpuImageError::Resource)?, width)?, taps)?)?
            }
        } else {
            add(reference, area_extra_work(p)?)?
        };
        r.extra_work = add(add(base.visits, r.packing_words)?, add(kernel, u64::from(r.stages))?.saturating_sub(reference))?;
        let inputs = p.inputs();
        let input = inputs.source.pixels().len() as u64;
        let overlay = inputs.overlay.map_or(4, |r| r.pixels().len() as u64);
        let output = mul(pixels, 4)?;
        r.extra_bytes = add(
            add(add(add(add(add(input, overlay)?, mul(output, 2)?)?, mul(r.decisions, 3)?)?, r.scratch)?, 12288)?,
            add(mul(u64::from(r.stages), 576)?, 4096)?,
        )?;
        let buffers = [input, overlay, output, r.decisions, r.float_bytes, r.cover_bytes, 4096];
        let dispatch_rows = if compound { layout.height } else { r.stripes.map_or(layout.height, |s| s.rows) };
        let caps = device.max_uniform_buffer_binding_size >= 32
            && device.max_storage_buffers_per_shader_stage >= 7
            && device.max_uniform_buffers_per_shader_stage >= 1
            && layout.width.div_ceil(8) <= device.max_compute_workgroups_per_dimension
            && dispatch_rows.div_ceil(8) <= device.max_compute_workgroups_per_dimension
            && buffers.into_iter().all(|n| n <= device.max_buffer_size && n <= device.max_storage_buffer_binding_size);
        if caps && p.admit_extra(r.extra_bytes, r.extra_work).is_ok() {
            return Ok(r);
        }
        if radius.is_none() {
            p.admit_extra(r.extra_bytes, r.extra_work)?;
            return Err(GpuImageError::Resource);
        }
    }
    Err(GpuImageError::Resource)
}

#[cfg(test)]
mod tests;
