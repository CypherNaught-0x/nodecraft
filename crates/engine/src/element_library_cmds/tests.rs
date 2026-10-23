use super::*;
use crate::Session;
use crate::element_library::{ElementStore, MemoryElements, install};
use nodecraft_types::ProjectId;
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
    image_wires(canvas, id).iter().map(|wire| wired_image(canvas, wire).unwrap().hash).collect()
}

fn place(name: &str) -> Json {
    json!({"canvas": CANVAS, "name": name, "id": PLACED, "pos": {"x": 900, "y": 100}})
}

#[test]
fn an_element_saved_in_one_project_is_placed_in_another_with_its_images() {
    let store = MemoryElements::default();
    let mut first = with_hero(&store);
    let saved = first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    assert_eq!(saved["saved"], json!({"name": "Hero", "category": "character", "guidelines": "red scarf", "images": 2, "sheet": false, "shared": false}));
    assert_eq!(store.media_count(), 2);
    let expected = images(&first, HERO);

    let mut second = session(3, &store);
    assert_eq!(second.dispatch("library.elements", &json!({})).unwrap()["elements"][0]["name"], "Hero");
    let placed = second.dispatch("element.place", &place("Hero")).unwrap();
    assert_eq!(placed["id"], json!(PLACED));
    assert_eq!(placed["sources"].as_array().unwrap().len(), 2);
    let node = canvas(&second).nodes().get(&PLACED).unwrap().clone();
    assert_eq!((text(&node, NAME), text(&node, CATEGORY), text(&node, GUIDELINES)), ("Hero".into(), "character".into(), "red scarf".into()));
    assert_eq!(images(&second, PLACED), expected, "the images, in order");
    assert!(expected.iter().all(|hash| second.assets().contains(hash)), "copied into the project");

    second.dispatch("edit.undo", &json!({})).unwrap();
    assert!(canvas(&second).nodes().is_empty(), "one undo step");
}

#[test]
fn placing_over_a_taken_name_asks_for_another() {
    let store = MemoryElements::default();
    let mut s = with_hero(&store);
    s.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    assert_eq!(s.dispatch("element.place", &place("hero")).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(s.enabled("element.place", &place("hero")).unwrap_err().kind, ErrorKind::Conflict);
    let mut renamed = place("Hero");
    renamed["as"] = json!("Hero2");
    assert_eq!(s.dispatch("element.place", &renamed).unwrap()["name"], "Hero2");
    assert_eq!(images(&s, PLACED), images(&s, HERO));
    assert_eq!(s.dispatch("element.place", &place("Villain")).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn saving_needs_replace_over_an_entry_and_ready_images() {
    let store = MemoryElements::default();
    let mut s = with_hero(&store);
    let params = json!({"canvas": CANVAS, "id": HERO});
    s.dispatch("element.save", &params).unwrap();
    assert_eq!(s.dispatch("element.save", &params).unwrap_err().kind, ErrorKind::Conflict);
    let replaced = s.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO, "replace": true, "shared": true})).unwrap();
    assert_eq!((replaced["replaced"].clone(), replaced["saved"]["shared"].clone()), (json!(true), json!(true)));

    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": SEED, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch(
        "element.create",
        &json!({"canvas": CANVAS, "id": PLACED, "name": "Empty", "from": [{"node": SEED, "port": "out"}], "edges": [EdgeId::from_bytes([12; 16])], "pos": {"x": 0, "y": 900}}),
    )
    .unwrap();
    let empty = json!({"canvas": CANVAS, "id": PLACED});
    assert_eq!(s.dispatch("element.save", &empty).unwrap_err().kind, ErrorKind::InvalidInput, "an image source with no image");
    assert_eq!(s.dispatch("element.save", &json!({"canvas": CANVAS, "id": A})).unwrap_err().kind, ErrorKind::InvalidInput, "not an Element");
}

#[test]
fn update_swaps_its_sources_for_the_entrys_as_one_step() {
    let store = MemoryElements::default();
    let mut first = with_hero(&store);
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    let mut second = session(3, &store);
    second.dispatch("element.place", &place("Hero")).unwrap();
    // The library's Hero changes: one image, other guidelines.
    first.dispatch("edge.disconnect", &json!({"canvas": CANVAS, "id": EdgeId::from_bytes([11; 16])})).unwrap();
    first.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": HERO, "name": GUIDELINES, "value": {"type": "text", "value": "blue scarf"}})).unwrap();
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO, "replace": true})).unwrap();
    assert_eq!(store.media_count(), 1, "the image no entry shows is dropped");

    // Another project's change shows once the library is read again.
    let update = json!({"canvas": CANVAS, "id": PLACED, "seed": SEED});
    assert_eq!(second.dispatch("library.elements", &json!({})).unwrap()["elements"][0]["guidelines"], "blue scarf");
    let updated = second.dispatch("element.update", &update).unwrap();
    assert_eq!(updated["removed"].as_array().unwrap().len(), 2, "its old sources fed only it");
    assert_eq!(images(&second, PLACED), images(&first, HERO));
    let node = canvas(&second).nodes().get(&PLACED).unwrap().clone();
    assert_eq!(text(&node, GUIDELINES), "blue scarf");
    assert_eq!(canvas(&second).nodes().len(), 2);
    second.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(canvas(&second).nodes().len(), 3, "one undo step");
    assert_eq!(images(&second, PLACED).len(), 2);
}

#[test]
fn update_unwires_sources_that_feed_others() {
    let store = MemoryElements::default();
    let mut s = with_hero(&store);
    s.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    let updated = s.dispatch("element.update", &json!({"canvas": CANVAS, "id": HERO, "seed": SEED})).unwrap();
    assert_eq!(updated["removed"], json!([A, B]));
    // Wire A into a second Element as well: a later update keeps it.
    let mut s = with_hero(&store);
    s.dispatch(
        "element.create",
        &json!({"canvas": CANVAS, "id": PLACED, "name": "Other", "from": [{"node": A, "port": "out"}], "edges": [EdgeId::from_bytes([12; 16])], "pos": {"x": 0, "y": 900}}),
    )
    .unwrap();
    let updated = s.dispatch("element.update", &json!({"canvas": CANVAS, "id": HERO, "seed": SEED})).unwrap();
    assert_eq!(updated["removed"], json!([B]));
    assert!(canvas(&s).nodes().contains_key(&A));
    assert_eq!(image_wires(canvas(&s), PLACED).len(), 1);
}

#[test]
fn the_library_is_renamed_and_tidied() {
    let store = MemoryElements::default();
    let mut s = with_hero(&store);
    s.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    assert_eq!(s.dispatch("library.rename_element", &json!({"name": "Hero", "to": "bad name"})).unwrap_err().kind, ErrorKind::InvalidInput);
    s.dispatch("library.rename_element", &json!({"name": "hero", "to": "Heroine"})).unwrap();
    assert_eq!(s.dispatch("library.elements", &json!({})).unwrap()["elements"][0]["name"], "Heroine");
    assert_eq!(s.dispatch("library.remove_element", &json!({"name": "Hero"})).unwrap_err().kind, ErrorKind::NotFound);
    s.dispatch("library.remove_element", &json!({"name": "Heroine"})).unwrap();
    assert_eq!(s.dispatch("library.elements", &json!({})).unwrap(), json!({"elements": []}));
    assert_eq!(store.media_count(), 0);
}

#[test]
fn library_media_that_went_bad_fails_the_place_and_changes_nothing() {
    let store = MemoryElements::default();
    let mut first = with_hero(&store);
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    for hash in store.media_kept().unwrap() {
        store.put_media(&hash, b"tampered").unwrap();
    }
    let mut second = session(3, &store);
    assert!(second.dispatch("element.place", &place("Hero")).is_err());
    assert!(canvas(&second).nodes().is_empty());
    assert!(second.assets().is_empty());

    // Saving again rewrites the damaged images.
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO, "replace": true})).unwrap();
    second.dispatch("library.elements", &json!({})).unwrap();
    second.dispatch("element.place", &place("Hero")).unwrap();
    assert_eq!(images(&second, PLACED), images(&first, HERO));
}

#[test]
fn renaming_or_removing_an_entry_leaves_placed_elements_working() {
    let store = MemoryElements::default();
    let mut first = with_hero(&store);
    first.dispatch("element.save", &json!({"canvas": CANVAS, "id": HERO})).unwrap();
    let mut second = session(3, &store);
    second.dispatch("library.elements", &json!({})).unwrap();
    second.dispatch("element.place", &place("Hero")).unwrap();
    let placed = second.document().clone();
    let hashes = images(&second, PLACED);

    first.dispatch("library.rename_element", &json!({"name": "Hero", "to": "Heroine"})).unwrap();
    first.dispatch("library.remove_element", &json!({"name": "Heroine"})).unwrap();
    assert_eq!(store.media_count(), 0, "the library let its images go");
    assert_eq!(second.document(), &placed, "the project keeps its copy");
    assert_eq!(images(&second, PLACED), hashes);
    assert!(hashes.iter().all(|hash| second.assets().contains(hash)));
    second
        .dispatch("node.param_set", &json!({"canvas": CANVAS, "id": PLACED, "name": GUIDELINES, "value": {"type": "text", "value": "still editable"}}))
        .unwrap();
}

#[test]
fn without_a_library_the_commands_say_so() {
    let mut document = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut document).unwrap();
    let mut s = Session::new(document).unwrap();
    assert_eq!(s.dispatch("library.elements", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.dispatch("element.place", &place("Hero")).unwrap_err().kind, ErrorKind::Unsupported);
}
