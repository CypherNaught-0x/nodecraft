use super::*;
use crate::Session;
use nodecraft_types::{EdgeId, ProjectId};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([3; 16]);
const B: NodeId = NodeId::from_bytes([4; 16]);
const EDITOR: NodeId = NodeId::from_bytes([5; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    for (id, kind, x) in [(A, "source.image", 0), (B, "source.image", 0), (EDITOR, ID, 400)] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": x, "y": 0}})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": EDITOR, "name": "width", "value": Value::Unsigned(nodecraft_types::Unsigned(400))})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": EDITOR, "name": "height", "value": Value::Unsigned(nodecraft_types::Unsigned(200))})).unwrap();
    for (n, from) in [(1u8, A), (2, B)] {
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": INPUT}, "order": n});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }
    s
}

fn list(s: &mut Session) -> Vec<Json> {
    let listed = s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!((&listed["width"], &listed["height"]), (&json!(400), &json!(200)));
    listed["layers"].as_array().unwrap().clone()
}

fn ids(s: &mut Session) -> Vec<String> {
    list(s).iter().map(|l| l["id"].as_str().unwrap().to_owned()).collect()
}

fn title() -> Json {
    json!({"id": "title", "name": "Title", "x": 100, "y": 50, "width": 120, "height": 40, "content": {"kind": "text", "text": "Hi", "size": 30, "color": "#112233"}})
}

fn on(layer: &str) -> Json {
    json!({"canvas": CANVAS, "id": EDITOR, "layer": layer})
}

#[test]
fn each_wired_image_is_listed_as_a_fitted_layer_until_it_is_stored() {
    let mut s = session();
    let (a, b) = (layers::auto_id(&A.to_string(), 0), layers::auto_id(&B.to_string(), 0));
    assert_eq!(ids(&mut s), [a.clone(), b.clone()]);
    let first = list(&mut s).remove(0);
    assert_eq!(first["content"], json!({"kind": "source", "origin": A.to_string(), "nth": 0, "fit": true}));
    assert_eq!((&first["x"], &first["width"]), (&json!(200.0), &json!(400.0)));
    // Adding a text layer stores the image layers below it, in place.
    let added = s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    assert_eq!(added, json!({"changed": true, "layer": "title"}));
    assert_eq!(ids(&mut s), [a.clone(), b.clone(), "title".into()]);
    assert_eq!(stored(&node(&s)).unwrap().layers.len(), 3);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().layers.len(), 0, "one undo step");
    // A layer id is unique, and only text and shapes are added.
    s.dispatch("edit.redo", &json!({})).unwrap();
    let again = s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap_err();
    assert_eq!(again.kind, ErrorKind::Conflict);
    let mut image = title();
    image["id"] = json!("img");
    image["content"] = json!({"kind": "source", "origin": "x", "nth": 0});
    assert!(s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": image})).is_err());
}

fn node(s: &Session) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&EDITOR).unwrap().clone()
}

#[test]
fn update_merges_fields_and_respects_the_lock() {
    let mut s = session();
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    let mut change = on("title");
    change["set"] = json!({"x": 10, "rotation": 90, "content": {"text": "Hello", "align": "center"}});
    assert_eq!(s.dispatch("layers.update", &change).unwrap(), json!({"changed": true}));
    let layer = stored(&node(&s)).unwrap().get("title").cloned().unwrap();
    assert_eq!((layer.x, layer.rotation), (10.0, 90.0));
    assert!(matches!(&layer.content, Content::Text { text, size, align: layers::Align::Center, .. } if text == "Hello" && *size == 30.0));
    // The kind and the id stay.
    for set in [json!({"content": {"kind": "shape"}}), json!({"id": "other"}), json!({"opacity": 2})] {
        change["set"] = set;
        assert_eq!(s.dispatch("layers.update", &change).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    // Locked: no moving, but renaming and unlocking-while-moving work.
    change["set"] = json!({"locked": true});
    s.dispatch("layers.update", &change).unwrap();
    change["set"] = json!({"y": 5});
    assert!(s.dispatch("layers.update", &change).is_err());
    change["set"] = json!({"name": "Heading"});
    s.dispatch("layers.update", &change).unwrap();
    change["set"] = json!({"locked": false, "y": 5});
    s.dispatch("layers.update", &change).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().get("title").map(|l| (l.name.as_str(), l.y, l.locked)), Some(("Heading", 5.0, false)));
    // The same value again changes nothing.
    assert_eq!(s.dispatch("layers.update", &change).unwrap(), json!({"changed": false}));
    // Changing an automatic image layer stores it.
    let a = layers::auto_id(&A.to_string(), 0);
    let mut hide = on(&a);
    hide["set"] = json!({"visible": false});
    s.dispatch("layers.update", &hide).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().get(&a).map(|l| l.visible), Some(false));
}

#[test]
fn reorder_and_remove_keep_image_layers_while_they_are_wired() {
    let mut s = session();
    let (a, b) = (layers::auto_id(&A.to_string(), 0), layers::auto_id(&B.to_string(), 0));
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    let mut to = on("title");
    to["index"] = json!(0);
    s.dispatch("layers.reorder", &to).unwrap();
    assert_eq!(ids(&mut s), ["title".to_owned(), a.clone(), b.clone()]);
    let mut top = on(&a);
    top["index"] = json!(64);
    s.dispatch("layers.reorder", &top).unwrap();
    assert_eq!(ids(&mut s), ["title".to_owned(), b.clone(), a.clone()]);
    assert_eq!(s.dispatch("layers.remove", &on(&a)).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("layers.remove", &on("nope")).unwrap_err().kind, ErrorKind::NotFound);
    s.dispatch("layers.remove", &on("title")).unwrap();
    assert_eq!(ids(&mut s), [b.clone(), a.clone()]);
    // Unwiring an image keeps its stored layer listed (not drawn) until it is deleted.
    s.dispatch("edge.disconnect", &json!({"canvas": CANVAS, "id": EdgeId::from_bytes([1; 16])})).unwrap();
    assert_eq!(ids(&mut s), [b.clone(), a.clone()]);
    s.dispatch("layers.remove", &on(&a)).unwrap();
    assert_eq!(ids(&mut s), [b]);
}

#[test]
fn align_moves_by_bounds_against_the_frame_or_the_selection() {
    let mut s = session();
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    let mut square = title();
    square["id"] = json!("sq");
    square["x"] = json!(300);
    square["y"] = json!(150);
    square["width"] = json!(20);
    square["height"] = json!(20);
    square["rotation"] = json!(45);
    square["content"] = json!({"kind": "shape", "shape": "rectangle", "fill": "#ff0000"});
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": square})).unwrap();
    let align =
        |s: &mut Session, layers: Json, to: &str| s.dispatch("layers.align", &json!({"canvas": CANVAS, "id": EDITOR, "layers": layers, "to": to})).unwrap();
    // One layer: against the frame.
    align(&mut s, json!(["title"]), "right");
    let title = stored(&node(&s)).unwrap().get("title").cloned().unwrap();
    assert!((title.bounds().2 - 400.0).abs() < 1e-9 && title.y == 50.0, "{:?}", title.bounds());
    // Several: against their joint box, rotation included (the square's corners reach 14.14 px).
    align(&mut s, json!(["title", "sq"]), "top");
    let layers = stored(&node(&s)).unwrap();
    let top = |id: &str| layers.get(id).map(|l| l.bounds().1).unwrap();
    assert!((top("title") - 30.0).abs() < 1e-9 && (top("sq") - 30.0).abs() < 1e-9, "{} {}", top("title"), top("sq"));
    // A locked layer stays and is reported.
    let mut lock = on("sq");
    lock["set"] = json!({"locked": true});
    s.dispatch("layers.update", &lock).unwrap();
    let result = align(&mut s, json!(["title", "sq"]), "left");
    assert_eq!(result["locked"], json!(["sq"]));
    assert!(s.dispatch("layers.align", &json!({"canvas": CANVAS, "id": EDITOR, "layers": [], "to": "left"})).is_err());
}

#[test]
fn only_a_layer_editor_has_layers_and_bad_layers_are_refused() {
    let mut s = session();
    assert_eq!(s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": A})).unwrap_err().kind, ErrorKind::InvalidInput);
    let mut huge = title();
    huge["width"] = json!(1e9);
    assert!(s.dispatch("node.layers_set", &json!({"canvas": CANVAS, "id": EDITOR, "layers": [huge]})).is_err());
    let set = json!({"canvas": CANVAS, "id": EDITOR, "layers": [title()]});
    assert_eq!(s.dispatch("node.layers_set", &set).unwrap(), json!({"changed": true}));
    assert_eq!(s.dispatch("node.layers_set", &set).unwrap(), json!({"changed": false}));
    // A node whose stored layers are corrupt reports it rather than listing nothing.
    let mut corrupt = node(&s);
    corrupt.config.insert(CONFIG.into(), Value::Text("{\"layers\": 3}".into()));
    let project = s.document().clone();
    let mut project = project;
    Change::SetNode { canvas: CANVAS, node: corrupt }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    assert!(s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).is_err());
}
