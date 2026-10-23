use crate::Session;
use crate::element_cmds::{image_wires, text};
use crate::element_library::{MemoryElements, install};
use nodecraft_graph::{Canvas, Change};
use nodecraft_nodes::element::{GUIDELINES, NAME};
use nodecraft_types::ProjectId;
use nodecraft_types::{AssetRef, Blake3, CanvasId, EdgeId, ErrorKind, NodeId};
use serde_json::json;
use std::rc::Rc;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([3; 16]);
const B: NodeId = NodeId::from_bytes([4; 16]);
const HERO: NodeId = NodeId::from_bytes([6; 16]);
const PLACED: NodeId = NodeId::from_bytes([7; 16]);
const SEED: NodeId = NodeId::from_bytes([8; 16]);

/// A project with one canvas, given the library in `store`.
fn session(project: u8, store: &MemoryElements) -> Session {
    let mut document = nodecraft_graph::Project::new(ProjectId::from_bytes([project; 16]), "P").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut document).unwrap();
    let mut s = Session::new(document).unwrap();
    install(&mut s, Rc::new(store.clone()));
    s
}

fn png(width: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * 2).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(width, 2, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

fn image(s: &mut Session, width: u32) -> AssetRef {
    AssetRef { hash: s.import_asset(&png(width)).unwrap(), mime: "image/png".parse().unwrap(), meta: Default::default() }
}

/// A project whose canvas has Hero: two image sources wired in.
fn with_hero(store: &MemoryElements) -> Session {
    let mut s = session(2, store);
    for (id, width) in [(A, 2), (B, 3)] {
        let asset = image(&mut s, width);
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
        s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": id, "name": "asset", "value": {"type": "media", "value": asset}})).unwrap();
    }
    let from = [json!({"node": A, "port": "out"}), json!({"node": B, "port": "out"})];
    s.dispatch(
        "element.create",
        &json!({"canvas": CANVAS, "id": HERO, "name": "Hero", "category": "character", "guidelines": "red scarf", "from": from,
            "edges": [EdgeId::from_bytes([10; 16]), EdgeId::from_bytes([11; 16])], "pos": {"x": 400, "y": 0}}),
    )
    .unwrap();
    s
}

fn canvas(s: &Session) -> &Canvas {
    s.document().canvases().get(&CANVAS).unwrap()
}

fn images(s: &Session, id: NodeId) -> Vec<Blake3> {
    let canvas = canvas(s);
    image_wires(canvas, id)
        .iter()
        .map(|wire| match &canvas.nodes().get(&wire.from.node).unwrap().params.get("asset") {
            Some(nodecraft_types::Value::Media(asset)) => asset.hash,
            other => panic!("not an image source: {other:?}"),
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_element_exported_from_one_library_is_imported_into_another_and_placed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Hero.ncelement");
    let mine = MemoryElements::default();
    let mut first = with_hero(&mine);
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO, "shared": true})).unwrap();
    let expected = images(&first, HERO);
    let exported = first.dispatch("library.export_element", &json!({"name": "hero", "path": file})).unwrap();
    assert_eq!(exported["exported"]["name"], "Hero");
    assert_eq!(first.dispatch("library.export_element", &json!({"name": "Hero", "path": file})).unwrap_err().kind, ErrorKind::Conflict);
    first.dispatch("library.export_element", &json!({"name": "Hero", "path": file, "overwrite": true})).unwrap();
    assert_eq!(
        first.dispatch("library.export_element", &json!({"name": "Nobody", "path": dir.path().join("n.ncelement")})).unwrap_err().kind,
        ErrorKind::NotFound
    );
    assert_eq!(first.dispatch("library.export_element", &json!({"name": "Hero", "path": "relative.ncelement"})).unwrap_err().kind, ErrorKind::InvalidInput);

    let theirs = MemoryElements::default();
    let mut second = session(3, &theirs);
    let imported = second.dispatch("library.import_element", &json!({"path": file})).unwrap();
    assert_eq!(imported["imported"], json!({"name": "Hero", "category": "character", "guidelines": "red scarf", "images": 2, "sheet": false, "shared": false}));
    assert_eq!(theirs.media_count(), 2);
    assert_eq!(second.dispatch("library.import_element", &json!({"path": file})).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(second.dispatch("library.import_element", &json!({"path": file, "as": "Hero-2"})).unwrap()["imported"]["name"], "Hero-2");
    assert_eq!(second.dispatch("library.import_element", &json!({"path": file, "replace": true})).unwrap()["replaced"], true);
    assert_eq!(second.dispatch("library.import_element", &json!({"path": file, "as": "no spaces"})).unwrap_err().kind, ErrorKind::InvalidInput);

    second.dispatch("element.place", &json!({"canvas": CANVAS, "name": "Hero", "id": PLACED, "pos": {"x": 900, "y": 100}})).unwrap();
    assert_eq!(images(&second, PLACED), expected, "the same images, checked against their hashes");

    std::fs::write(dir.path().join("junk.ncelement"), b"not a bundle").unwrap();
    assert_eq!(second.dispatch("library.import_element", &json!({"path": dir.path().join("junk.ncelement")})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(second.dispatch("library.import_element", &json!({"path": dir.path().join("gone")})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(second.dispatch("library.import_element", &json!({"path": dir.path()})).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn sharing_marks_an_entry_for_others() {
    let store = MemoryElements::default();
    let mut s = with_hero(&store);
    s.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    assert_eq!(s.dispatch("library.share_element", &json!({"name": "Hero", "shared": true})).unwrap()["shared"]["shared"], true);
    assert_eq!(s.dispatch("library.elements", &json!({})).unwrap()["elements"][0]["shared"], true);
    s.dispatch("library.share_element", &json!({"name": "Hero", "shared": false})).unwrap();
    assert_eq!(s.dispatch("library.elements", &json!({})).unwrap()["elements"][0]["shared"], false);
    assert_eq!(s.dispatch("library.share_element", &json!({"name": "Nobody", "shared": true})).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn an_element_placed_under_another_name_updates_from_its_entry() {
    let store = MemoryElements::default();
    let mut first = with_hero(&store);
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    let mut second = session(3, &store);
    second.dispatch("library.elements", &json!({})).unwrap();
    second.dispatch("element.place", &json!({"canvas": CANVAS, "name": "Hero", "id": PLACED, "pos": {"x": 900, "y": 100}, "as": "Hero-2"})).unwrap();
    // And renamed again on the canvas.
    second.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": PLACED, "name": NAME, "value": {"type": "text", "value": "Sidekick"}})).unwrap();

    first.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": HERO, "name": GUIDELINES, "value": {"type": "text", "value": "blue scarf"}})).unwrap();
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO, "replace": true})).unwrap();
    second.dispatch("library.elements", &json!({})).unwrap();
    second.dispatch("element.update", &json!({"canvas": CANVAS, "id": PLACED, "seed": SEED})).unwrap();
    let node = canvas(&second).nodes().get(&PLACED).unwrap().clone();
    assert_eq!((text(&node, NAME), text(&node, GUIDELINES)), ("Sidekick".into(), "blue scarf".into()));

    // Once its entry is gone it says so.
    first.dispatch("library.rename_element", &json!({"name": "Hero", "to": "Villain"})).unwrap();
    second.dispatch("library.elements", &json!({})).unwrap();
    let error = second.dispatch("element.update", &json!({"canvas": CANVAS, "id": PLACED, "seed": SEED})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(error.message.contains("Hero"), "{}", error.message);

    // Saved under its own name, it updates from that entry, not the one it was placed from.
    first.dispatch("library.rename_element", &json!({"name": "Villain", "to": "Hero"})).unwrap();
    second.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": PLACED, "name": GUIDELINES, "value": {"type": "text", "value": "green scarf"}})).unwrap();
    second.dispatch("element.save", &json!({"canvas": CANVAS, "id": PLACED})).unwrap();
    second.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": PLACED, "name": GUIDELINES, "value": {"type": "text", "value": "edited"}})).unwrap();
    second.dispatch("element.update", &json!({"canvas": CANVAS, "id": PLACED, "seed": NodeId::from_bytes([9; 16])})).unwrap();
    assert_eq!(text(canvas(&second).nodes().get(&PLACED).unwrap(), GUIDELINES), "green scarf");
}
