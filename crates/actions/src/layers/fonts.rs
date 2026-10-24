//! Fonts a text layer can name (#548): the bundled Lato, and the fonts a project imported into a
//! Layer Editor node. A font is one TrueType or OpenType file, read with `skrifa`; its name is the
//! family it declares, with its style when that isn't Regular ("Lato Bold"). A layer naming a font
//! that isn't here draws with Lato, and [`Fonts::has`] says so.
use super::{MAX_NAME_BYTES, bad};
use crate::image::ImageError;
use skrifa::{FontRef, MetadataProvider, string::StringId};

/// The bundled font's name, and the fallback for any font that isn't available.
pub const BUILTIN_FONT: &str = "lato";
static LATO: &[u8] = include_bytes!("../../../../assets/fonts/Lato-Regular.ttf");

/// Largest font file a project imports, in bytes.
pub const MAX_FONT_BYTES: usize = 16 * 1024 * 1024;
/// Most fonts one Layer Editor node holds.
pub const MAX_FONTS: usize = 16;

/// What a font file is, read from its tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontInfo {
    /// The name layers use for it.
    pub name: String,
    /// `font/ttf` or `font/otf`.
    pub mime: &'static str,
}

/// Reads a font file without drawing it: one TrueType (`.ttf`) or OpenType (`.otf`) font with
/// outlines, a character map and a name. Collections, bitmap-only fonts and anything else are
/// refused.
pub fn font_info(bytes: &[u8]) -> Result<FontInfo, ImageError> {
    if bytes.len() > MAX_FONT_BYTES {
        return Err(bad("font: larger than 16 MiB"));
    }
    let mime = match bytes.get(..4) {
        Some([0, 1, 0, 0] | b"true") => "font/ttf",
        Some(b"OTTO") => "font/otf",
        Some(b"ttcf") => return Err(bad("font: a collection (.ttc); use one .ttf or .otf")),
        _ => return Err(bad("font: not a TrueType or OpenType file")),
    };
    guarded(|| {
        let font = FontRef::new(bytes).map_err(|_| bad("font: its tables can't be read"))?;
        if font.outline_glyphs().format().is_none() {
            return Err(bad("font: it has no outlines"));
        }
        if !font.charmap().has_map() {
            return Err(bad("font: it maps no characters"));
        }
        Ok(FontInfo { name: name(&font).ok_or(bad("font: it has no name"))?, mime })
    })
}

/// Runs `read`, which parses or draws an imported font, refusing the font as damaged if the
/// font libraries panic on it (#705). They are meant to return errors, but a font is hostile
/// input, and one bounds check they miss must not take the app down. Native builds only: in
/// the browser a panic aborts, so a fix belongs upstream too.
pub(super) fn guarded<T>(read: impl FnOnce() -> Result<T, ImageError>) -> Result<T, ImageError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)).unwrap_or_else(|_| Err(bad("font: it is damaged and can't be drawn")))
}

/// The first string `ids` names that has text, English where there is one, without control
/// characters and trimmed.
fn string(font: &FontRef<'_>, ids: &[StringId]) -> Option<String> {
    ids.iter().find_map(|id| {
        let text: String = font.localized_strings(*id).english_or_first()?.chars().filter(|c| !c.is_control()).collect();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    })
}

/// "Family Style", or "Family" for the regular style, cut to [`MAX_NAME_BYTES`] at a character.
fn name(font: &FontRef<'_>) -> Option<String> {
    let family = string(font, &[StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME])?;
    let style = string(font, &[StringId::TYPOGRAPHIC_SUBFAMILY_NAME, StringId::SUBFAMILY_NAME]);
    let mut name = match style {
        Some(style) if !style.eq_ignore_ascii_case("regular") && !family.ends_with(&style) => format!("{family} {style}"),
        _ => family,
    };
    while name.len() > MAX_NAME_BYTES {
        name.pop();
    }
    Some(name)
}

/// Whether two font names are the same font: case and surrounding space don't matter.
pub fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The fonts a render can draw with, beyond the bundled one. Built per render from bytes the
/// caller holds; adding a font only reads its table directory.
#[derive(Default)]
pub struct Fonts<'a> {
    faces: Vec<(String, FontRef<'a>)>,
}

impl<'a> Fonts<'a> {
    /// Adds `bytes` as the font `name`. A later font of the same name replaces it.
    pub fn add(&mut self, name: &str, bytes: &'a [u8]) -> Result<(), ImageError> {
        if bytes.len() > MAX_FONT_BYTES {
            return Err(bad("font: larger than 16 MiB"));
        }
        if self.faces.len() >= MAX_FONTS {
            return Err(bad("font: at most 16 per Layer Editor"));
        }
        let font = guarded(|| FontRef::new(bytes).map_err(|_| bad("font: its tables can't be read")))?;
        self.faces.retain(|(n, _)| !same_name(n, name));
        self.faces.push((name.to_owned(), font));
        Ok(())
    }

    /// Whether `name` draws with its own font rather than falling back to Lato.
    pub fn has(&self, name: &str) -> bool {
        same_name(name, BUILTIN_FONT) || self.faces.iter().any(|(n, _)| same_name(n, name))
    }

    /// The font `name` draws with: the project's of that name, else Lato.
    pub(super) fn resolve(&self, name: &str) -> Result<FontRef<'a>, ImageError> {
        match self.faces.iter().find(|(n, _)| same_name(n, name)) {
            Some((_, font)) => Ok(font.clone()),
            None => FontRef::new(LATO).map_err(|_| bad("font")),
        }
    }
}
