use super::*;
use crate::Session;
use nodecraft_graph::{Field, FieldRect, Project};
use nodecraft_types::{Blake3, MediaMeta, ProjectId};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMG: NodeId = NodeId::from_bytes([3; 16]);
const TXT: NodeId = NodeId::from_bytes([4; 16]);
const NOTE_A: NodeId = NodeId::from_bytes([5; 16]);
const NOTE_B: NodeId = NodeId::from_bytes([6; 16]);
const DECK: NodeId = NodeId::from_bytes([9; 16]);
const FIELD: FieldId = FieldId::from_bytes([7; 16]);

fn asset() -> AssetRef {
    AssetRef { hash: Blake3::from_bytes([8; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() }
}

/// An image source, a text source to its right, and a field below holding two notes.
fn session() -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut field = Field::new(FIELD, "Moodboard");
    field.rect = FieldRect { pos: Position { x: 0.0, y: 500.0 }, width: 800.0, height: 400.0 };
    Change::PutField { canvas: CANVAS, field }.apply(&mut project).unwrap();
    let nodes = [
        (IMG, "source.image", (0.0, 0.0), "Cat", None, Some(("asset", Value::Media(asset())))),
        (TXT, "source.text", (400.0, 0.0), "", None, Some(("text", Value::Text("a cat in a hat".into())))),
        (NOTE_A, "structure.note", (300.0, 600.0), "Right", Some(FIELD), Some(("text", Value::Text("right".into())))),
        (NOTE_B, "structure.note", (0.0, 600.0), "Left", Some(FIELD), Some(("text", Value::Text("left".into())))),
    ];
    for (id, kind, (x, y), title, field, param) in nodes {
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id, BTreeMap::new(), &ParamValues::new()).unwrap();
        node.pos = Position { x, y };
        node.title = title.into();
        node.field = field;
        if let Some((name, value)) = param {
            node.params.insert(name.into(), value);
        }
        Change::AddNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    }
    Session::new(project).unwrap()
}

fn create(s: &mut Session, nodes: &[NodeId], fields: &[FieldId]) -> Json {
    s.dispatch("deck.create", &json!({"canvas": CANVAS, "id": DECK, "pos": {"x": 900, "y": 0}, "title": "Pitch", "nodes": nodes, "fields": fields})).unwrap()
}

fn list(s: &mut Session) -> Json {
    s.dispatch("deck.list", &json!({"canvas": CANVAS, "id": DECK})).unwrap()
}

fn names(s: &mut Session) -> Vec<String> {
    list(s)["slides"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect()
}

/// Params for an edit of the deck.
fn on(extra: Json) -> Json {
    let mut p = json!({"canvas": CANVAS, "id": DECK});
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    p
}

#[test]
fn a_deck_made_from_nodes_and_fields_follows_the_canvas() {
    let mut s = session();
    // The notes are in the field, so they get no slides of their own; the order is the canvas's.
    let made = create(&mut s, &[TXT, NOTE_A, IMG, IMG], &[FIELD]);
    assert_eq!(made, json!({"id": DECK, "slides": 3}));
    assert_eq!(names(&mut s), ["Cat", "Text", "Moodboard"]);
    let listed = list(&mut s);
    assert_eq!((&listed["width"], &listed["height"], &listed["writable"]), (&json!(1920), &json!(1080), &json!(true)));
    let slides = listed["slides"].as_array().unwrap();
    assert_eq!(slides[0]["sources"], json!([{"layer": format!("src-{IMG}"), "origin": IMG.to_string(), "nth": 0, "shows": "image", "image": asset()}]));
    assert_eq!(slides[1]["sources"][0]["text"], json!("a cat in a hat"));
    // The field slide has its title, then its notes left to right as they sit.
    let field: Vec<&Json> = slides[2]["sources"].as_array().unwrap().iter().collect();
    assert_eq!(field.iter().map(|s| s["text"].as_str().unwrap()).collect::<Vec<_>>(), ["left", "right"]);
    assert_eq!(slides[2]["layers"].as_array().unwrap().len(), 3);
    // One undo step removes the deck.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.document().canvases().get(&CANVAS).unwrap().nodes().get(&DECK).is_none());
}

#[test]
fn making_a_deck_refuses_what_is_not_there() {
    let mut s = session();
    let p = |nodes: Json, fields: Json| json!({"canvas": CANVAS, "id": DECK, "pos": {"x": 0, "y": 0}, "nodes": nodes, "fields": fields});
    let missing = s.dispatch("deck.create", &p(json!([NodeId::from_bytes([77; 16])]), json!([]))).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);
    let missing = s.dispatch("deck.create", &p(json!([]), json!([FieldId::from_bytes([77; 16])]))).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);
    let far = json!({"canvas": CANVAS, "id": DECK, "pos": {"x": 1e9, "y": 0}});
    assert_eq!(s.dispatch("deck.create", &far).unwrap_err().kind, ErrorKind::InvalidInput);
    // An empty deck is a deck; its id must be new.
    assert_eq!(create(&mut s, &[], &[]), json!({"id": DECK, "slides": 0}));
    let again = s.dispatch("deck.create", &p(json!([]), json!([]))).unwrap_err();
    assert_ne!(again.kind, ErrorKind::Internal);
}

#[test]
fn slides_are_added_moved_renamed_and_removed_one_undo_step_each() {
    let mut s = session();
    create(&mut s, &[IMG, TXT], &[]);
    let added = s.dispatch("deck.slide_add", &on(json!({"slide": {"name": "Intro"}, "index": 0}))).unwrap();
    assert_eq!(added, json!({"changed": true, "slide": "slide-3"}));
    assert_eq!(names(&mut s), ["Intro", "Cat", "Text"]);
    let duplicate = s.dispatch("deck.slide_add", &on(json!({"slide": {"id": "slide-3"}}))).unwrap_err();
    assert_eq!(duplicate.kind, ErrorKind::Conflict);
    s.dispatch("deck.slide_move", &on(json!({"slide": "slide-3", "index": 99}))).unwrap();
    assert_eq!(names(&mut s), ["Cat", "Text", "Intro"]);
    s.dispatch("deck.slide_update", &on(json!({"slide": "slide-1", "name": "Kitten"}))).unwrap();
    assert_eq!(names(&mut s), ["Kitten", "Text", "Intro"]);
    // The same name again changes nothing and adds no undo step.
    let same = s.dispatch("deck.slide_update", &on(json!({"slide": "slide-1", "name": "Kitten"}))).unwrap();
    assert_eq!(same, json!({"changed": false}));
    s.dispatch("deck.slide_remove", &on(json!({"slide": "slide-2"}))).unwrap();
    assert_eq!(names(&mut s), ["Kitten", "Intro"]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(names(&mut s), ["Kitten", "Text", "Intro"]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(names(&mut s), ["Cat", "Text", "Intro"]);
    let gone = s.dispatch("deck.slide_remove", &on(json!({"slide": "nope"}))).unwrap_err();
    assert_eq!(gone.kind, ErrorKind::NotFound);
}

#[test]
fn a_stale_revision_is_refused_and_nothing_changes() {
    let mut s = session();
    create(&mut s, &[IMG], &[]);
    let seen = list(&mut s)["revision"].as_u64().unwrap();
    s.dispatch("deck.slide_update", &on(json!({"slide": "slide-1", "name": "A", "expected_revision": seen}))).unwrap();
    let stale = s.dispatch("deck.slide_update", &on(json!({"slide": "slide-1", "name": "B", "expected_revision": seen}))).unwrap_err();
    assert_eq!(stale.kind, ErrorKind::Conflict);
    assert_eq!(names(&mut s), ["A"]);
}

#[test]
fn layers_merge_and_keep_their_kind_and_id() {
    let mut s = session();
    create(&mut s, &[IMG], &[]);
    let layer = format!("src-{IMG}");
    let mut p = on(json!({"slide": "slide-1", "layer": layer, "set": {"x": 10, "content": {"fit": false}}}));
    assert_eq!(s.dispatch("deck.layer_update", &p).unwrap(), json!({"changed": true}));
    let listed = list(&mut s);
    let stored = &listed["slides"][0]["layers"][0];
    assert_eq!((&stored["x"], &stored["content"]["origin"]), (&json!(10.0), &json!(IMG.to_string())));
    assert_ne!(stored["content"]["fit"], json!(true), "fit false is the default, so it may be left out");
    for set in [json!({"content": {"kind": "text"}}), json!({"id": "other"}), json!({"opacity": 2}), json!({"content": {"kind": "image", "input": 0}})] {
        p["set"] = set;
        assert_eq!(s.dispatch("deck.layer_update", &p).unwrap_err().kind, ErrorKind::InvalidInput);
    }
}

#[test]
fn set_keeps_unsupported_layers_and_newer_fields_but_takes_no_new_unknowns() {
    let mut s = session();
    create(&mut s, &[], &[]);
    // A newer NodeCraft stored a slide note and a video layer.
    let stored = r##"{"version":1,"theme":"dark","slides":[{"id":"s1","name":"One","notes":"hi","layers":[
        {"id":"v","name":"Video","x":0,"y":0,"width":1,"height":1,"content":{"kind":"video","clip":"abc"}}]}]}"##;
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    let mut node = canvas.nodes().get(&DECK).unwrap().clone();
    node.config.insert(CONFIG.into(), Value::Text(stored.into()));
    let mut project = s.document().clone();
    Change::SetNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let listed = list(&mut s);
    assert_eq!(listed["slides"][0]["unsupported"], json!([0]));
    // Listing and setting back what was listed keeps it all.
    let back = listed["slides"].clone();
    let slides: Vec<Json> = back.as_array().unwrap().iter().map(|s| json!({"id": s["id"], "name": "Renamed", "layers": s["layers"]})).collect();
    s.dispatch("deck.set", &on(json!({"slides": slides}))).unwrap();
    let after = stored_deck(&s);
    assert_eq!(after.extra.get("theme"), Some(&json!("dark")));
    assert_eq!(after.slides[0].extra.get("notes"), Some(&json!("hi")));
    assert!(matches!(after.slides[0].layers[0], Item::Unsupported(_)));
    // A new unknown layer isn't taken.
    let unknown = json!([{"layers": [{"id": "w", "name": "W", "x": 0, "y": 0, "width": 1, "height": 1, "content": {"kind": "web"}}]}]);
    assert_eq!(s.dispatch("deck.set", &on(json!({"slides": unknown}))).unwrap_err().kind, ErrorKind::InvalidInput);
    // A layer that names another's id isn't taken either.
    let twice = json!([{"layers": [back[0]["layers"][0], {"id": "v", "name": "V", "x": 0, "y": 0, "width": 1, "height": 1, "content": {"kind": "shape", "shape": "rect"}}]}]);
    assert_eq!(s.dispatch("deck.set", &on(json!({"slides": twice}))).unwrap_err().kind, ErrorKind::InvalidInput);
}

fn stored_deck(s: &Session) -> Deck {
    stored(s.document().canvases().get(&CANVAS).unwrap().nodes().get(&DECK).unwrap()).unwrap()
}

#[test]
fn a_newer_deck_is_listed_read_only_and_never_changed() {
    let mut s = session();
    create(&mut s, &[], &[]);
    let mut node = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&DECK).unwrap().clone();
    node.config.insert(CONFIG.into(), Value::Text(r#"{"version":2,"slides":[{"pages":1}]}"#.into()));
    let mut project = s.document().clone();
    Change::SetNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let listed = list(&mut s);
    assert_eq!((&listed["writable"], &listed["version"], &listed["count"]), (&json!(false), &json!(2), &json!(0)));
    for (command, extra) in [
        ("deck.set", json!({"slides": []})),
        ("deck.slide_add", json!({"slide": {}})),
        ("deck.slide_remove", json!({"slide": "a"})),
        ("deck.slide_move", json!({"slide": "a", "index": 0})),
        ("deck.slide_update", json!({"slide": "a"})),
        ("deck.layer_update", json!({"slide": "a", "layer": "b", "set": {}})),
    ] {
        assert_eq!(s.dispatch(command, &on(extra)).unwrap_err().kind, ErrorKind::Unsupported, "{command}");
    }
}

#[test]
fn sources_follow_their_node() {
    let mut s = session();
    create(&mut s, &[TXT], &[]);
    s.dispatch("node.remove", &json!({"canvas": CANVAS, "ids": [TXT]})).or_else(|_| s.dispatch("node.remove", &json!({"canvas": CANVAS, "id": TXT}))).unwrap();
    assert_eq!(list(&mut s)["slides"][0]["sources"][0]["shows"], json!("missing"), "the layer is kept, drawn empty");
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    assert!(matches!(shown(canvas, "not an id", 0), Shown::Missing));
    assert!(matches!(shown(canvas, &IMG.to_string(), 1), Shown::Missing));
    assert!(matches!(shown(canvas, &IMG.to_string(), u32::MAX), Shown::Missing));
}

#[test]
fn only_a_deck_card_has_slides() {
    let mut s = session();
    let wrong = s.dispatch("deck.list", &json!({"canvas": CANVAS, "id": IMG})).unwrap_err();
    assert_eq!(wrong.kind, ErrorKind::InvalidInput);
    let gone = s.dispatch("deck.list", &json!({"canvas": CANVAS, "id": DECK})).unwrap_err();
    assert_eq!(gone.kind, ErrorKind::NotFound);
}
