//! Procedural artwork for the showcase documents.
//!
//! Every pixel here is computed: gradients, value noise, sine ridges, hashed grain. No external
//! images are read, so the committed artwork cannot carry someone else's copyright and a rerun of
//! the tool reproduces the same bytes. The renderer then imports these PNGs through the product's
//! own asset pipeline (`engine::import`), so the cards show genuine decoded previews.
#![forbid(unsafe_code)]

use image::RgbaImage;

/// Fixed seed: the artwork is deterministic, so the committed files are reproducible.
const SEED: u64 = 0x2f3a_11c7_9d5b_e071;

/// A stable in-unit float from two integers and a salt (splitmix64 finaliser).
fn hash(x: i64, y: i64, salt: u64) -> f32 {
    let mut h =
        SEED ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ salt.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    ((h >> 40) as f32) / ((1u64 << 24) as f32)
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn clamp01(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// Bilinearly interpolated lattice noise in `0.0..=1.0`.
fn noise(x: f32, y: f32, salt: u64) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (smoothstep(x - xi), smoothstep(y - yi));
    let (x0, y0) = (xi as i64, yi as i64);
    let a = mix(hash(x0, y0, salt), hash(x0 + 1, y0, salt), fx);
    let b = mix(hash(x0, y0 + 1, salt), hash(x0 + 1, y0 + 1, salt), fx);
    mix(a, b, fy)
}

/// Three octaves of that noise, normalised to `0.0..=1.0`.
fn fbm(x: f32, y: f32, salt: u64) -> f32 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    for octave in 0..3u64 {
        sum += noise(x * freq, y * freq, salt.wrapping_add(octave)) * amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / 0.875
}

/// A pixel written from floats in `0.0..=1.0` with a bounded grain on top.
fn put(pixels: &mut [u8], index: usize, rgb: [f32; 3], grain: f32) {
    let g = (grain * 255.0).round() as i32;
    for channel in 0..3 {
        if let Some(slot) = pixels.get_mut(index + channel) {
            let value = (rgb.get(channel).copied().unwrap_or(0.0) * 255.0).round() as i32 + g;
            *slot = value.clamp(0, 255) as u8;
        }
    }
    if let Some(slot) = pixels.get_mut(index + 3) {
        *slot = 255;
    }
}

/// `A quiet forest at dawn`: the picture the showcase prompt describes. A three-stop sky, a low
/// sun with a soft bloom, banded haze, three ridges and a hashed tree line. 16:9 so it fills a
/// card's preview band exactly.
pub fn dawn(width: u32, height: u32) -> RgbaImage {
    let w = i64::from(width);
    let h = i64::from(height);
    let mut pixels = vec![0u8; (width as usize).saturating_mul(height as usize).saturating_mul(4)];
    let sun_x = 0.68;
    let sun_y = 0.58;
    for y in 0..h {
        let v = (y as f32) / (h.max(1) as f32);
        for x in 0..w {
            let u = (x as f32) / (w.max(1) as f32);
            // Sky: night indigo down to a warm horizon.
            let top = [0.06f32, 0.07, 0.16];
            let mid = [0.36, 0.20, 0.33];
            let low = [0.94, 0.55, 0.24];
            let [mut r, mut g, mut b] = if v < 0.55 {
                let t = smoothstep(clamp01(v / 0.55));
                [mix(top[0], mid[0], t), mix(top[1], mid[1], t), mix(top[2], mid[2], t)]
            } else {
                let t = smoothstep(clamp01((v - 0.55) / 0.45));
                [mix(mid[0], low[0], t), mix(mid[1], low[1], t), mix(mid[2], low[2], t)]
            };
            // Sun disc plus a wide bloom, in screen-space distance.
            let dx = (u - sun_x) * 1.78;
            let dy = v - sun_y;
            let d = (dx * dx + dy * dy).sqrt();
            let bloom = (-d * 3.4).exp();
            let disc = clamp01((0.075 - d) * 26.0);
            r = clamp01(r + bloom * 0.34 + disc * 0.85);
            g = clamp01(g + bloom * 0.22 + disc * 0.74);
            b = clamp01(b + bloom * 0.13 + disc * 0.52);
            // Haze bands drifting with the noise field.
            let haze = fbm(u * 5.0 + 0.4, v * 2.2 - 1.0, 7) * clamp01(1.0 - v * 1.35);
            r = clamp01(r + haze * 0.10);
            g = clamp01(g + haze * 0.07);
            b = clamp01(b + haze * 0.09);
            // Three ridges, far to near: sine over noise, each darker.
            let mut ridge_shade = 0.0f32;
            for (layer, base) in [0.62f32, 0.72, 0.83].into_iter().enumerate() {
                let l = layer as u64;
                let ridge = base + (u * (7.0 + l as f32 * 4.0) + l as f32).sin() * 0.022 + fbm(u * (3.0 + l as f32 * 2.5), 0.5 + l as f32, 11 + l) * 0.05;
                if v > ridge {
                    ridge_shade = 1.0 + l as f32;
                }
            }
            let mut out = [r, g, b];
            if ridge_shade > 0.0 {
                // Nearest ridge gets a hashed tree line along its crest.
                let depth = ridge_shade / 3.0;
                let dark = [0.09 * (1.0 - depth), 0.10 * (1.0 - depth), 0.13 * (1.0 - depth * 0.8)];
                out = dark;
                if ridge_shade >= 3.0 {
                    let trunk = (u * 210.0).floor() as i64;
                    let spike = hash(trunk, 0, 23);
                    let crest = 0.83 + (u * 34.0).sin() * 0.022 + fbm(u * 3.0 * 3.5, 0.5 + 2.0, 11 + 2) * 0.05;
                    if v < crest + 0.012 + spike * 0.055 && ((u * 210.0) - (u * 210.0).floor() - 0.5).abs() < 0.18 + spike * 0.2 {
                        out = [dark[0] * 0.55, dark[1] * 0.55, dark[2] * 0.6];
                    }
                }
            }
            let grain = (hash(x, y, 31) - 0.5) * 0.018;
            put(&mut pixels, ((y * w + x) as usize) * 4, out, grain);
        }
    }
    match RgbaImage::from_raw(width, height, pixels) {
        Some(image) => image,
        None => RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])),
    }
}

/// Teal ribbons over a near-black field, with hashed stars: the accent palette of the app, as a
/// picture.
pub fn aurora(width: u32, height: u32) -> RgbaImage {
    let w = i64::from(width);
    let h = i64::from(height);
    let mut pixels = vec![0u8; (width as usize).saturating_mul(height as usize).saturating_mul(4)];
    for y in 0..h {
        let v = (y as f32) / (h.max(1) as f32);
        for x in 0..w {
            let u = (x as f32) / (w.max(1) as f32);
            let mut out = [0.03f32, 0.05, 0.07];
            for (band, tint) in [(0u64, [0.30f32, 0.85, 0.62]), (1, [0.24, 0.62, 0.86]), (2, [0.62, 0.44, 0.90])] {
                let centre =
                    0.30 + band as f32 * 0.17 + (u * (2.0 + band as f32) + band as f32 * 1.7).sin() * 0.09 + fbm(u * 3.0, band as f32 * 2.0, 41 + band) * 0.10;
                let width = 0.055 + 0.02 * (u * 5.0 + band as f32).cos().abs();
                let glow = (-((v - centre) / width).powi(2)).exp();
                let shimmer = 0.72 + 0.28 * (u * 40.0 + v * 6.0 + band as f32 * 2.0).sin();
                out = [
                    clamp01(out[0] + glow * tint[0] * shimmer * 0.85),
                    clamp01(out[1] + glow * tint[1] * shimmer * 0.85),
                    clamp01(out[2] + glow * tint[2] * shimmer * 0.85),
                ];
            }
            if hash(x, y, 53) > 0.9988 {
                out = [0.92, 0.95, 0.98];
            }
            let vignette = 1.0 - ((u - 0.5) * (u - 0.5) + (v - 0.5) * (v - 0.5)) * 0.55;
            out = [out[0] * vignette, out[1] * vignette, out[2] * vignette];
            let grain = (hash(x, y, 59) - 0.5) * 0.02;
            put(&mut pixels, ((y * w + x) as usize) * 4, out, grain);
        }
    }
    match RgbaImage::from_raw(width, height, pixels) {
        Some(image) => image,
        None => RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])),
    }
}

/// Concentric interference rings: a cold bloom that reads well in a small card preview.
pub fn bloom(width: u32, height: u32) -> RgbaImage {
    let w = i64::from(width);
    let h = i64::from(height);
    let mut pixels = vec![0u8; (width as usize).saturating_mul(height as usize).saturating_mul(4)];
    for y in 0..h {
        let v = (y as f32) / (h.max(1) as f32);
        for x in 0..w {
            let u = (x as f32) / (w.max(1) as f32);
            let dx = (u - 0.5) * 1.0;
            let dy = (v - 0.5) * 1.0;
            let r = (dx * dx + dy * dy).sqrt();
            let rings = 0.5 + 0.5 * (r * 22.0 - 0.6).cos();
            let falloff = (-r * 2.1).exp();
            let warp = fbm(u * 4.0, v * 4.0, 67) * 0.35;
            let cold = [0.20f32, 0.44, 0.86];
            let warm = [0.86, 0.36, 0.62];
            let t = clamp01(rings * (0.6 + warp));
            let out = [
                clamp01(mix(cold[0], warm[0], t) * falloff + 0.04),
                clamp01(mix(cold[1], warm[1], t) * falloff + 0.04),
                clamp01(mix(cold[2], warm[2], t) * falloff + 0.06),
            ];
            let grain = (hash(x, y, 71) - 0.5) * 0.015;
            put(&mut pixels, ((y * w + x) as usize) * 4, out, grain);
        }
    }
    match RgbaImage::from_raw(width, height, pixels) {
        Some(image) => image,
        None => RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])),
    }
}

/// The three plates as encoded PNG bytes, in the order the scene imports them. The importer
/// sniffs and probes each one, so the sizes here are only what the painters were asked for.
///
/// # Errors
/// Returns a message if an image cannot be encoded.
pub fn plates() -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for (name, painted) in [("plate-dawn.png", dawn(896, 504)), ("plate-aurora.png", aurora(512, 512)), ("plate-bloom.png", bloom(512, 512))] {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(painted)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .map_err(|error| format!("encode {name}: {error}"))?;
        out.push((name, bytes));
    }
    Ok(out)
}
