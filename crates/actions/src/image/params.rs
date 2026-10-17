//! Typed action-owned inventory; shared descriptor generation belongs to #162.
use serde::{Deserialize, Serialize};

macro_rules! enums { ($($name:ident { $($variant:ident),+ });+ $(;)?) => { $(
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum $name { $($variant),+ }
)+ }; }
enums! {
    Blend { Normal, Multiply, Screen, Overlay, Darken, Lighten, Difference };
    KeyMode { RemoveMatched, KeepMatched };
    Axis { Horizontal, Vertical, Both };
    Resampling { Nearest, Bilinear, Area };
    Fit { Stretch, Contain, Cover };
    RadialMode { Spin, Zoom };
    Aperture { Disk, Hexagon };
    Selection { Matched, Unmatched }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
impl Color {
    pub const TRANSPARENT: Self = Self { r: 0, g: 0, b: 0, a: 0 };
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255, a: 255 };
    pub const GREEN: Self = Self { r: 0, g: 255, b: 0, a: 255 };
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
impl Default for Point {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5 }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grade {
    pub warmth: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub brightness: f64,
}
impl Default for Grade {
    fn default() -> Self {
        Self { warmth: 0., contrast: 1., saturation: 1., brightness: 0. }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Preset {
    None {},
    Grayscale {},
    Sepia {},
    Noir {},
    Duotone { shadow: Color, highlight: Color },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub preset: Preset,
    pub intensity: f64,
}
impl Default for Filter {
    fn default() -> Self {
        Self { preset: Preset::None {}, intensity: 1. }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tint {
    pub color: Color,
    pub intensity: f64,
    pub blend: Blend,
}
impl Default for Tint {
    fn default() -> Self {
        Self { color: Color::WHITE, intensity: 0., blend: Blend::Normal }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    pub target: Color,
    pub tolerance: f64,
    pub softness: f64,
}
impl Default for Matcher {
    fn default() -> Self {
        Self { target: Color::GREEN, tolerance: 0.1, softness: 0.1 }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub matcher: Matcher,
    pub mode: KeyMode,
}
impl Default for Key {
    fn default() -> Self {
        Self { matcher: Matcher::default(), mode: KeyMode::RemoveMatched }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceColor {
    pub matcher: Matcher,
    pub replacement: Color,
    pub replace_alpha: bool,
}
impl Default for ReplaceColor {
    fn default() -> Self {
        Self { matcher: Matcher::default(), replacement: Color::BLACK, replace_alpha: false }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Ratio {
    Source {},
    Square {},
    Landscape4_3 {},
    Portrait3_4 {},
    Landscape16_9 {},
    Portrait9_16 {},
    Custom { numerator: u32, denominator: u32 },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Padding {
    Solid { color: Color },
    Blurred { sigma: f64 },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AspectMode {
    Crop {},
    Pad { padding: Padding },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aspect {
    pub ratio: Ratio,
    pub mode: AspectMode,
    pub anchor: Point,
}
impl Default for Aspect {
    fn default() -> Self {
        Self { ratio: Ratio::Source {}, mode: AspectMode::Crop {}, anchor: Point::default() }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotate {
    pub angle: f64,
    pub expand: bool,
    pub background: Color,
    pub resampling: Resampling,
}
impl Default for Rotate {
    fn default() -> Self {
        Self { angle: 0., expand: true, background: Color::TRANSPARENT, resampling: Resampling::Bilinear }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Crop {
    Full {},
    Pixels { x: u32, y: u32, width: u32, height: u32 },
    Normalized { x: f64, y: f64, width: f64, height: f64 },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResizeSize {
    Scale { factor: f64 },
    Exact { width: u32, height: u32, fit: Fit, anchor: Point },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resize {
    pub size: ResizeSize,
    pub resampling: Resampling,
}
impl Default for Resize {
    fn default() -> Self {
        Self { size: ResizeSize::Scale { factor: 1. }, resampling: Resampling::Bilinear }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Anchor {
    Center {},
    TopLeft {},
    Top {},
    TopRight {},
    Left {},
    Right {},
    BottomLeft {},
    Bottom {},
    BottomRight {},
    Custom { point: Point },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OverlaySize {
    RelativeWidth { fraction: f64 },
    Source {},
    Pixels { width: u32 },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub anchor: Anchor,
    pub offset_x: i32,
    pub offset_y: i32,
    pub size: OverlaySize,
    pub opacity: f64,
    pub resampling: Resampling,
}
impl Default for Overlay {
    fn default() -> Self {
        Self {
            anchor: Anchor::Center {},
            offset_x: 0,
            offset_y: 0,
            size: OverlaySize::RelativeWidth { fraction: 0.25 },
            opacity: 1.,
            resampling: Resampling::Bilinear,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Blur {
    Gaussian { sigma: f64 },
    Box { radius: u32 },
    Motion { length: f64, angle: f64, samples: u32 },
    Radial { mode: RadialMode, center: Point, amount: f64, samples: u32 },
    Bilateral { radius: u32, spatial_sigma: f64, range_sigma: f64 },
    Bokeh { radius: f64, samples: u32, aperture: Aperture },
    TiltShift { sigma: f64, focus: Point, angle: f64, band_width: f64, falloff: f64 },
    TargetColor { sigma: f64, matcher: Matcher, selection: Selection },
}
impl Default for Blur {
    fn default() -> Self {
        Self::Gaussian { sigma: 2. }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageRequest {
    #[serde(rename = "image.grade")]
    Grade { options: Grade },
    #[serde(rename = "image.filter")]
    Filter { options: Filter },
    #[serde(rename = "image.tint")]
    Tint { options: Tint },
    #[serde(rename = "image.key")]
    Key { options: Key },
    #[serde(rename = "image.replace_color")]
    ReplaceColor { options: ReplaceColor },
    #[serde(rename = "image.aspect")]
    Aspect { options: Aspect },
    #[serde(rename = "image.rotate")]
    Rotate { options: Rotate },
    #[serde(rename = "image.flip")]
    Flip { axis: Axis },
    #[serde(rename = "image.blur")]
    Blur { options: Blur },
    #[serde(rename = "image.crop")]
    Crop { options: Crop },
    #[serde(rename = "image.resize")]
    Resize { options: Resize },
    #[serde(rename = "image.overlay")]
    Overlay { options: Overlay },
    #[serde(rename = "image.duplicate")]
    Duplicate { count: u32 },
}
