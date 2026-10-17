use super::params::*;

#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Pixel {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}
pub(super) fn linear(v: u8) -> f64 {
    static LUT: std::sync::OnceLock<[f64; 256]> = std::sync::OnceLock::new();
    let table = LUT.get_or_init(|| {
        std::array::from_fn(|i| {
            let s = i as f64 / 255.;
            if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        })
    });
    table.get(usize::from(v)).copied().unwrap_or(0.)
}
fn byte(v: f64) -> u8 {
    (v.clamp(0., 1.) * 255.).round() as u8
}
fn encoded(v: f64) -> u8 {
    let v = v.clamp(0., 1.);
    byte(if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1. / 2.4) - 0.055 })
}
impl Pixel {
    pub fn from_color(c: Color) -> Self {
        Self { r: linear(c.r), g: linear(c.g), b: linear(c.b), a: f64::from(c.a) / 255. }
    }
    pub fn from_bytes(v: &[u8]) -> Self {
        let mut p = v.iter().copied();
        Self::from_color(Color { r: p.next().unwrap_or(0), g: p.next().unwrap_or(0), b: p.next().unwrap_or(0), a: p.next().unwrap_or(0) })
    }
    pub fn premult(self) -> Self {
        Self { r: self.r * self.a, g: self.g * self.a, b: self.b * self.a, a: self.a }
    }
    pub fn straight(self) -> Self {
        if self.a > 0. { Self { r: self.r / self.a, g: self.g / self.a, b: self.b / self.a, a: self.a } } else { Self::default() }
    }
    pub fn bytes(self) -> [u8; 4] {
        let alpha = byte(self.a);
        if alpha == 0 { [0; 4] } else { [encoded(self.r), encoded(self.g), encoded(self.b), alpha] }
    }
    pub fn scale(self, n: f64) -> Self {
        Self { r: self.r * n, g: self.g * n, b: self.b * n, a: self.a * n }
    }
    pub fn plus(self, p: Self) -> Self {
        Self { r: self.r + p.r, g: self.g + p.g, b: self.b + p.b, a: self.a + p.a }
    }
    pub fn mix(self, p: Self, t: f64) -> Self {
        self.scale(1. - t).plus(p.scale(t))
    }
    pub fn over(self, background: Self) -> Self {
        self.plus(background.scale(1. - self.a))
    }
    pub fn luma(self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
    pub fn stored(self) -> [f32; 4] {
        [self.r as f32, self.g as f32, self.b as f32, self.a as f32]
    }
    pub fn load(p: [f32; 4]) -> Self {
        let [r, g, b, a] = p;
        Self { r: r.into(), g: g.into(), b: b.into(), a: a.into() }
    }
}
pub(super) fn mask(p: Pixel, m: Matcher) -> f64 {
    let t = Pixel::from_color(m.target);
    let d = (((p.r - t.r).powi(2) + (p.g - t.g).powi(2) + (p.b - t.b).powi(2)) / 3.).sqrt();
    if d <= m.tolerance {
        1.
    } else if m.softness == 0. {
        0.
    } else {
        let end = (m.tolerance + m.softness).min(1.);
        ((end - d) / (end - m.tolerance)).clamp(0., 1.)
    }
}
pub(super) fn grade(p: Pixel, g: Grade) -> Pixel {
    let exposure = 2f64.powf(g.brightness);
    let rgb = Pixel {
        r: (p.r * 2f64.powf(g.warmth / 2.) * exposure - 0.18) * g.contrast + 0.18,
        g: (p.g * exposure - 0.18) * g.contrast + 0.18,
        b: (p.b * 2f64.powf(-g.warmth / 2.) * exposure - 0.18) * g.contrast + 0.18,
        a: p.a,
    };
    let y = rgb.luma();
    Pixel { r: y + g.saturation * (rgb.r - y), g: y + g.saturation * (rgb.g - y), b: y + g.saturation * (rgb.b - y), a: p.a }
}
pub(super) fn filter(p: Pixel, f: &Filter) -> Pixel {
    let y = p.luma();
    let q = match f.preset {
        Preset::None {} => p,
        Preset::Grayscale {} => Pixel { r: y, g: y, b: y, a: p.a },
        Preset::Sepia {} => Pixel { r: (1.15 * y).min(1.), g: y, b: 0.8 * y, a: p.a },
        Preset::Noir {} => {
            let v = ((y - 0.18) * 1.8 + 0.18).clamp(0., 1.);
            Pixel { r: v, g: v, b: v, a: p.a }
        }
        Preset::Duotone { shadow, highlight } => Pixel::from_color(shadow).mix(Pixel::from_color(highlight), y),
    };
    Pixel { a: p.a, ..p.mix(q, f.intensity) }
}
pub(super) fn tint(p: Pixel, t: Tint) -> Pixel {
    let c = Pixel::from_color(t.color);
    let blend = |s: f64, v: f64| match t.blend {
        Blend::Normal => v,
        Blend::Multiply => s * v,
        Blend::Screen => 1. - (1. - s) * (1. - v),
        Blend::Overlay => {
            if s <= 0.5 {
                2. * s * v
            } else {
                1. - 2. * (1. - s) * (1. - v)
            }
        }
        Blend::Darken => s.min(v),
        Blend::Lighten => s.max(v),
        Blend::Difference => (s - v).abs(),
    };
    p.mix(Pixel { r: blend(p.r, c.r), g: blend(p.g, c.g), b: blend(p.b, c.b), a: p.a }, t.intensity * c.a)
}
