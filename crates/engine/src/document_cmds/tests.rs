use super::*;
use nodecraft_graph::{Canvas, Project};
use nodecraft_media::pdf_fixture::pdf;
use nodecraft_types::ProjectId;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE_ID: NodeId = NodeId::from_bytes([1; 16]);
const NEW: NodeId = NodeId::from_bytes([9; 16]);
const AT: Position = Position { x: 400.0, y: 20.0 };

/// A session holding a three-page PDF source node.
fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Reader").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let bytes = pdf(&[
        "BT /F1 12 Tf 20 150 Td (Cover) Tj ET",
        "BT /F1 12 Tf 20 150 Td (Top left) Tj ET BT /F1 12 Tf 200 30 Td (Bottom right) Tj ET",
        "0 0 1 rg 0 0 50 50 re f",
    ]);
    import_files(&mut s, C, Position { x: 0.0, y: 0.0 }, 300.0, &[Incoming { name: "paper.pdf", bytes: &bytes }], &[SOURCE_ID]).unwrap();
    s
}

fn node(s: &Session, id: NodeId) -> Option<&Node> {
    s.document().canvases().get(&C).and_then(|canvas| canvas.nodes().get(&id))
}

fn extract(page: u32) -> Extract {
    Extract { canvas: C, node: SOURCE_ID, page, id: NEW, at: AT }
}

#[test]
fn a_page_extracts_into_a_new_pdf_node_as_one_undo_step() {
    let mut s = session();
    assert_eq!(extract_page(&mut s, &extract(1)).unwrap(), NEW);
    let copy = node(&s, NEW).unwrap();
    assert_eq!((copy.kind.as_str(), copy.pos), (SOURCE, AT));
    let Some(Value::Media(asset)) = copy.params.get("asset") else { panic!("an asset") };
    assert_eq!(asset.meta.page_count, Some(1));
    let bytes = s
        .assets()
        .open_verified(asset, &TransferCancel::default(), &StreamMetrics::default())
        .unwrap()
        .read_small(MAX_ASSET_BYTES, &StreamMetrics::default())
        .unwrap();
    let document = Document::open(&bytes, &DocumentLimits::default()).unwrap();
    assert_eq!(document.text(0).unwrap(), "Top left\nBottom right");
    // The copy is independent: undo removes it, and the source still has three pages.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, NEW).is_none());
    let Some(Value::Media(source)) = node(&s, SOURCE_ID).unwrap().params.get("asset") else { panic!("an asset") };
    assert_eq!(source.meta.page_count, Some(3));
}

#[test]
fn text_from_a_page_or_a_region_becomes_a_text_node() {
    let mut s = session();
    extract_text(&mut s, &extract(1), Some([0.0, 0.0, 0.5, 0.5])).unwrap();
    let text = node(&s, NEW).unwrap();
    assert_eq!(text.kind, "source.text");
    assert_eq!(text.params.get("text"), Some(&Value::Text("Top left".into())));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, NEW).is_none(), "one undo step");
    extract_text(&mut s, &extract(1), None).unwrap();
    assert_eq!(node(&s, NEW).unwrap().params.get("text"), Some(&Value::Text("Top left\nBottom right".into())));
}

#[test]
fn extractions_refuse_what_they_cannot_do_and_change_nothing() {
    let mut s = session();
    let revision = s.revision();
    let no_text = extract_text(&mut s, &extract(2), None).unwrap_err();
    assert!(no_text.message.contains("no text"), "{}", no_text.message);
    assert!(extract_text(&mut s, &extract(1), Some([0.4, 0.4, 0.45, 0.45])).unwrap_err().message.contains("no text there"));
    assert!(extract_text(&mut s, &extract(1), Some([f64::NAN, 0.0, 1.0, 1.0])).is_err());
    let past = extract_page(&mut s, &extract(3)).unwrap_err();
    assert!(past.message.contains("has 3 pages; there is no page 4"), "{}", past.message);
    assert!(extract_page(&mut s, &extract(u32::MAX)).is_err());
    assert!(extract_page(&mut s, &Extract { id: SOURCE_ID, ..extract(0) }).is_err(), "an existing id");
    assert!(extract_page(&mut s, &Extract { at: Position { x: f32::NAN, y: 0.0 }, ..extract(0) }).is_err());
    assert!(extract_page(&mut s, &Extract { node: NEW, ..extract(0) }).is_err(), "no such node");
    assert_eq!(s.revision(), revision);
}

#[test]
fn use_pages_sets_a_document_actions_range_as_one_undo_step() {
    let mut s = session();
    let action = s.kinds().get(PAGES).unwrap().instantiate_instance(NEW, BTreeMap::new(), &ParamValues::new()).unwrap();
    s.dispatch("node.add", &json!({"canvas": C, "node": action})).unwrap();
    s.dispatch("document.use_pages", &json!({"canvas": C, "node": NEW, "first": 2, "last": 3})).unwrap();
    let params = &node(&s, NEW).unwrap().params;
    assert_eq!((params.get("first"), params.get("last")), (Some(&Value::Unsigned(Unsigned(2))), Some(&Value::Unsigned(Unsigned(3)))));
    s.dispatch("edit.undo", &json!({})).unwrap();
    let params = &node(&s, NEW).unwrap().params;
    assert_ne!(params.get("first"), Some(&Value::Unsigned(Unsigned(2))), "both pages undo together");
    assert_ne!(params.get("last"), Some(&Value::Unsigned(Unsigned(3))));
    for bad in [
        json!({"canvas": C, "node": NEW, "first": 0, "last": 0}),
        json!({"canvas": C, "node": NEW, "first": 3, "last": 2}),
        json!({"canvas": C, "node": NEW, "first": MAX_PAGE + 1, "last": 0}),
        json!({"canvas": C, "node": SOURCE_ID, "first": 1, "last": 1}),
        json!({"canvas": C, "node": NEW, "first": 1}),
    ] {
        assert!(s.enabled("document.use_pages", &bad).is_err(), "{bad}");
        assert!(s.dispatch("document.use_pages", &bad).is_err(), "{bad}");
    }
}
