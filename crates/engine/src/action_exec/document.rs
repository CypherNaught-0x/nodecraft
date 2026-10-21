//! PDF to images and PDF to text (#113): the connected PDF read within the media limits, a page
//! range at a time. Pages are drawn on the action worker and staged as PNG like any other image
//! result; text is what the pages draw (a scan has none, there is no OCR).
use super::{LocalActions, whole, word};
use nodecraft_actions::image::{ImageLimits, Raster};
use nodecraft_media::{Document, DocumentLimits};
use nodecraft_nodes::document::{MAX_IMAGE_PAGES, MAX_PAGE_SIDE, MAX_TEXT_PAGES, page_range};
use nodecraft_types::{ParamValues, Value};

/// Most text one run of PDF to text returns; the rest of the range is left out.
const MAX_TEXT_BYTES: usize = nodecraft_actions::text::MAX_OUTPUT_BYTES;

/// `#rrggbb` or `#rrggbbaa` as straight sRGB RGBA.
fn rgba(text: &str) -> Result<[u8; 4], String> {
    let json = super::color(text)?;
    let channel = |name: &str| json.get(name).and_then(serde_json::Value::as_u64).and_then(|v| u8::try_from(v).ok()).unwrap_or(255);
    Ok([channel("r"), channel("g"), channel("b"), channel("a")])
}

/// Appends `s` to `out` up to `MAX_TEXT_BYTES`, cut at a character boundary; false once full.
fn append(out: &mut String, s: &str) -> bool {
    let room = MAX_TEXT_BYTES.saturating_sub(out.len());
    if s.len() <= room {
        out.push_str(s);
        return true;
    }
    let mut cut = room;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    out.push_str(s.get(..cut).unwrap_or_default());
    false
}

impl LocalActions {
    fn document(&self, inputs: &[Value]) -> Result<Document, String> {
        let bytes = self.media_bytes(inputs.first(), "PDF")?;
        Document::open(&bytes, &DocumentLimits::default()).map_err(|e| format!("the PDF can't be read: {e}"))
    }

    fn pages(&self, params: &ParamValues, document: &Document, cap: u64) -> Result<std::ops::Range<u32>, String> {
        page_range(whole(params, "first")?, whole(params, "last")?, document.page_count(), cap)
    }

    /// Each page in the range as an image, in page order.
    pub(super) fn document_pages(&self, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        let size = u32::try_from(whole(params, "size")?.clamp(1, MAX_PAGE_SIDE)).map_err(|_| "the size is too large".to_owned())?;
        let background = rgba(word(params, "background")?)?;
        let document = self.document(inputs)?;
        let mut images = Vec::new();
        for index in self.pages(params, &document, MAX_IMAGE_PAGES)? {
            let page = document.render(index, size, background).map_err(|e| format!("page {} can't be drawn: {e}", index.saturating_add(1)))?;
            let raster = Raster::new(page.width, page.height, page.rgba, ImageLimits::default())
                .map_err(|e| format!("page {} can't be used: {e}", index.saturating_add(1)))?;
            images.push(self.stage(&raster)?);
        }
        Ok(Value::List(images))
    }

    /// The text of the pages in the range, one after another.
    pub(super) fn document_text(&self, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        let headings = match word(params, "separator")? {
            "blank" => false,
            "heading" => true,
            other => return Err(format!("{other} isn't a way to separate pages")),
        };
        let document = self.document(inputs)?;
        let mut out = String::new();
        for index in self.pages(params, &document, MAX_TEXT_PAGES)? {
            let text = document.text(index).map_err(|e| format!("page {} can't be read: {e}", index.saturating_add(1)))?;
            let mut part = String::new();
            if !out.is_empty() {
                part.push_str("\n\n");
            }
            if headings {
                part.push_str(&format!("[Page {}]\n", index.saturating_add(1)));
            }
            part.push_str(&text);
            if !append(&mut out, &part) {
                break;
            }
        }
        Ok(Value::Text(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_at_a_character_boundary_once_full() {
        let mut out = "a".repeat(MAX_TEXT_BYTES - 1);
        assert!(!append(&mut out, "éx"));
        assert_eq!(out.len(), MAX_TEXT_BYTES - 1, "half of é never lands");
        let mut out = String::new();
        assert!(append(&mut out, "short"));
        assert_eq!(rgba("#ff000080"), Ok([255, 0, 0, 128]));
        assert_eq!(rgba("#00ff00"), Ok([0, 255, 0, 255]));
        assert!(rgba("red").is_err());
    }
}
