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

#[test]
fn a_page_extracts_into_a_pdf_of_its_own_that_reads_the_same() {
    let bytes = pdf(&["BT /F1 12 Tf 20 150 Td (First) Tj ET", "BT /F1 12 Tf 20 150 Td (Second page) Tj ET 0 0 1 rg 0 0 50 50 re f"]);
    let doc = Document::open(&bytes, &DocumentLimits::default()).unwrap();
    let page = doc.extract_page(1).unwrap();
    let copy = Document::open(&page, &DocumentLimits::default()).unwrap();
    assert_eq!(copy.page_count(), 1);
    assert_eq!(copy.text(0).unwrap(), "Second page");
    assert_eq!(copy.render(0, 300, [255; 4]).unwrap(), doc.render(1, 300, [255; 4]).unwrap(), "the copy draws as the original page");
    assert!(doc.extract_page(2).is_err(), "page 3 doesn't exist");
    assert!(doc.extract_page(u32::MAX).is_err());
}

#[test]
fn text_in_a_region_keeps_only_the_glyphs_starting_there() {
    // The page is 300 x 200 points; y grows up in PDF space, down in a region.
    let bytes = pdf(&["BT /F1 12 Tf 20 150 Td (Top left) Tj ET BT /F1 12 Tf 200 30 Td (Bottom right) Tj ET"]);
    let reader_doc = Document::open(&bytes, &DocumentLimits::default()).unwrap();
    let reader = reader_doc.reader();
    assert_eq!(reader.text_in(0, None).unwrap(), "Top left\nBottom right");
    assert_eq!(reader.text_in(0, Some([0.0, 0.0, 0.5, 0.5])).unwrap(), "Top left");
    assert_eq!(reader.text_in(0, Some([1.0, 1.0, 0.5, 0.5])).unwrap(), "Bottom right", "corners in either order");
    assert_eq!(reader.text_in(0, Some([0.4, 0.4, 0.45, 0.45])).unwrap(), "");
    assert_eq!(reader.text_in(0, Some([-5.0, -5.0, 9.0, 9.0])).unwrap(), "Top left\nBottom right", "clamped to the page");
    assert!(reader.text_in(0, Some([f64::NAN, 0.0, 1.0, 1.0])).is_err());
}

#[test]
fn images_become_one_page_each() {
    let red = PageImage { width: 40, height: 20, rgba: [255, 0, 0, 255].repeat(40 * 20) };
    let blue = PageImage { width: 10, height: 30, rgba: [0, 0, 255, 255].repeat(10 * 30) };
    let bytes = images_to_pdf(vec![red, blue]).unwrap();
    let doc = Document::open(&bytes, &DocumentLimits::default()).unwrap();
    assert_eq!(probe_document(&bytes, &DocumentLimits::default()).unwrap(), DocumentInfo { pages: 2 });
    let first = doc.render(0, 40, [255; 4]).unwrap();
    assert_eq!((first.width, first.height), (40, 20), "the page keeps the image's shape");
    assert_eq!(&first.rgba[(10 * 40 + 20) * 4..][..4], [255, 0, 0, 255]);
    let second = doc.render(1, 30, [255; 4]).unwrap();
    assert_eq!((second.width, second.height), (10, 30));
    assert_eq!(&second.rgba[(15 * 10 + 5) * 4..][..4], [0, 0, 255, 255]);

    assert!(images_to_pdf(Vec::new()).is_err());
    assert!(images_to_pdf(vec![PageImage { width: 2, height: 2, rgba: vec![0; 15] }]).is_err(), "pixels must match the size");
    assert!(images_to_pdf(vec![PageImage { width: 0, height: 2, rgba: Vec::new() }]).is_err());
}
