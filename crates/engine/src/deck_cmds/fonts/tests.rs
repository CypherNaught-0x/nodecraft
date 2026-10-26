use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{ProjectId, Value};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const DECK: NodeId = NodeId::from_bytes([69; 16]);
const OTHER: NodeId = NodeId::from_bytes([70; 16]);

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Public fonts").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    for id in [DECK, OTHER] {
        session.dispatch("deck.create", &json!({"canvas": CANVAS, "id": id, "pos": {"x": 0, "y": 0}, "nodes": []})).unwrap();
    }
    let mut text = nodecraft_actions::deck::source_layer("heading", "Headline", (320.0, 180.0), (550.0, 120.0));
    text.content = Content::Text {
        text: "Public example".into(),
        font: " lItO ".into(),
        size: 32.0,
        line_spacing: 1.0,
        tracking: 0.0,
        align: layers::Align::Center,
        color: "#111111".into(),
    };
    session.dispatch("deck.slide_add", &json!({"canvas": CANVAS, "id": DECK, "slide": {"id": "intro", "layers": [text]}})).unwrap();
    session
}

fn lito() -> Vec<u8> {
    let mut bytes = include_bytes!("../../../../../assets/fonts/Lato-Regular.ttf").to_vec();
    for (from, to) in [(&b"\0L\0a\0t\0o"[..], &b"\0L\0i\0t\0o"[..]), (b"Lato", b"Lito")] {
        let mut at = 0;
        while let Some(found) = bytes[at..].windows(from.len()).position(|window| window == from) {
            bytes[at + found..at + found + from.len()].copy_from_slice(to);
            at += found + from.len();
        }
    }
    bytes
}

fn list(session: &mut Session, id: NodeId) -> Json {
    session.dispatch("deck.list", &json!({"canvas": CANVAS, "id": id})).unwrap()
}

#[test]
fn deck693_fonts_belong_to_the_deck_resolve_names_and_undo_without_changing_requested_text() {
    let mut s = session();
    let before = list(&mut s, DECK);
    assert_eq!(before["slides"][0]["font_resolutions"][0]["resolved"], layers::BUILTIN_FONT);
    assert_eq!(before["slides"][0]["font_resolutions"][0]["fallback"], true);
    let revision = s.revision();
    assert_eq!(import(&mut s, CANVAS, DECK, &lito(), Some(revision)).unwrap(), "Lito");
    let listed = list(&mut s, DECK);
    assert_eq!(listed["fonts"][0]["name"], "Lito");
    assert_eq!(listed["slides"][0]["font_resolutions"][0]["resolved"], "Lito");
    assert_eq!(listed["slides"][0]["font_resolutions"][0]["fallback"], false);
    assert!(list(&mut s, OTHER)["fonts"].as_array().unwrap().is_empty());
    s.dispatch("edit.undo", &json!({})).unwrap();
    let undone = list(&mut s, DECK);
    assert!(undone["fonts"].as_array().unwrap().is_empty());
    assert_eq!(undone["slides"][0]["layers"][0]["content"]["font"], " lItO ");
    assert_eq!(undone["slides"][0]["font_resolutions"][0]["fallback"], true);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(list(&mut s, DECK)["fonts"], listed["fonts"]);
    let revision = s.revision();
    s.dispatch("deck.font_remove", &json!({"canvas": CANVAS, "id": DECK, "name": " LITO ", "expected_revision": revision})).unwrap();
    assert_eq!(list(&mut s, DECK)["slides"][0]["font_resolutions"][0]["fallback"], true);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(list(&mut s, DECK)["fonts"], listed["fonts"]);
}

#[test]
fn deck693_import_rejects_invalid_builtin_and_stale_requests_before_editing() {
    let mut s = session();
    let before = s.document().clone();
    let revision = s.revision();
    let assets = s.assets().len();
    for bytes in [&b"not a font"[..], &include_bytes!("../../../../../assets/fonts/Lato-Regular.ttf")[..]] {
        assert_eq!(import(&mut s, CANVAS, DECK, bytes, Some(revision)).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    assert_eq!(import(&mut s, CANVAS, DECK, &lito(), Some(revision.saturating_sub(1))).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.assets().len(), assets);
}

#[test]
fn deck693_future_versions_and_stale_edits_refuse_before_reading_the_asset() {
    let s = session();
    let mut project = s.document().clone();
    let mut node = project.canvases().get(&CANVAS).unwrap().nodes().get(&DECK).unwrap().clone();
    node.config.insert(nodecraft_nodes::deck::CONFIG.into(), Value::Text("{\"version\":999}".into()));
    Change::SetNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let before = s.document().clone();
    let p = json!({"canvas": CANVAS, "id": DECK, "font": {"hash": "00".repeat(32), "mime": "font/ttf"}});
    assert_eq!(s.dispatch("deck.font_add", &p).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.dispatch("deck.font_remove", &json!({"canvas": CANVAS, "id": DECK, "name": "Lito"})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(import(&mut s, CANVAS, DECK, &lito(), None).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.document(), &before);
}

#[test]
fn deck693_listed_slides_round_trip_without_persisting_resolution_annotations() {
    let mut s = session();
    import(&mut s, CANVAS, DECK, &lito(), None).unwrap();
    let before = s.document().clone();
    let listed = list(&mut s, DECK);
    let result = s.dispatch("deck.set", &json!({"canvas": CANVAS, "id": DECK, "slides": listed["slides"], "expected_revision": listed["revision"]})).unwrap();
    assert_eq!(result["changed"], false);
    assert_eq!(s.document(), &before);
}

#[test]
fn deck693_font_files_round_trip_once_with_each_cards_reference() {
    let mut s = session();
    let bytes = lito();
    import(&mut s, CANVAS, DECK, &bytes, None).unwrap();
    import(&mut s, CANVAS, OTHER, &bytes, None).unwrap();
    let snapshot = nodecraft_format::Snapshot { project: s.document().clone(), assets: s.assets().clone(), epoch: 1 };
    let saved = nodecraft_format::decode(&nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    assert_eq!(saved.assets.len(), 1, "two cards share one content-addressed font payload");
    for id in [DECK, OTHER] {
        let node = saved.project.canvases().get(&CANVAS).unwrap().nodes().get(&id).unwrap();
        let fonts = held(node);
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].0, "Lito");
        assert_eq!(saved.assets.get(&fonts[0].1.hash).unwrap(), bytes.as_slice());
    }
}

#[test]
fn deck693_font_limit_and_malformed_held_metadata_are_bounded_before_changes() {
    let s = session();
    let mut project = s.document().clone();
    let mut node = project.canvases().get(&CANVAS).unwrap().nodes().get(&DECK).unwrap().clone();
    let fake: AssetRef = serde_json::from_value(json!({"hash": "00".repeat(32), "mime": "font/ttf"})).unwrap();
    let pair = |name: String| Value::List(vec![Value::Text(name), Value::Media(fake.clone())]);
    let mut pairs = vec![Value::Bool(false), pair("Lato".into()), pair("x".repeat(layers::MAX_NAME_BYTES + 1))];
    pairs.extend((0..80).map(|n| pair(format!("Other {n}"))));
    node.config.insert(nodecraft_nodes::layers::FONTS_CONFIG.into(), Value::List(pairs));
    assert_eq!(held(&node).len(), layers::MAX_FONTS);
    Change::SetNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let hash = s.import_asset(&lito()).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let error = s.dispatch("deck.font_add", &json!({"canvas": CANVAS, "id": DECK, "font": {"hash": hash, "mime": "font/ttf"}})).unwrap_err();
    assert!(error.message.contains("16 fonts"));
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(list(&mut s, DECK)["fonts"].as_array().unwrap().len(), layers::MAX_FONTS);
}
