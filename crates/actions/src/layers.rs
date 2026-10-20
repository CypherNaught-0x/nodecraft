//! The layer editor's compositor (#101): image, text and shape layers drawn bottom first into one
//! frame. Layers are stored as JSON in the node (`{"layers": [...]}`); `Layers::parse` checks them
//! and `composite` draws them with `vello_cpu`, so the canvas preview and a run produce the same
//! pixels. Geometry is in frame pixels with the origin top left: a layer is a box centred on
//! `(x, y)`, `width` × `height` px, turned `rotation` degrees clockwise about its centre and
//! mirrored by `flip_x`/`flip_y`. Only visible layers are drawn.
//!
//! Text is shaped with `harfrust` and drawn from `skrifa` outlines. The bundled font is Lato
//! (OFL-1.1, `assets/fonts`); a layer naming a font that isn't available falls back to it.
use crate::image::{ImageError, ImageLimits, Raster};
use serde::{Deserialize, Serialize};
use vello_cpu::kurbo::{Affine, BezPath, Ellipse, Rect, Shape as _, Stroke};
use vello_cpu::peniko::{Extend, ImageQuality, ImageSampler};
use vello_cpu::{Level, Pixmap, RenderContext, RenderSettings, Resources, color};

mod text;
pub use text::{BUILTIN_FONT, text_size};

/// Most layers one node holds.
pub const MAX_LAYERS: usize = 64;
/// Longest stored layer list, encoded.
pub const MAX_ENCODED_BYTES: usize = 256 * 1024;
/// Longest text one layer holds, in bytes.
pub const MAX_TEXT_BYTES: usize = 4096;
/// Longest layer id and name, in bytes.
pub const MAX_ID_BYTES: usize = 64;
pub const MAX_NAME_BYTES: usize = 128;
/// The frame's edge range, in pixels.
pub const MIN_FRAME: u32 = 100;
pub const MAX_FRAME: u32 = 4096;
/// How far from the frame a layer's position and size may go, in pixels.
pub const MAX_COORD: f64 = 100_000.0;
/// Text size range, in pixels.
pub const MIN_TEXT_SIZE: f64 = 1.0;
pub const MAX_TEXT_SIZE: f64 = 1000.0;
/// Most a shape's outline may be, in pixels.
pub const MAX_STROKE: f64 = 1000.0;

/// The layers of one node, bottom first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layers {
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Stable across moves and renames; letters, digits, `-` and `_`.
    pub id: String,
    pub name: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f64,
    /// The centre, in frame pixels.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Degrees, clockwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotation: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_x: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_y: bool,
    pub content: Content,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    /// The `nth` image connected from node `origin`, as stored in the node. Planning resolves it
    /// to `Image`, so a source keeps its layer when wires are reordered.
    Source { origin: String, nth: u32 },
    /// The `input`th image handed to `composite`.
    Image { input: u32 },
    Text {
        text: String,
        #[serde(default = "builtin_font")]
        font: String,
        size: f64,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        line_spacing: f64,
        /// Extra space after each character, in em.
        #[serde(default, skip_serializing_if = "is_zero")]
        tracking: f64,
        #[serde(default)]
        align: Align,
        color: String,
    },
    Shape {
        shape: ShapeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<String>,
        #[serde(default, skip_serializing_if = "is_zero")]
        stroke_width: f64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Triangle,
}

fn yes() -> bool {
    true
}
fn one() -> f64 {
    1.0
}
fn builtin_font() -> String {
    BUILTIN_FONT.into()
}
fn is_true(v: &bool) -> bool {
    *v
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn is_one(v: &f64) -> bool {
    *v == 1.0
}
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn bad(what: &'static str) -> ImageError {
    ImageError::Parameter(what)
}

fn within(value: f64, min: f64, max: f64, what: &'static str) -> Result<(), ImageError> {
    if value.is_finite() && (min..=max).contains(&value) { Ok(()) } else { Err(bad(what)) }
}

/// `#rrggbb` or `#rrggbbaa`, as straight-alpha RGBA.
pub fn parse_color(text: &str) -> Result<[u8; 4], ImageError> {
    let hex = text.strip_prefix('#').filter(|h| (h.len() == 6 || h.len() == 8) && h.is_ascii()).ok_or(bad("color"))?;
    let byte = |at: usize| hex.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()).ok_or(bad("color"));
    Ok([byte(0)?, byte(2)?, byte(4)?, if hex.len() == 8 { byte(6)? } else { 255 }])
}

impl Layers {
    /// Parses and checks stored or planned layers. Rejects oversized input before decoding it.
    pub fn parse(json: &str) -> Result<Self, ImageError> {
        if json.len() > MAX_ENCODED_BYTES {
            return Err(ImageError::RequestLimit);
        }
        let layers: Self = serde_json::from_str(json).map_err(|_| ImageError::RequestSyntax)?;
        layers.check()?;
        Ok(layers)
    }

    pub fn encode(&self) -> Result<String, ImageError> {
        self.check()?;
        let json = serde_json::to_string(self).map_err(|_| ImageError::RequestSyntax)?;
        if json.len() > MAX_ENCODED_BYTES { Err(ImageError::RequestLimit) } else { Ok(json) }
    }

    pub fn check(&self) -> Result<(), ImageError> {
        if self.layers.len() > MAX_LAYERS {
            return Err(bad("layers"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for layer in &self.layers {
            layer.check()?;
            if !ids.insert(layer.id.as_str()) {
                return Err(bad("layer id"));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }
}

/// Whether `id` can name a layer.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Layer {
    pub fn check(&self) -> Result<(), ImageError> {
        if !valid_id(&self.id) {
            return Err(bad("layer id"));
        }
        if self.name.len() > MAX_NAME_BYTES {
            return Err(bad("layer name"));
        }
        within(self.opacity, 0.0, 1.0, "opacity")?;
        within(self.x, -MAX_COORD, MAX_COORD, "x")?;
        within(self.y, -MAX_COORD, MAX_COORD, "y")?;
        within(self.width, f64::MIN_POSITIVE, MAX_COORD, "width")?;
        within(self.height, f64::MIN_POSITIVE, MAX_COORD, "height")?;
        within(self.rotation, -3600.0, 3600.0, "rotation")?;
        match &self.content {
            Content::Source { origin, .. } => {
                if origin.is_empty() || origin.len() > MAX_ID_BYTES {
                    return Err(bad("source"));
                }
            }
            Content::Image { .. } => {}
            Content::Text { text, font, size, line_spacing, tracking, color, .. } => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(bad("text"));
                }
                if font.len() > MAX_NAME_BYTES {
                    return Err(bad("font"));
                }
                within(*size, MIN_TEXT_SIZE, MAX_TEXT_SIZE, "text size")?;
                within(*line_spacing, 0.5, 5.0, "line spacing")?;
                within(*tracking, -1.0, 2.0, "tracking")?;
                parse_color(color)?;
            }
            Content::Shape { fill, stroke, stroke_width, .. } => {
                for color in fill.iter().chain(stroke) {
                    parse_color(color)?;
                }
                within(*stroke_width, 0.0, MAX_STROKE, "stroke width")?;
            }
        }
        Ok(())
    }

    /// Maps the layer's own box (`0..width`, `0..height`) into frame pixels.
    pub fn transform(&self) -> Affine {
        let flip = Affine::scale_non_uniform(if self.flip_x { -1.0 } else { 1.0 }, if self.flip_y { -1.0 } else { 1.0 });
        Affine::translate((self.x, self.y)) * Affine::rotate(self.rotation.to_radians()) * flip * Affine::translate((-self.width / 2.0, -self.height / 2.0))
    }
}

/// The frame layers are drawn into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA painted under every layer.
    pub background: [u8; 4],
}

impl Frame {
    /// The output's size when drawn at `scale`, checked against the frame and image limits.
    pub fn output_size(self, scale: f64, limits: ImageLimits) -> Result<(u16, u16), ImageError> {
        if !(MIN_FRAME..=MAX_FRAME).contains(&self.width) || !(MIN_FRAME..=MAX_FRAME).contains(&self.height) {
            return Err(ImageError::Dimensions);
        }
        within(scale, 1.0 / 64.0, 1.0, "scale")?;
        let edge = |v: u32| -> Result<u16, ImageError> {
            let scaled = (f64::from(v) * scale).round().max(1.0);
            // At most MAX_FRAME, so it fits u16.
            u16::try_from(scaled as u32).map_err(|_| ImageError::Dimensions)
        };
        let (w, h) = (edge(self.width)?, edge(self.height)?);
        let pixels = u64::from(w) * u64::from(h);
        if u32::from(w) > limits.edge || u32::from(h) > limits.edge || pixels > limits.pixels {
            return Err(ImageError::Dimensions);
        }
        Ok((w, h))
    }
}

/// Draws visible `layers` into `frame`, scaled by `scale` (1 for a run, less for a preview).
/// `images` are the planned image inputs, which `Content::Image` layers index; a layer whose
/// image is missing is an error, since planning drops unbound ones.
pub fn composite(frame: Frame, layers: &Layers, images: &[Raster], scale: f64, limits: ImageLimits) -> Result<Raster, ImageError> {
    limits.validate()?;
    layers.check()?;
    let (w, h) = frame.output_size(scale, limits)?;
    let out_bytes = u64::from(w) * u64::from(h) * 4;
    let visible: Vec<&Layer> = layers.layers.iter().filter(|l| l.visible && l.opacity > 0.0).collect();

    // Budgets before anything is allocated: the scene's pixmap and the output raster, plus a
    // premultiplied copy of each image drawn; one pass over the frame per layer, plus text
    // outlines and image conversion.
    let mut working = out_bytes.saturating_mul(2);
    let mut work = out_bytes / 4;
    let mut used = Vec::new();
    used.try_reserve_exact(images.len()).map_err(|_| ImageError::Allocation)?;
    used.resize(images.len(), false);
    for layer in &visible {
        work = work.saturating_add(out_bytes / 4);
        match &layer.content {
            Content::Image { input } => {
                let slot = used.get_mut(*input as usize).ok_or(bad("image layer"))?;
                if !*slot {
                    *slot = true;
                    let image = images.get(*input as usize).ok_or(bad("image layer"))?;
                    let bytes = u64::from(image.width()) * u64::from(image.height()) * 4;
                    working = working.saturating_add(bytes);
                    work = work.saturating_add(bytes / 4);
                }
            }
            Content::Text { text, .. } => work = work.saturating_add((text.len() as u64).saturating_mul(4096)),
            Content::Source { .. } => return Err(bad("unresolved source layer")),
            Content::Shape { .. } => {}
        }
    }
    if working > limits.working_bytes {
        return Err(ImageError::ScratchLimit);
    }
    if work > limits.work {
        return Err(ImageError::WorkLimit);
    }

    let mut pixmaps = Vec::new();
    pixmaps.try_reserve_exact(images.len()).map_err(|_| ImageError::Allocation)?;
    for (image, used) in images.iter().zip(&used) {
        pixmaps.push(if *used { Some(std::sync::Arc::new(premultiplied(image)?)) } else { None });
    }

    // The baseline SIMD level renders the same pixels on every machine.
    let mut ctx = RenderContext::new_with(w, h, RenderSettings { level: Level::baseline(), num_threads: 0 });
    let view = Affine::scale(scale);
    let [r, g, b, a] = frame.background;
    if a > 0 {
        ctx.set_paint(color::AlphaColor::<color::Srgb>::from_rgba8(r, g, b, a));
        ctx.fill_rect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
    }
    for layer in visible {
        let at = view * layer.transform();
        match &layer.content {
            Content::Image { input } => {
                let pixmap = pixmaps.get(*input as usize).and_then(Option::as_ref).ok_or(bad("image layer"))?;
                let (iw, ih) = (f64::from(pixmap.width()), f64::from(pixmap.height()));
                ctx.set_transform(at * Affine::scale_non_uniform(layer.width / iw, layer.height / ih));
                ctx.set_paint(vello_cpu::Image {
                    image: vello_cpu::ImageSource::Pixmap(pixmap.clone()),
                    sampler: ImageSampler { x_extend: Extend::Pad, y_extend: Extend::Pad, quality: ImageQuality::Medium, alpha: layer.opacity as f32 },
                });
                ctx.fill_rect(&Rect::new(0.0, 0.0, iw, ih));
            }
            Content::Text { color: c, .. } => {
                let path = text::outline(layer)?;
                ctx.set_transform(at);
                ctx.set_paint(paint(parse_color(c)?, layer.opacity));
                ctx.fill_path(&path);
            }
            Content::Shape { shape, fill, stroke, stroke_width } => {
                let path = shape_path(*shape, layer.width, layer.height);
                ctx.set_transform(at);
                // An opacity layer, so an outline over a fill doesn't double up where they meet.
                let faded = layer.opacity < 1.0;
                if faded {
                    ctx.push_opacity_layer(layer.opacity as f32);
                }
                if let Some(c) = fill.as_deref().map(parse_color).transpose()? {
                    ctx.set_paint(paint(c, 1.0));
                    ctx.fill_path(&path);
                }
                if let Some(c) = stroke.as_deref().map(parse_color).transpose()?
                    && *stroke_width > 0.0
                {
                    ctx.set_paint(paint(c, 1.0));
                    ctx.set_stroke(Stroke::new(*stroke_width));
                    ctx.stroke_path(&path);
                }
                if faded {
                    ctx.pop_layer();
                }
            }
            Content::Source { .. } => return Err(bad("unresolved source layer")),
        }
    }
    ctx.flush();
    let mut target = Pixmap::new(w, h);
    ctx.render(&mut target, &mut Resources::new());
    let mut pixels = crate::image::buffer(out_bytes as usize)?;
    for (out, px) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(target.take_unpremultiplied()) {
        out.copy_from_slice(&[px.r, px.g, px.b, px.a]);
    }
    Raster::new(u32::from(w), u32::from(h), pixels, limits)
}

fn paint(c: [u8; 4], opacity: f64) -> color::AlphaColor<color::Srgb> {
    let alpha = (f64::from(c[3]) * opacity).round().clamp(0.0, 255.0) as u8;
    color::AlphaColor::<color::Srgb>::from_rgba8(c[0], c[1], c[2], alpha)
}

/// A shape's outline in its layer's own box.
pub fn shape_path(shape: ShapeKind, width: f64, height: f64) -> BezPath {
    match shape {
        ShapeKind::Rectangle => Rect::new(0.0, 0.0, width, height).to_path(0.1),
        ShapeKind::Ellipse => Ellipse::from_rect(Rect::new(0.0, 0.0, width, height)).to_path(0.1),
        ShapeKind::Triangle => {
            let mut path = BezPath::new();
            path.move_to((width / 2.0, 0.0));
            path.line_to((width, height));
            path.line_to((0.0, height));
            path.close_path();
            path
        }
    }
}

fn premultiplied(image: &Raster) -> Result<Pixmap, ImageError> {
    let w = u16::try_from(image.width()).map_err(|_| ImageError::Dimensions)?;
    let h = u16::try_from(image.height()).map_err(|_| ImageError::Dimensions)?;
    let mut data = Vec::new();
    data.try_reserve_exact(usize::from(w) * usize::from(h)).map_err(|_| ImageError::Allocation)?;
    for &[r, g, b, a] in image.pixels().as_chunks::<4>().0 {
        let mul = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        data.push(color::PremulRgba8 { r: mul(r), g: mul(g), b: mul(b), a });
    }
    if data.len() != usize::from(w) * usize::from(h) {
        return Err(ImageError::BufferShape);
    }
    Ok(Pixmap::from_parts(data, w, h))
}

#[cfg(test)]
mod tests;
