use crate::library::MemoryLibrary;
use crate::shader_library;
use crate::{Session, commands::Registry};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::shader::{Pin, pinned, stored};
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const CARD: NodeId = NodeId::from_bytes([2; 16]);
const PLACED: NodeId = NodeId::from_bytes([3; 16]);

const TINT: &str = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return color * tint();\n}\n";

fn session(store: &MemoryLibrary) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([8; 16]), "Shaders").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap();
    shader_library::install(&mut s, Box::new(store.clone())).unwrap();
    s.dispatch("node.create", &json!({"canvas": C, "id": CARD, "kind": "image.shader", "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

fn card(s: &Session, id: NodeId) -> nodecraft_graph::Node {
    s.document().canvases().get(&C).and_then(|canvas| canvas.nodes().get(&id)).cloned().unwrap()
}

fn tint(s: &mut Session) {
    let params = json!([{"name": "tint", "label": "Tint", "min": 0.0, "max": 2.0, "default": 1.0}]);
    s.dispatch("node.shader_set", &json!({"canvas": C, "id": CARD, "source": TINT, "params": params})).unwrap();
}

fn save(s: &mut Session, name: Option<&str>) -> Json {
    let mut params = json!({"canvas": C, "id": CARD});
    if let Some(name) = name {
        params["name"] = json!(name);
    }
    s.dispatch("shader.save", &params).unwrap()
}

#[test]
fn a_saved_shader_is_placed_in_another_project_and_cards_follow_new_versions() {
    let store = MemoryLibrary::default();
    let mut s = session(&store);
    assert_eq!(save(&mut s, Some("Invert")), json!({"name": "Invert", "version": 1, "saved": true}));
    assert_eq!(
        s.dispatch("shader.update", &json!({"canvas": C, "id": CARD, "name": "Invert", "version": 1})).unwrap()["from"],
        json!({"name": "Invert", "version": 1})
    );
    assert_eq!(pinned(&card(&s, CARD)), Some(Pin { name: "Invert".into(), version: 1 }));
    // A hand edit keeps where it came from; saving without a name saves the next version of it.
    tint(&mut s);
    assert_eq!(pinned(&card(&s, CARD)).map(|pin| pin.version), Some(1));
    assert_eq!(save(&mut s, None), json!({"name": "Invert", "version": 2, "saved": true}));

    let mut other = session(&store);
    assert_eq!(other.dispatch("library.shaders", &json!({})).unwrap(), json!({"shaders": [{"name": "Invert", "versions": [1, 2], "latest": 2}]}));
    let placed = other.dispatch("shader.place", &json!({"canvas": C, "name": "invert", "version": 1, "id": PLACED, "pos": {"x": 40, "y": 0}})).unwrap();
    assert_eq!(placed, json!({"id": PLACED, "name": "Invert", "version": 1}));
    let node = card(&other, PLACED);
    assert_eq!((node.title.as_str(), pinned(&node)), ("Invert", Some(Pin { name: "Invert".into(), version: 1 })));
    assert!(node.params.contains_key("amount"), "the default shader's slider");
    // Updating with no version moves it to the latest, as one undo step.
    other.dispatch("shader.update", &json!({"canvas": C, "id": PLACED})).unwrap();
    let node = card(&other, PLACED);
    assert_eq!((stored(&node).unwrap().source.as_str(), pinned(&node).map(|pin| pin.version)), (TINT, Some(2)));
    assert!(!node.params.contains_key("amount"), "a value for a parameter it no longer declares goes");
    other.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(pinned(&card(&other, PLACED)).map(|pin| pin.version), Some(1));
    // Up to date already: nothing changes.
    other.dispatch("shader.update", &json!({"canvas": C, "id": PLACED})).unwrap();
    assert_eq!(other.dispatch("shader.update", &json!({"canvas": C, "id": PLACED})).unwrap()["changed"], json!(false));
}

#[test]
fn the_pin_doesnt_make_results_stale() {
    let store = MemoryLibrary::default();
    let mut s = session(&store);
    tint(&mut s);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let hash = |s: &Session| nodecraft_exec::staleness::input_hash(s.document().canvases().get(&C).unwrap(), &kinds, CARD).unwrap();
    let before = hash(&s);
    save(&mut s, Some("Invert"));
    s.dispatch("shader.update", &json!({"canvas": C, "id": CARD, "name": "Invert"})).unwrap();
    assert!(pinned(&card(&s, CARD)).is_some());
    assert_eq!(hash(&s), before);
}

#[test]
fn bad_requests_say_why() {
    let store = MemoryLibrary::default();
    let mut s = session(&store);
    let error = |s: &mut Session, id: &str, params: Json| s.dispatch(id, &params).unwrap_err().kind;
    assert_eq!(error(&mut s, "shader.save", json!({"canvas": C, "id": CARD})), ErrorKind::InvalidInput, "no name and no library shader");
    assert_eq!(error(&mut s, "shader.save", json!({"canvas": C, "id": CARD, "name": " x"})), ErrorKind::InvalidInput);
    assert_eq!(error(&mut s, "shader.place", json!({"canvas": C, "name": "Nope", "id": PLACED, "pos": {"x": 0, "y": 0}})), ErrorKind::NotFound);
    assert_eq!(error(&mut s, "shader.update", json!({"canvas": C, "id": CARD})), ErrorKind::InvalidInput, "the card came from nowhere");
    assert_eq!(error(&mut s, "library.remove_shader", json!({"name": "Nope"})), ErrorKind::NotFound);
    save(&mut s, Some("Invert"));
    assert_eq!(error(&mut s, "shader.update", json!({"canvas": C, "id": CARD, "name": "Invert", "version": 9})), ErrorKind::NotFound);
    assert_eq!(error(&mut s, "shader.place", json!({"canvas": C, "name": "Invert", "id": PLACED, "pos": {"x": f32::MAX, "y": 0}})), ErrorKind::InvalidInput);
    s.dispatch("node.create", &json!({"canvas": C, "id": PLACED, "kind": "source.text", "pos": {"x": 0, "y": 0}})).unwrap();
    assert_eq!(error(&mut s, "shader.save", json!({"canvas": C, "id": PLACED, "name": "Text"})), ErrorKind::InvalidInput);
    assert_eq!(error(&mut s, "shader.update", json!({"canvas": C, "id": PLACED, "name": "Invert"})), ErrorKind::InvalidInput);
}

#[test]
fn without_a_library_the_commands_are_unavailable() {
    let mut project = Project::new(ProjectId::from_bytes([8; 16]), "Bare").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap();
    for (id, params) in [("library.shaders", json!({})), ("shader.place", json!({"canvas": C, "name": "A", "id": PLACED, "pos": {"x": 0, "y": 0}}))] {
        assert_eq!(s.dispatch(id, &params).unwrap_err().kind, ErrorKind::Unsupported, "{id}");
    }
}
