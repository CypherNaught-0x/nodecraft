//! PDF documents (#113): page count, pages drawn as images and the text they show, through
//! `craft-pdf` (craft-libs' patched Hayro reader). Every entry point takes untrusted bytes and
//! is bounded by [`DocumentLimits`]; the reader's own panics are caught natively and reported as
//! errors.
//!
//! Text is what the pages draw, in content order. PDFs store glyphs at positions, not words and
//! lines, so spaces and line breaks are inferred from where each glyph sits; a scanned page draws
//! no glyphs and has no text (there is no OCR).
use crate::MediaError;
use craft_pdf::interpret::hayro_cmap::BfString;
use craft_pdf::interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask, TransformExt, font::Glyph,
};
use craft_pdf::raster::{self, RenderCache};
use craft_pdf::syntax::{DecryptionError, LoadPdfError, Pdf};
use kurbo::{Affine, BezPath, Point, Rect, Shape};

/// Bounds for reading one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentLimits {
    /// Largest file opened.
    pub max_input_bytes: usize,
    /// Longest side of a drawn page, in pixels.
    pub max_side: u32,
    /// Most text taken from one page, in bytes.
    pub max_page_text_bytes: usize,
}

impl DocumentLimits {
    /// The most any caller may ask for; [`DocumentLimits::default`] is this.
    pub const HARD: Self = Self { max_input_bytes: 256 * 1024 * 1024, max_side: 4096, max_page_text_bytes: 1024 * 1024 };
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self::HARD
    }
}

/// What a probe learns without drawing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentInfo {
    pub pages: u32,
}

/// A drawn page: straight (not premultiplied) RGBA8, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An opened PDF.
pub struct Document {
    pdf: Pdf,
    limits: DocumentLimits,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document").field("pages", &self.page_count()).finish()
    }
}

fn open_error(error: craft_pdf::OpenError) -> MediaError {
    match error {
        craft_pdf::OpenError::InputLimit { limit } => MediaError::LimitExceeded(format!("the PDF is larger than {limit} bytes")),
        craft_pdf::OpenError::Allocation => MediaError::LimitExceeded("there isn't enough memory to open the PDF".into()),
        craft_pdf::OpenError::Load(LoadPdfError::Decryption(DecryptionError::PasswordProtected)) => {
            MediaError::UnsupportedFeature("the PDF is password protected".into())
        }
        craft_pdf::OpenError::Load(LoadPdfError::Decryption(_)) => MediaError::UnsupportedFeature("the PDF's encryption isn't supported".into()),
        craft_pdf::OpenError::Load(LoadPdfError::Invalid) => MediaError::Decode("the PDF is damaged or not a PDF".into()),
        craft_pdf::OpenError::BackendPanic => MediaError::Decode("the PDF reader failed on this file".into()),
    }
}

fn raster_error(error: raster::Error) -> MediaError {
    match error {
        raster::Error::InvalidPage => MediaError::Decode("the page has no area to draw".into()),
        raster::Error::OutputLimit | raster::Error::Allocation => MediaError::LimitExceeded("the page is too large to draw".into()),
        raster::Error::BackendPanic => MediaError::Decode("the PDF renderer failed on this page".into()),
        raster::Error::Cancelled => MediaError::Decode("drawing the page was cancelled".into()),
        raster::Error::InvalidOptions | raster::Error::BufferLength => MediaError::InvalidParameter("page drawing options"),
    }
}

impl Document {
    /// Opens `bytes` as a PDF within `limits` (which can only lower [`DocumentLimits::HARD`]).
    pub fn open(bytes: &[u8], limits: &DocumentLimits) -> Result<Self, MediaError> {
        let hard = DocumentLimits::HARD;
        let limits = DocumentLimits {
            max_input_bytes: limits.max_input_bytes.min(hard.max_input_bytes),
            max_side: limits.max_side.clamp(1, hard.max_side),
            max_page_text_bytes: limits.max_page_text_bytes.min(hard.max_page_text_bytes),
        };
        if bytes.len() > limits.max_input_bytes {
            let as_u64 = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
            return Err(MediaError::TooLarge { size: as_u64(bytes.len()), limit: as_u64(limits.max_input_bytes) });
        }
        if !bytes.starts_with(b"%PDF-") {
            return Err(MediaError::Unrecognized);
        }
        let options = craft_pdf::OpenOptions { max_input_bytes: limits.max_input_bytes, ..Default::default() };
        let pdf = craft_pdf::open_slice_with_options(bytes, None, &options).map_err(open_error)?;
        Ok(Self { pdf, limits })
    }

    pub fn page_count(&self) -> u32 {
        u32::try_from(self.pdf.pages().len()).unwrap_or(u32::MAX)
    }

    fn page(&self, index: u32) -> Result<&craft_pdf::syntax::page::Page<'_>, MediaError> {
        let count = self.page_count();
        usize::try_from(index)
            .ok()
            .and_then(|i| self.pdf.pages().get(i))
            .ok_or_else(|| MediaError::LimitExceeded(format!("page {} doesn't exist; the PDF has {count}", index.saturating_add(1))))
    }

    /// Page `index` (from 0) drawn so its longer side is `edge` pixels (at most the limit's
    /// `max_side`), over `background` (straight sRGB RGBA).
    pub fn render(&self, index: u32, edge: u32, background: [u8; 4]) -> Result<PageImage, MediaError> {
        let page = self.page(index)?;
        let (width, height) = page.render_dimensions();
        let longest = width.max(height);
        if !longest.is_finite() || longest <= 0.0 {
            return Err(MediaError::Decode("the page has no area to draw".into()));
        }
        let edge = edge.clamp(1, self.limits.max_side) as f32;
        let scale = edge / longest;
        let options = raster::Options { scale: [scale, scale], background, ..Default::default() };
        let drawn = raster::render(page, &RenderCache::new(), &InterpreterSettings::default(), &options).map_err(raster_error)?;
        let (width, height) = (drawn.width, drawn.height);
        let mut rgba = drawn.bytes().to_vec();
        // The renderer premultiplies; an opaque background leaves nothing to undo.
        if background[3] != 255 {
            for [r, g, b, a] in rgba.as_chunks_mut::<4>().0 {
                if *a != 0 && *a != 255 {
                    let undo = |c: u8| ((u16::from(c) * 255 + u16::from(*a) / 2) / u16::from(*a)).min(255) as u8;
                    (*r, *g, *b) = (undo(*r), undo(*g), undo(*b));
                }
            }
        }
        Ok(PageImage { width, height, rgba })
    }

    /// The text page `index` (from 0) draws, cut at the limit's `max_page_text_bytes` (at a
    /// character boundary). Empty for a page that draws no text, such as a scan.
    pub fn text(&self, index: u32) -> Result<String, MediaError> {
        let page = self.page(index)?;
        let cache = InterpreterCache::new();
        let (width, height) = page.render_dimensions();
        let bbox = Rect::new(0.0, 0.0, f64::from(width.max(1.0)), f64::from(height.max(1.0)));
        let mut context = Context::new(page.initial_transform(true).to_kurbo(), bbox, &cache, page.xref(), InterpreterSettings::default());
        let mut device = TextDevice::new(self.limits.max_page_text_bytes);
        craft_pdf::interpret_page(page, &mut context, &mut device).map_err(|_| MediaError::Decode("the PDF reader failed on this page".into()))?;
        Ok(device.finish())
    }
}

/// The page count of `bytes`, a PDF.
pub fn probe_document(bytes: &[u8], limits: &DocumentLimits) -> Result<DocumentInfo, MediaError> {
    Document::open(bytes, limits).map(|d| DocumentInfo { pages: d.page_count() })
}

/// Where the previous glyph ended, to tell a word gap or a new line from the next glyph.
#[derive(Clone, Copy)]
struct Pen {
    /// Baseline origin and right edge, in device space (y down).
    origin: Point,
    right: f64,
    em: f64,
}

/// Collects glyphs' Unicode text, inferring spaces and line breaks from their positions.
struct TextDevice {
    text: String,
    max_bytes: usize,
    full: bool,
    pen: Option<Pen>,
}

impl TextDevice {
    fn new(max_bytes: usize) -> Self {
        Self { text: String::new(), max_bytes, full: false, pen: None }
    }

    fn push(&mut self, s: &str) {
        for c in s.chars() {
            // Control characters other than line structure only garble the text.
            let c = if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c };
            if self.text.len().saturating_add(c.len_utf8()) > self.max_bytes {
                self.full = true;
                return;
            }
            self.text.push(c);
        }
    }

    fn ends_with_space(&self) -> bool {
        self.text.is_empty() || self.text.ends_with(char::is_whitespace)
    }

    fn finish(self) -> String {
        let mut lines: Vec<&str> = self.text.lines().map(str::trim_end).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}

impl<'a> Device<'a> for TextDevice {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine, _: &Paint<'a>, _: &GlyphDrawMode) {
        if self.full {
            return;
        }
        let Some(unicode) = glyph.as_unicode() else { return };
        let text = match unicode {
            BfString::Char(c) => c.to_string(),
            BfString::String(s) => s,
        };
        // Glyph space is 1000 units to the em.
        let full = transform * glyph_transform;
        let origin = full * Point::ORIGIN;
        let [_, _, c, d, _, _] = full.as_coeffs();
        let em = c.hypot(d) * 1000.0;
        if !(origin.x.is_finite() && origin.y.is_finite() && em.is_finite() && em > 0.0) {
            return;
        }
        let right = match glyph {
            Glyph::Outline(outline) => {
                let path = outline.outline();
                let empty = path.elements().is_empty();
                let bounds = (full * path).bounding_box();
                if empty || !bounds.x1.is_finite() { origin.x + em * 0.25 } else { bounds.x1.max(origin.x) }
            }
            Glyph::Type3(_) => origin.x + em * 0.5,
        };
        if let Some(pen) = self.pen {
            let size = em.max(pen.em);
            let new_line = (origin.y - pen.origin.y).abs() > size * 0.5 || origin.x < pen.origin.x - size;
            if new_line {
                self.push("\n");
            } else if origin.x - pen.right > size * 0.15 && !self.ends_with_space() && !text.starts_with(char::is_whitespace) {
                self.push(" ");
            }
        }
        self.push(&text);
        self.pen = Some(Pen { origin, right, em });
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

#[cfg(any(test, feature = "fixtures"))]
pub mod fixture;
#[cfg(test)]
mod tests;
