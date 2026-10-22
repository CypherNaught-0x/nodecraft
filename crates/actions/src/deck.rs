//! Decks (#122): slides, each a stack of layers in the Layer Editor's format (see `layers`), kept
//! as JSON in a Deck card (`{"version": 1, "slides": [...]}`). `Deck::parse` reads what was
//! stored leniently: a layer this version can't read is kept, unsupported, and fields it doesn't
//! know are kept too, so editing a deck never drops data. A deck from a newer version is read
//! only. `Deck::encode` stores what the deck commands checked.
//!
//! A slide's source layers name canvas nodes; `drawable` turns them into what that node shows
//! now (an image or its text), so a slide follows its sources.
use crate::image::ImageError;
use crate::layers::{self, Align, Content, Frame, Layer, MAX_ID_BYTES, MAX_LAYERS, MAX_NAME_BYTES, valid_id};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// The deck format this build reads and writes.
pub const VERSION: u64 = 1;
/// Most slides one deck holds.
pub const MAX_SLIDES: usize = 256;
/// Longest stored deck, encoded.
pub const MAX_ENCODED_BYTES: usize = 1024 * 1024;
/// The size a card is laid out at when a field becomes a slide, in canvas units.
pub const CARD_SIZE: (f64, f64) = (320.0, 240.0);
/// How much of the frame a made slide leaves clear at each edge.
const MARGIN: f64 = 0.05;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Deck {
    #[serde(default = "version")]
    pub version: u64,
    #[serde(default)]
    pub slides: Vec<Slide>,
    /// Fields a newer version added, kept as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Deck {
    fn default() -> Self {
        Self { version: VERSION, slides: Vec::new(), extra: Map::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slide {
    /// Stable across moves and renames; letters, digits, `-` and `_`.
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Bottom first.
    #[serde(default)]
    pub layers: Vec<Item>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A stored layer: one this version reads, or one it keeps as it is without drawing it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Item {
    Layer(Layer),
    Unsupported(Value),
}

impl<'de> Deserialize<'de> for Item {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        Ok(match serde_json::from_value::<Layer>(raw.clone()) {
            Ok(layer) if usable(&layer).is_ok() => Self::Layer(layer),
            _ => Self::Unsupported(raw),
        })
    }
}

impl Item {
    /// The layer's id: an unsupported layer's, when it has a string one.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Layer(layer) => Some(&layer.id),
            Self::Unsupported(raw) => raw.get("id").and_then(Value::as_str),
        }
    }

    pub fn layer(&self) -> Option<&Layer> {
        match self {
            Self::Layer(layer) => Some(layer),
            Self::Unsupported(_) => None,
        }
    }
}

fn version() -> u64 {
    VERSION
}

fn bad(what: &'static str) -> ImageError {
    ImageError::Parameter(what)
}

/// Whether a deck may hold `layer`: a checked layer, not an `Image` (that names a run's input,
/// which a deck has none of).
pub fn usable(layer: &Layer) -> Result<(), ImageError> {
    layer.check()?;
    if matches!(layer.content, Content::Image { .. }) { Err(bad("a deck's image layer names its node (kind \"source\")")) } else { Ok(()) }
}

impl Deck {
    /// Reads a stored deck. Rejects oversized input before decoding it; a layer that can't be
    /// read is kept as unsupported, and a deck from a newer version keeps only its version and
    /// fields (it is read only, see `writable`).
    pub fn parse(json: &str) -> Result<Self, ImageError> {
        if json.len() > MAX_ENCODED_BYTES {
            return Err(ImageError::RequestLimit);
        }
        let raw: Value = serde_json::from_str(json).map_err(|_| ImageError::RequestSyntax)?;
        let stored = raw.get("version").map_or(Some(VERSION), Value::as_u64).ok_or(bad("deck version"))?;
        if stored > VERSION {
            let Value::Object(mut extra) = raw else { return Err(ImageError::RequestSyntax) };
            extra.remove("version");
            return Ok(Self { version: stored, slides: Vec::new(), extra });
        }
        let deck: Self = serde_json::from_value(raw).map_err(|_| ImageError::RequestSyntax)?;
        deck.check()?;
        Ok(deck)
    }

    /// Whether this build may change the deck: not one from a newer version.
    pub fn writable(&self) -> bool {
        self.version <= VERSION
    }

    pub fn encode(&self) -> Result<String, ImageError> {
        if !self.writable() {
            return Err(bad("deck version"));
        }
        self.check()?;
        let json = serde_json::to_string(self).map_err(|_| ImageError::RequestSyntax)?;
        if json.len() > MAX_ENCODED_BYTES { Err(ImageError::RequestLimit) } else { Ok(json) }
    }

    pub fn check(&self) -> Result<(), ImageError> {
        if self.slides.len() > MAX_SLIDES {
            return Err(bad("slide count: a deck holds at most 256 slides"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for slide in &self.slides {
            slide.check()?;
            if !ids.insert(slide.id.as_str()) {
                return Err(bad("slide id"));
            }
        }
        Ok(())
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.slides.iter().position(|s| s.id == id)
    }

    /// A slide id not in use: `slide-n`.
    pub fn fresh_id(&self) -> String {
        (1..=self.slides.len().saturating_add(1)).map(|n| format!("slide-{n}")).find(|id| self.index_of(id).is_none()).unwrap_or_else(|| "slide".into())
    }
}

impl Slide {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into(), layers: Vec::new(), extra: Map::new() }
    }

    pub fn check(&self) -> Result<(), ImageError> {
        if !valid_id(&self.id) {
            return Err(bad("slide id"));
        }
        if self.name.len() > MAX_NAME_BYTES {
            return Err(bad("slide name"));
        }
        if self.layers.len() > MAX_LAYERS {
            return Err(bad("layer count: a slide holds at most 64 layers"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for item in &self.layers {
            if let Item::Layer(layer) = item {
                usable(layer)?;
            }
            if let Some(id) = item.id()
                && !ids.insert(id)
            {
                return Err(bad("layer id"));
            }
        }
        Ok(())
    }

    pub fn layer_index(&self, id: &str) -> Option<usize> {
        self.layers.iter().position(|item| item.id() == Some(id))
    }
}

/// What a source layer's node shows now.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown<T> {
    Image(T),
    Text(String),
    /// Gone, or nothing to show yet.
    Missing,
}

/// A slide ready to draw: its visible layers, bottom first, with each source layer that shows an
/// image turned into an `Image` layer of `images` and one that shows text into a text layer. A
/// missing source stays a `Source` layer, drawn as an empty box.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawable<T> {
    pub layers: Vec<Layer>,
    pub images: Vec<T>,
    /// The source layers whose node shows nothing, by id.
    pub missing: Vec<String>,
}

/// `slide` as it draws now, each source looked up with `source(origin, nth)`.
pub fn drawable<T>(slide: &Slide, frame: Frame, mut source: impl FnMut(&str, u32) -> Shown<T>) -> Drawable<T> {
    let mut out = Drawable { layers: Vec::new(), images: Vec::new(), missing: Vec::new() };
    for layer in slide.layers.iter().filter_map(Item::layer).filter(|l| l.visible) {
        let Content::Source { origin, nth, fit } = &layer.content else {
            out.layers.push(layer.clone());
            continue;
        };
        let content = match source(origin, *nth) {
            Shown::Image(image) => {
                let input = u32::try_from(out.images.len()).unwrap_or(u32::MAX);
                out.images.push(image);
                Content::Image { input, fit: *fit }
            }
            Shown::Text(text) => text_content(&text, layer.width, layer.height, frame.background),
            Shown::Missing => {
                out.missing.push(layer.id.clone());
                layer.content.clone()
            }
        };
        out.layers.push(Layer { content, ..layer.clone() });
    }
    out
}

/// Black or white, whichever reads on `background`.
fn ink(background: [u8; 4]) -> &'static str {
    let [r, g, b, a] = background.map(f64::from);
    let light = (0.2126 * r + 0.7152 * g + 0.0722 * b) * a / 255.0 + 255.0 - a;
    if light > 140.0 { "#000000" } else { "#ffffff" }
}

/// A text source's layer: the text wrapped at word boundaries to the box and sized so the lines
/// fit in it, from an estimate of Lato's average advance.
fn text_content(text: &str, width: f64, height: f64, background: [u8; 4]) -> Content {
    let text = clip(text.trim(), layers::MAX_TEXT_BYTES / 2);
    let mut size = (height / 4.0).min(width / 8.0).clamp(layers::MIN_TEXT_SIZE, layers::MAX_TEXT_SIZE);
    let mut wrapped = wrap(text, width, size);
    for _ in 0..24 {
        let lines = wrapped.lines().count().max(1) as f64;
        if lines * size * 1.25 <= height || size <= layers::MIN_TEXT_SIZE * 8.0 {
            break;
        }
        size = (size * 0.85).max(layers::MIN_TEXT_SIZE);
        wrapped = wrap(text, width, size);
    }
    Content::Text {
        text: clip(&wrapped, layers::MAX_TEXT_BYTES).into(),
        font: layers::BUILTIN_FONT.into(),
        size: size.round().clamp(layers::MIN_TEXT_SIZE, layers::MAX_TEXT_SIZE),
        line_spacing: 1.0,
        tracking: 0.0,
        align: Align::Left,
        color: ink(background).into(),
    }
}

/// `text` cut to at most `limit` bytes, at a character boundary.
fn clip(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// `text` with newlines added so no line runs past `width` at `size` (an estimate).
fn wrap(text: &str, width: f64, size: f64) -> String {
    let columns = ((width / (size * 0.5)).floor() as usize).max(1);
    let mut out = String::new();
    for (n, line) in text.lines().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut length = 0usize;
        for word in line.split_whitespace() {
            let chars = word.chars().count();
            if length > 0 && length.saturating_add(1).saturating_add(chars) > columns {
                out.push('\n');
                length = 0;
            } else if length > 0 {
                out.push(' ');
                length = length.saturating_add(1);
            }
            out.push_str(word);
            length = length.saturating_add(chars);
        }
    }
    out
}

/// A source layer for node `origin`, centred on `(x, y)` in a `width` × `height` box.
pub fn source_layer(origin: &str, name: &str, (x, y): (f64, f64), (width, height): (f64, f64)) -> Layer {
    Layer {
        id: layer_id(origin),
        name: clip(name, MAX_NAME_BYTES).into(),
        visible: true,
        locked: false,
        opacity: 1.0,
        x,
        y,
        width: width.max(1.0),
        height: height.max(1.0),
        rotation: 0.0,
        flip_x: false,
        flip_y: false,
        content: Content::Source { origin: clip(origin, MAX_ID_BYTES).into(), nth: 0, fit: true },
    }
}

fn layer_id(origin: &str) -> String {
    let id = format!("src-{origin}");
    if valid_id(&id) { id } else { "src".into() }
}

/// A slide showing one node, fitted to the frame.
pub fn node_slide(id: String, name: &str, origin: &str, frame: Frame) -> Slide {
    let (w, h) = (f64::from(frame.width), f64::from(frame.height));
    let mut slide = Slide::new(id, clip(name, MAX_NAME_BYTES));
    slide.layers.push(Item::Layer(source_layer(origin, name, (w / 2.0, h / 2.0), (w * (1.0 - 2.0 * MARGIN), h * (1.0 - 2.0 * MARGIN)))));
    slide
}

/// A slide showing a field's nodes as they sit on the canvas (`members`: each node's id, title
/// and top-left corner), scaled to fit under a title when the field has one.
pub fn field_slide(id: String, title: &str, members: &[(String, String, (f64, f64))], frame: Frame) -> Slide {
    let (w, h) = (f64::from(frame.width), f64::from(frame.height));
    let mut slide = Slide::new(id, clip(title, MAX_NAME_BYTES));
    let margin = w.min(h) * MARGIN;
    let mut top = margin;
    if !title.trim().is_empty() {
        let band = h * 0.12;
        let mut heading = source_layer("title", "Title", (w / 2.0, top + band / 2.0), (w - 2.0 * margin, band));
        heading.id = "title".into();
        heading.content = Content::Text {
            text: clip(title.trim(), layers::MAX_TEXT_BYTES).into(),
            font: layers::BUILTIN_FONT.into(),
            size: (band * 0.6).clamp(layers::MIN_TEXT_SIZE, layers::MAX_TEXT_SIZE).round(),
            line_spacing: 1.0,
            tracking: 0.0,
            align: Align::Center,
            color: ink(frame.background).into(),
        };
        slide.layers.push(Item::Layer(heading));
        top += band + margin / 2.0;
    }
    let finite = members.iter().filter(|(_, _, (x, y))| x.is_finite() && y.is_finite());
    let (cw, ch) = CARD_SIZE;
    let bounds = finite.clone().fold(None, |acc: Option<(f64, f64, f64, f64)>, (_, _, (x, y))| {
        let (l, t, r, b) = acc.unwrap_or((*x, *y, *x + cw, *y + ch));
        Some((l.min(*x), t.min(*y), r.max(*x + cw), b.max(*y + ch)))
    });
    let Some((left, upper, right, lower)) = bounds else { return slide };
    let (room_w, room_h) = ((w - 2.0 * margin).max(1.0), (h - top - margin).max(1.0));
    let scale = (room_w / (right - left).max(1.0)).min(room_h / (lower - upper).max(1.0));
    let (used_w, used_h) = ((right - left) * scale, (lower - upper) * scale);
    let (ox, oy) = (margin + (room_w - used_w) / 2.0, top + (room_h - used_h) / 2.0);
    for (origin, name, (x, y)) in finite.take(MAX_LAYERS.saturating_sub(slide.layers.len())) {
        let centre = (ox + (x - left + cw / 2.0) * scale, oy + (y - upper + ch / 2.0) * scale);
        let layer = source_layer(origin, name, centre, (cw * scale * 0.95, ch * scale * 0.95));
        if slide.layer_index(&layer.id).is_none() {
            slide.layers.push(Item::Layer(layer));
        }
    }
    slide
}

#[cfg(test)]
mod tests;
