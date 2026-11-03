use crate::Session;
use crate::library_browse_cmds::tests::{ALPHA, BETA, INK, PAINTER, PHOTO, SOUND, WRITER, g, png, session};
use nodecraft_types::{Blake3, ErrorKind, NodeId, Value};
use serde_json::{Value as Json, json};

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([0xA0 + n; 16])
}

fn hash_of(s: &Session, node: NodeId, canvas: nodecraft_types::CanvasId) -> Blake3 {
    match s.document().canvases()[&canvas].nodes()[&node].params.get("asset") {
        Some(Value::Media(asset)) => asset.hash,
        other => panic!("no asset: {other:?}"),
    }
}

#[test]
fn several_entries_become_source_cards_in_one_undo_step_titled_after_where_they_came_from() {
    let mut s = session();
    let photo = hash_of(&s, PHOTO, ALPHA);
    let sound = hash_of(&s, SOUND, BETA);
    let before = s.document().clone();
    let items = json!([
        {"hash": photo},
        {"from": {"canvas": ALPHA, "node": PAINTER, "generation": g(1), "port": "out", "item": 0}},
        {"from": {"canvas": ALPHA, "node": WRITER, "generation": g(3)}},
        {"hash": sound},
    ]);
    let ids = [id(1), id(2), id(3), id(4)];
    let placed = s.dispatch("library.attach", &json!({"canvas": ALPHA, "items": items, "ids": ids, "at": {"x": 100, "y": 50}, "step": 300})).unwrap();
    let placed = placed["placed"].as_array().unwrap();
    assert_eq!(placed.len(), 4);
    assert_eq!(placed[0]["from"], json!({"canvas": ALPHA, "node": PHOTO, "generation": null}), "the photo was first found on its source card");
    assert_eq!(placed[1]["from"]["generation"], json!(g(1)));
    assert_eq!(placed[3]["from"]["canvas"], json!(BETA), "assets come from any canvas");

    let nodes = s.document().canvases()[&ALPHA].nodes();
    let card = |n: u8| &nodes[&id(n)];
    assert_eq!([1, 2, 3, 4].map(|n| card(n).kind.as_str()), ["source.image", "source.image", "source.text", "source.audio"],);
    assert_eq!([1, 2, 3, 4].map(|n| card(n).title.as_str()), ["Holiday photo", "Painter · 1/2", "Writer · 1/1", ""]);
    assert!(matches!(card(1).params.get("asset"), Some(Value::Media(a)) if a.hash == photo && a.mime.essence() == "image/png"));
    assert!(matches!(card(3).params.get("text"), Some(Value::Text(t)) if t.starts_with('é')));
    // A 2×2 grid from `at`.
    let at = [1, 2, 3, 4].map(|n| (card(n).pos.x, card(n).pos.y));
    assert_eq!(at, [(100.0, 50.0), (400.0, 50.0), (100.0, 350.0), (400.0, 350.0)]);
    // The library now counts the placed copies as uses of the same stored bytes.
    let listed = s.dispatch("library.assets", &json!({})).unwrap();
    assert_eq!(listed["total"], 3, "no new asset: placing reuses the project's bytes");

    let after = s.document().clone();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before, "one undo step");
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &after);
}

#[test]
fn a_request_with_any_bad_entry_is_refused_whole() {
    let mut s = session();
    let photo = hash_of(&s, PHOTO, ALPHA);
    let before = s.document().clone();
    let at = json!({"x": 0, "y": 0});
    let one = |item: Json| json!({"canvas": ALPHA, "items": [{"hash": photo}, item], "ids": [id(1), id(2)], "at": at});
    let unknown = Blake3::from_bytes([7; 32]);
    let style = json!({"from": {"canvas": BETA, "node": INK, "generation": crate::library_browse_cmds::tests::g(0)}});
    let ink = s.document().canvases()[&BETA].nodes()[&INK].history[0].id;
    let many: Vec<Json> = (0..33).map(|_| json!({"hash": photo})).collect();
    let many_ids: Vec<NodeId> = (0..33).map(|n| NodeId::from_bytes([n; 16])).collect();
    for (params, kind) in [
        (one(json!({"hash": unknown})), ErrorKind::NotFound),
        (one(style), ErrorKind::NotFound),
        (one(json!({"from": {"canvas": BETA, "node": INK, "generation": ink}})), ErrorKind::Unsupported),
        (one(json!({"from": {"canvas": ALPHA, "node": PAINTER, "generation": g(1), "port": "out", "item": 9}})), ErrorKind::NotFound),
        // The caller can't supply a type or metadata for an asset: the project's reference is used.
        (one(json!({"hash": photo, "mime": "text/html"})), ErrorKind::InvalidInput),
        (json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [PHOTO], "at": at}), ErrorKind::Conflict),
        (json!({"canvas": ALPHA, "items": [{"hash": photo}, {"hash": photo}], "ids": [id(1), id(1)], "at": at}), ErrorKind::Conflict),
        (json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [], "at": at}), ErrorKind::InvalidInput),
        (json!({"canvas": ALPHA, "items": [], "ids": [], "at": at}), ErrorKind::InvalidInput),
        (json!({"canvas": ALPHA, "items": many, "ids": many_ids, "at": at}), ErrorKind::InvalidInput),
        (json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": at, "step": 0}), ErrorKind::InvalidInput),
        (json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": at, "step": 1e30}), ErrorKind::InvalidInput),
        (json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 1e30, "y": 0}}), ErrorKind::InvalidInput),
        (
            json!({"canvas": ALPHA, "items": [{"hash": photo}, {"hash": photo}], "ids": [id(1), id(2)], "at": {"x": 9_999_999.0, "y": 0}, "step": 10_000}),
            ErrorKind::InvalidInput,
        ),
        (json!({"canvas": nodecraft_types::CanvasId::from_bytes([9; 16]), "items": [{"hash": photo}], "ids": [id(1)], "at": at}), ErrorKind::NotFound),
    ] {
        assert_eq!(s.enabled("library.attach", &params).unwrap_err().kind, kind, "{params}");
        assert_eq!(s.dispatch("library.attach", &params).unwrap_err().kind, kind, "{params}");
        assert_eq!(s.document(), &before, "nothing changed: {params}");
    }
}

#[test]
fn an_asset_whose_bytes_the_project_no_longer_stores_is_refused() {
    let full = session();
    let photo = hash_of(&full, PHOTO, ALPHA);
    // The same document, with none of its media stored.
    let mut bare = Session::new(full.document().clone()).unwrap();
    let error = bare.dispatch("library.attach", &json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}})).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::NotFound, "the file is missing from this project"));
    let from = json!({"from": {"canvas": ALPHA, "node": PAINTER, "generation": g(1), "port": "out", "item": 0}});
    let error = bare.dispatch("library.attach", &json!({"canvas": ALPHA, "items": [from], "ids": [id(1)], "at": {"x": 0, "y": 0}})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    // Once stored again, it places.
    bare.import_asset(&png(10)).unwrap();
    bare.dispatch("library.attach", &json!({"canvas": ALPHA, "items": [{"hash": photo}], "ids": [id(1)], "at": {"x": 0, "y": 0}})).unwrap();
}
