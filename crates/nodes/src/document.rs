//! PDF documents (#113): the PDF source node, and the actions that read one: its pages drawn as
//! images, and the text they show. The source document is never changed.
use crate::SourceKind;
use crate::action::{ActionKind, choice, color, count, kind, port};
use nodecraft_types::Modality;

/// The PDF source kind.
pub const SOURCE: &str = "source.document";
/// The PDF to images action kind: one image per page in the range.
pub const PAGES: &str = "document.pages";
/// The PDF to text action kind: the text the pages in the range show.
pub const TEXT: &str = "document.text";
/// The highest page number a range names.
pub const MAX_PAGE: u64 = 100_000;
/// Most pages drawn by one run of PDF to images.
pub const MAX_IMAGE_PAGES: u64 = 64;
/// Most pages read by one run of PDF to text.
pub const MAX_TEXT_PAGES: u64 = 2_000;
/// The longest side of a drawn page.
pub const MAX_PAGE_SIDE: u64 = 4096;

/// A PDF, shown on its card as its first page and its page count.
pub fn kind_source() -> SourceKind {
    SourceKind::new(SOURCE, "PDF", "D", Modality::Document)
}

/// Page `first` through page `last` (from 1); a `last` of 0 is the document's last page.
fn range() -> Vec<nodecraft_types::ParamSpec> {
    vec![count("first", "First page", 1, MAX_PAGE, 1), count("last", "Last page (0 = the end)", 0, MAX_PAGE, 0)]
}

/// Each page in the range drawn as an image whose longer side is `size` pixels, over
/// `background` (white by default; transparent keeps the page's own transparency). `only` picks
/// every page, or just the ones that draw no text (scans), to send those to a vision model (#592).
pub fn pages_kind() -> ActionKind {
    let mut params = range();
    params.push(choice("only", "Pages", &[("all", "All pages"), ("without_text", "Pages without text (scans)")], "all"));
    params.push(count("size", "Size", 64, MAX_PAGE_SIDE, 1600));
    params.push(color("background", "Background", "#ffffffff"));
    kind(PAGES, "PDF to images", vec![port("document", Modality::Document, false)], vec![port("out", Modality::Image, true)], params)
}

/// The text the pages in the range show, in reading order as the PDF draws it, one page after
/// another with a blank line or a page heading between them. With headings, a page that draws no
/// text is still listed and marked as not read, so a scan's place is visible (#592).
pub fn text_kind() -> ActionKind {
    let mut params = range();
    params.push(choice("separator", "Between pages", &[("blank", "Blank line"), ("heading", "Page heading")], "blank"));
    kind(TEXT, "PDF to text", vec![port("document", Modality::Document, false)], vec![port("out", Modality::Text, false)], params)
}

/// The pages (from 0) a range names in a document of `pages` pages, at most `cap` of them; an
/// error a person can act on when the range is empty or past the end.
pub fn page_range(first: u64, last: u64, pages: u32, cap: u64) -> Result<std::ops::Range<u32>, String> {
    if pages == 0 {
        return Err("the PDF has no pages".into());
    }
    let first = first.max(1);
    let last = if last == 0 { u64::from(pages) } else { last };
    if first > u64::from(pages) {
        return Err(format!("the PDF has {pages} page{}; the first page asked for is {first}", if pages == 1 { "" } else { "s" }));
    }
    if last < first {
        return Err(format!("the last page ({last}) comes before the first ({first})"));
    }
    let last = last.min(u64::from(pages));
    if last - first + 1 > cap {
        return Err(format!("that is {} pages; at most {cap} are read at once, so narrow the range", last - first + 1));
    }
    // Both ends are at most `pages`, a u32.
    let start = u32::try_from(first - 1).map_err(|_| "the first page is out of range".to_owned())?;
    let end = u32::try_from(last).map_err(|_| "the last page is out of range".to_owned())?;
    Ok(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_the_asked_pages_within_the_document_and_the_cap() {
        assert_eq!(page_range(1, 0, 5, 64), Ok(0..5));
        assert_eq!(page_range(2, 3, 5, 64), Ok(1..3));
        assert_eq!(page_range(4, 99, 5, 64), Ok(3..5), "a last page past the end stops at the end");
        assert_eq!(page_range(0, 1, 5, 64), Ok(0..1));
        assert!(page_range(6, 0, 5, 64).is_err());
        assert!(page_range(3, 2, 5, 64).is_err());
        assert!(page_range(1, 0, 0, 64).is_err());
        assert!(page_range(1, 0, 100, 64).is_err());
        assert_eq!(page_range(1, 64, 100, 64), Ok(0..64));
        assert_eq!(page_range(u64::from(u32::MAX), u64::MAX, u32::MAX, u64::MAX).map(|r| r.len()), Ok(1));
        assert!(page_range(u64::MAX, u64::MAX, u32::MAX, u64::MAX).is_err());
    }
}
