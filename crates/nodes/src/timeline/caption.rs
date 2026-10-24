//! Captions (#574): text drawn over a Timeline's picture from a start to an end time. They are
//! stored with the clips (`captions` in [`super::Timeline`]) and follow the Layer Editor's text
//! rules: a font name the renderer doesn't have falls back to the bundled one.
use super::{MAX_US, bad, valid_id, yes};
use nodecraft_types::Error;
use serde::{Deserialize, Serialize};

/// Most captions on a timeline.
pub const MAX_CAPTIONS: usize = 64;
/// Longest caption text, in bytes, and most lines in it.
pub const MAX_CAPTION_BYTES: usize = 400;
pub const MAX_CAPTION_LINES: usize = 8;
/// Longest font name, in bytes (the Layer Editor's).
pub const MAX_FONT_BYTES: usize = 128;
/// The text size range and default, in output pixels.
pub const MIN_CAPTION_SIZE: u32 = 8;
pub const MAX_CAPTION_SIZE: u32 = 400;
pub const DEFAULT_CAPTION_SIZE: u32 = 48;
/// Widest outline around the letters, in output pixels.
pub const MAX_CAPTION_STROKE: u32 = 32;
/// The bundled font, and what any font name falls back to.
pub const CAPTION_FONT: &str = "lato";

/// Where a caption's lines sit across the frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

/// Where a caption sits up and down the frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Top,
    Middle,
    #[default]
    Bottom,
}

/// One caption: `text` shown from `start_us` until `end_us`, in microseconds from the start of
/// the output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caption {
    pub id: String,
    /// Lines break only at newlines.
    pub text: String,
    pub start_us: u64,
    pub end_us: u64,
    #[serde(default = "builtin_font")]
    pub font: String,
    /// The font size, in output pixels.
    #[serde(default = "default_size")]
    pub size: u32,
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub position: Position,
    /// `#rrggbb` or `#rrggbbaa`.
    #[serde(default = "white")]
    pub color: String,
    /// The outline's colour, drawn when `stroke_width` is over 0.
    #[serde(default = "black")]
    pub stroke: String,
    /// How far the outline reaches past each letter, in output pixels.
    #[serde(default)]
    pub stroke_width: u32,
    /// A disabled caption stays on the timeline but isn't drawn.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn builtin_font() -> String {
    CAPTION_FONT.into()
}

fn default_size() -> u32 {
    DEFAULT_CAPTION_SIZE
}

fn white() -> String {
    "#ffffff".into()
}

fn black() -> String {
    "#000000".into()
}

/// Whether `text` is `#rrggbb` or `#rrggbbaa`.
pub fn valid_color(text: &str) -> bool {
    text.strip_prefix('#').is_some_and(|hex| (hex.len() == 6 || hex.len() == 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl Caption {
    /// `text` from `start_us` to `end_us`, white at the bottom centre with no outline.
    pub fn new(id: String, text: String, start_us: u64, end_us: u64) -> Self {
        Self {
            id,
            text,
            start_us,
            end_us,
            font: builtin_font(),
            size: DEFAULT_CAPTION_SIZE,
            align: Align::default(),
            position: Position::default(),
            color: white(),
            stroke: black(),
            stroke_width: 0,
            enabled: true,
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        if !valid_id(&self.id) {
            return Err(bad("a caption id is 1 to 64 letters, digits, - or _"));
        }
        if self.text.trim().is_empty() || self.text.len() > MAX_CAPTION_BYTES {
            return Err(bad("a caption's text is 1 to 400 bytes"));
        }
        if self.text.chars().any(|c| c.is_control() && c != '\n') {
            return Err(bad("a caption's text has no control characters but line breaks"));
        }
        if self.text.split('\n').count() > MAX_CAPTION_LINES {
            return Err(bad("a caption has at most 8 lines"));
        }
        if self.start_us > MAX_US || self.end_us > MAX_US {
            return Err(bad("a caption's times are within an hour"));
        }
        if self.end_us <= self.start_us {
            return Err(bad("a caption ends after it starts"));
        }
        if self.font.is_empty() || self.font.len() > MAX_FONT_BYTES || self.font.chars().any(char::is_control) {
            return Err(bad("a caption's font is a name of 1 to 128 bytes"));
        }
        if !(MIN_CAPTION_SIZE..=MAX_CAPTION_SIZE).contains(&self.size) {
            return Err(bad("a caption's size is 8 to 400 pixels"));
        }
        if !valid_color(&self.color) || !valid_color(&self.stroke) {
            return Err(bad("a caption's colours are #rrggbb or #rrggbbaa"));
        }
        if self.stroke_width > MAX_CAPTION_STROKE {
            return Err(bad("a caption's outline is 0 to 32 pixels"));
        }
        Ok(())
    }

    /// A caption id none of `captions` has: `caption-1`, `caption-2`, ….
    pub fn fresh_id(captions: &[Caption]) -> String {
        (1..=u32::MAX).map(|n| format!("caption-{n}")).find(|id| !captions.iter().any(|c| c.id == *id)).unwrap_or_else(|| "caption".into())
    }
}
