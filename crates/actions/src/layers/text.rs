//! Text layers: shaped with `harfrust`, outlined with `skrifa`, laid out in the layer's own box.
//! Lines break only at newlines; each line is aligned within the box's width, and the first
//! line's top sits at the box's top.
use super::{Align, Content, Layer, bad, fonts::Fonts};
use crate::image::ImageError;
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use vello_cpu::kurbo::BezPath;

struct Pen<'a> {
    path: &'a mut BezPath,
    x: f64,
    y: f64,
}

impl Pen<'_> {
    fn at(&self, x: f32, y: f32) -> (f64, f64) {
        (self.x + f64::from(x), self.y - f64::from(y))
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.path.move_to(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.path.line_to(p);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.at(cx0, cy0), self.at(x, y));
        self.path.quad_to(c, p);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        self.path.curve_to(c0, c1, p);
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

struct Style<'a> {
    text: &'a str,
    font: &'a str,
    size: f64,
    line_spacing: f64,
    tracking: f64,
    align: Align,
}

fn style(layer: &Layer) -> Result<Style<'_>, ImageError> {
    match &layer.content {
        Content::Text { text, font, size, line_spacing, tracking, align, .. } => {
            Ok(Style { text, font, size: *size, line_spacing: *line_spacing, tracking: *tracking, align: *align })
        }
        _ => Err(bad("text layer")),
    }
}

struct Glyph {
    id: u32,
    x: f64,
    y: f64,
}

struct Line {
    glyphs: Vec<Glyph>,
    width: f64,
}

struct Laid {
    lines: Vec<Line>,
    ascent: f64,
    line_height: f64,
}

fn lay_out<'f>(style: &Style<'_>, fonts: &Fonts<'f>) -> Result<(skrifa::FontRef<'f>, Laid), ImageError> {
    let font = fonts.resolve(style.font)?;
    let size = style.size as f32;
    let metrics = font.metrics(Size::new(size), LocationRef::default());
    let units = f64::from(metrics.units_per_em.max(1));
    let px = style.size / units;
    let ascent = f64::from(metrics.ascent);
    let natural = f64::from(metrics.ascent - metrics.descent + metrics.leading);
    let line_height = natural * style.line_spacing;
    let shaper_data = harfrust::ShaperData::new(&font);
    let shaper = shaper_data.shaper(&font).instance(None).build();
    let mut lines = Vec::new();
    for text in style.text.split('\n') {
        let mut buffer = harfrust::UnicodeBuffer::new();
        buffer.push_str(text.trim_end_matches('\r'));
        buffer.guess_segment_properties();
        let shaped = shaper.shape(buffer, harfrust::ShapeOptions::new());
        let mut glyphs = Vec::new();
        let mut pen = 0.0;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            glyphs.push(Glyph { id: info.glyph_id, x: pen + f64::from(pos.x_offset) * px, y: f64::from(pos.y_offset) * px });
            pen += f64::from(pos.x_advance) * px + style.tracking * style.size;
        }
        // Tracking adds space between characters, not after the last one.
        let width = if glyphs.is_empty() { 0.0 } else { (pen - style.tracking * style.size).max(0.0) };
        lines.push(Line { glyphs, width });
    }
    Ok((font, Laid { lines, ascent, line_height }))
}

/// The box a text layer's text needs, drawn with its font from `fonts`: its widest line, and its
/// lines' height.
pub fn text_size(layer: &Layer, fonts: &Fonts<'_>) -> Result<(f64, f64), ImageError> {
    let (_, laid) = lay_out(&style(layer)?, fonts)?;
    let width = laid.lines.iter().map(|l| l.width).fold(0.0, f64::max);
    let count = laid.lines.len().max(1) as f64;
    Ok((width.ceil().max(1.0), (laid.line_height * count).ceil().max(1.0)))
}

/// A text layer's glyph outlines, in its own box.
pub(super) fn outline(layer: &Layer, fonts: &Fonts<'_>) -> Result<BezPath, ImageError> {
    let style = style(layer)?;
    let (font, laid) = lay_out(&style, fonts)?;
    let outlines = font.outline_glyphs();
    let settings = || DrawSettings::unhinted(Size::new(style.size as f32), LocationRef::default());
    let mut path = BezPath::new();
    for (row, line) in laid.lines.iter().enumerate() {
        let left = match style.align {
            Align::Left => 0.0,
            Align::Center => (layer.width - line.width) / 2.0,
            Align::Right => layer.width - line.width,
        };
        let baseline = laid.ascent + laid.line_height * row as f64;
        for glyph in &line.glyphs {
            let Some(outline) = outlines.get(skrifa::GlyphId::new(glyph.id)) else { continue };
            let mut pen = Pen { path: &mut path, x: left + glyph.x, y: baseline - glyph.y };
            // A glyph that won't draw is left out rather than failing the whole layer.
            let _ = outline.draw(settings(), &mut pen);
        }
    }
    Ok(path)
}
