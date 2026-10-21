use super::*;

use super::fixture::pdf;

const TWO_LINES: &str = "BT /F1 12 Tf 20 150 Td (Hello) Tj 40 0 Td (world) Tj 0 -20 Td (second line) Tj ET";

#[test]
fn pages_are_counted_and_text_keeps_words_and_lines() {
    let bytes = pdf(&[TWO_LINES, "BT /F1 18 Tf 20 100 Td (Page two) Tj ET", ""]);
    assert_eq!(probe_document(&bytes, &DocumentLimits::default()).unwrap(), DocumentInfo { pages: 3 });
    let doc = Document::open(&bytes, &DocumentLimits::default()).unwrap();
    assert_eq!(doc.text(0).unwrap(), "Hello world\nsecond line");
    assert_eq!(doc.text(1).unwrap(), "Page two");
    assert_eq!(doc.text(2).unwrap(), "", "a page that draws no text has none (no OCR)");
    assert!(doc.text(3).is_err(), "page 4 doesn't exist");
    assert!(doc.render(u32::MAX, 64, [255; 4]).is_err());
}

#[test]
fn text_is_cut_at_the_limit_on_a_character_boundary() {
    let bytes = pdf(&["BT /F1 12 Tf 20 150 Td (\\351\\351\\351\\351) Tj ET"]);
    let limits = DocumentLimits { max_page_text_bytes: 5, ..DocumentLimits::default() };
    let text = Document::open(&bytes, &limits).unwrap().text(0).unwrap();
    assert_eq!(text, "éé");
}

#[test]
fn a_page_is_drawn_at_the_asked_size_within_the_limit() {
    let bytes = pdf(&["0 0 1 rg 0 0 150 200 re f"]);
    let doc = Document::open(&bytes, &DocumentLimits::default()).unwrap();
    let page = doc.render(0, 300, [255, 255, 255, 255]).unwrap();
    assert_eq!((page.width, page.height), (300, 200));
    assert_eq!(page.rgba.len(), 300 * 200 * 4);
    // Left half blue, right half the white background.
    let at = |x: usize, y: usize| &page.rgba[(y * 300 + x) * 4..][..4];
    assert_eq!(at(10, 100), [0, 0, 255, 255]);
    assert_eq!(at(290, 100), [255, 255, 255, 255]);
    let small = Document::open(&bytes, &DocumentLimits { max_side: 60, ..DocumentLimits::default() }).unwrap();
    let page = small.render(0, 4096, [0; 4]).unwrap();
    assert_eq!((page.width, page.height), (60, 40));
    // A size whose scale isn't exact in f32 still gets its full longer side.
    for edge in [97, 333, 1601, 4093] {
        let page = doc.render(0, edge, [255; 4]).unwrap();
        assert_eq!(page.width, edge, "the longer side is exactly the size asked for");
        assert_eq!(page.height, ((edge as f32) * 2.0 / 3.0).round() as u32);
    }
    // A reader shares its caches across pages and gives the same pages.
    let reader = doc.reader();
    assert_eq!(reader.render(0, 97, [255; 4]).unwrap(), doc.render(0, 97, [255; 4]).unwrap());
    // Straight alpha over a transparent background: blue stays fully blue where drawn.
    assert_eq!(&page.rgba[(20 * 60 + 5) * 4..][..4], [0, 0, 255, 255]);
    assert_eq!(&page.rgba[(20 * 60 + 55) * 4..][..4], [0, 0, 0, 0]);
}

#[test]
fn hostile_inputs_are_errors() {
    let limits = DocumentLimits::default();
    assert_eq!(Document::open(b"not a pdf", &limits).unwrap_err(), MediaError::Unrecognized);
    assert!(Document::open(b"%PDF-1.7\ngarbage", &limits).is_err());
    let bytes = pdf(&[TWO_LINES]);
    let tiny = DocumentLimits { max_input_bytes: 10, ..limits };
    assert!(matches!(Document::open(&bytes, &tiny), Err(MediaError::TooLarge { .. })));
    let mut truncated = bytes.clone();
    truncated.truncate(bytes.len() / 2);
    // A truncated file is repaired or refused, never a panic.
    if let Ok(doc) = Document::open(&truncated, &limits) {
        for i in 0..doc.page_count() {
            let _ = doc.text(i);
            let _ = doc.render(i, 32, [255; 4]);
        }
    }
    let encrypted = String::from_utf8_lossy(&bytes).replace("/Root 1 0 R", "/Root 1 0 R /Encrypt << /Filter /Standard /V 9 /R 9 >> /ID [<00> <00>]");
    assert!(Document::open(encrypted.as_bytes(), &limits).is_err());
}
