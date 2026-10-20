use super::*;
use crate::Session;
use nodecraft_graph::Position;
use nodecraft_types::{CanvasId, ProjectId};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMAGE: NodeId = NodeId::from_bytes([3; 16]);
const EXPANDED: NodeId = NodeId::from_bytes([4; 16]);
const MASK: NodeId = NodeId::from_bytes([5; 16]);
const EDIT: NodeId = NodeId::from_bytes([6; 16]);

fn edge(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": IMAGE, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

fn created() -> Session {
    let mut s = session();
    let create = json!({"canvas": CANVAS, "from": {"node": IMAGE, "port": "out"}, "expand": EXPANDED, "mask": MASK, "edges": [edge(1), edge(2)], "pos": {"x": 400, "y": 0}});
    assert_eq!(s.dispatch("mask.create", &create).unwrap(), json!({"expand": EXPANDED, "mask": MASK}));
    s
}

fn node(s: &Session, id: NodeId) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&id).unwrap().clone()
}

fn edit(prompt: &str, model: Option<&str>) -> Json {
    let mut p = json!({"canvas": CANVAS, "mask": MASK, "id": EDIT, "edges": [edge(3), edge(4)], "pos": {"x": 1200, "y": 0}, "prompt": prompt});
    if let Some(model) = model {
        p["model"] = json!(model);
    }
    p
}

const STROKE: &str = r#"[{"size": 0.2, "hardness": 0.8, "points": [[0.2, 0.2], [0.6, 0.4]]}]"#;

#[test]
fn create_chains_expand_and_mask_behind_the_image_in_one_step() {
    let mut s = created();
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    assert_eq!(canvas.nodes().len(), 3);
    assert_eq!(node(&s, EXPANDED).kind, EXPAND);
    let mask = node(&s, MASK);
    assert_eq!(mask.params.get("transparent"), Some(&Value::Bool(true)), "an expanded canvas's margins are masked");
    let wires: Vec<(NodeId, NodeId)> = canvas.edges().values().map(|e| (e.from.node, e.to.node)).collect();
    assert!(wires.contains(&(IMAGE, EXPANDED)) && wires.contains(&(EXPANDED, MASK)), "{wires:?}");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document().canvases().get(&CANVAS).unwrap().nodes().len(), 1, "one undo step");
    // Only from one image output.
    let bad = json!({"canvas": CANVAS, "from": {"node": IMAGE, "port": "nope"}, "expand": EXPANDED, "mask": MASK, "edges": [edge(1), edge(2)], "pos": {"x": 0, "y": 0}});
    assert_eq!(s.dispatch("mask.create", &bad).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn strokes_are_stored_as_one_undo_step_and_bad_ones_change_nothing() {
    let mut s = created();
    let set = json!({"canvas": CANVAS, "id": MASK, "strokes": serde_json::from_str::<Json>(STROKE).unwrap()});
    assert_eq!(s.dispatch("node.mask_set", &set).unwrap(), json!({"changed": true}));
    assert!(strokes(&node(&s, MASK)).unwrap().paints());
    assert_eq!(s.dispatch("node.mask_set", &set).unwrap(), json!({"changed": false}));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(strokes(&node(&s, MASK)).unwrap(), Strokes::default());

    let before = s.document().clone();
    for strokes in [
        json!([{"size": 0, "hardness": 0.5, "points": [[0.5, 0.5]]}]),
        json!([{"size": 0.1, "hardness": 0.5, "points": [[9, 0.5]]}]),
        json!([{"size": 0.1, "hardness": 0.5, "points": []}]),
        json!([{"size": 0.1, "hardness": 0.5, "points": [[0.5, 0.5]], "colour": 1}]),
    ] {
        assert!(s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": MASK, "strokes": strokes})).is_err(), "{strokes}");
    }
    assert!(s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": IMAGE, "strokes": []})).is_err(), "only mask nodes");
    assert_eq!(s.document(), &before);
}

#[test]
fn an_edit_needs_something_marked_and_wires_image_and_mask_into_a_model_that_takes_one() {
    let mut s = created();
    let error = s.dispatch("mask.edit", &edit("a red door", None)).unwrap_err();
    assert!(error.message.contains("paint the area"), "{}", error.message);

    // Margins alone are enough: the transparent edge is the area to fill.
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": EXPANDED, "name": "right", "value": {"type": "unsigned", "value": "256"}})).unwrap();
    assert!(s.dispatch("mask.edit", &edit("more beach", None)).is_ok());
    s.dispatch("edit.undo", &json!({})).unwrap();

    s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": MASK, "strokes": serde_json::from_str::<Json>(STROKE).unwrap()})).unwrap();
    let models = inpaint_models(&GenerationCatalog::builtin().unwrap());
    assert!(!models.is_empty(), "the builtin catalog has inpainting models");
    assert_eq!(s.dispatch("mask.edit", &edit("a red door", None)).unwrap(), json!({"id": EDIT}));
    let generate = node(&s, EDIT);
    assert_eq!(generate.kind, KIND);
    assert_eq!(generate.params.get(MODEL), Some(&Value::Text(models[0].clone())));
    assert_eq!(generate.params.get(PROMPT), Some(&Value::Text("a red door".into())));
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    let into: Vec<(NodeId, String)> = canvas.edges().values().filter(|e| e.to.node == EDIT).map(|e| (e.from.node, e.to.port.clone())).collect();
    assert!(into.contains(&(EXPANDED, "image".into())) && into.contains(&(MASK, "mask".into())), "{into:?}");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.document().canvases().get(&CANVAS).unwrap().nodes().get(&EDIT).is_none(), "one undo step");

    // A model without a mask input is refused.
    assert_eq!(s.dispatch("mask.edit", &edit("x", Some("nope"))).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn the_mask_and_expand_actions_render_through_the_host() {
    use nodecraft_exec::run::ActionExecutor;
    let mut s = created();
    s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": MASK, "strokes": serde_json::from_str::<Json>(STROKE).unwrap()})).unwrap();
    let png = nodecraft_media::encode_png(8, 4, &[200u8, 100, 50, 255].repeat(32), &nodecraft_media::Limits::default()).unwrap();
    let mut assets = nodecraft_format::AssetStore::default();
    let hash = assets.insert(&png).unwrap();
    let source = Value::Media(nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() });
    let actions = crate::action_exec::LocalActions::new(assets);
    let mut margins = ParamValues::new();
    for (side, v) in [("left", 2u64), ("top", 0), ("right", 0), ("bottom", 0)] {
        margins.insert(side.into(), Value::Unsigned(nodecraft_types::Unsigned(v)));
    }
    let Value::Media(expanded) = actions.execute(EXPAND, &margins, std::slice::from_ref(&source)).unwrap() else { panic!() };
    assert_eq!((expanded.meta.width, expanded.meta.height), (Some(10), Some(4)));

    let mask = node(&s, MASK);
    let mut params = s.kinds().get(ID).unwrap().validate_instance(&mask, true).unwrap().into_visible_values();
    params.insert(nodecraft_nodes::mask::SPEC_PARAM.into(), Value::Text(nodecraft_nodes::mask::stored(&mask).unwrap().into()));
    let Value::Media(out) = actions.execute(ID, &params, &[Value::Media(expanded)]).unwrap() else { panic!() };
    assert_eq!((out.meta.width, out.meta.height), (Some(10), Some(4)));
    params.insert(nodecraft_nodes::mask::SPEC_PARAM.into(), Value::Text("{".into()));
    assert!(actions.execute(ID, &params, &[source]).unwrap_err().contains("mask can't be read"));
}

/// A mask over an imported 100×50 image (so its size is known), with one stroke at the centre.
fn painted() -> Session {
    let mut s = session();
    let png = nodecraft_media::encode_png(100, 50, &[90u8; 100 * 50 * 4], &nodecraft_media::Limits::default()).unwrap();
    let picture = NodeId::from_bytes([7; 16]);
    let incoming = [crate::import::Incoming { name: "pic.png", bytes: &png }];
    crate::import::import_files(&mut s, CANVAS, Position { x: 0.0, y: 300.0 }, 300.0, &incoming, &[picture]).unwrap();
    let create = json!({"canvas": CANVAS, "from": {"node": picture, "port": "out"}, "expand": EXPANDED, "mask": MASK, "edges": [edge(1), edge(2)], "pos": {"x": 400, "y": 0}});
    s.dispatch("mask.create", &create).unwrap();
    let stroke = json!([{"size": 0.2, "hardness": 1.0, "points": [[0.5, 0.5]]}]);
    s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": MASK, "strokes": stroke})).unwrap();
    s
}

fn margins_json(left: u32, right: u32) -> Json {
    json!({"canvas": CANVAS, "id": EXPANDED, "margins": {"left": left, "top": 0, "right": right, "bottom": 0}})
}

fn only_stroke(s: &Session) -> Stroke {
    strokes(&node(s, MASK)).unwrap().strokes.into_iter().next().unwrap()
}

#[test]
fn new_margins_move_painted_strokes_to_keep_them_over_the_same_pixels() {
    let mut s = painted();
    // 100×50 → 200×50: the centre is now a quarter across, and the brush keeps its 20 px.
    assert_eq!(s.dispatch("mask.margins_set", &margins_json(0, 100)).unwrap(), json!({"changed": true}));
    let moved = only_stroke(&s);
    assert!((moved.points[0][0] - 0.25).abs() < 1e-9 && (moved.points[0][1] - 0.5).abs() < 1e-9, "{moved:?}");
    assert!((moved.size - 0.1).abs() < 1e-9, "{moved:?}");
    assert_eq!(margins(&node(&s, EXPANDED)), Margins { right: 100, ..Margins::default() });
    // The margin moves to the left: the image (and the stroke) moves right.
    s.dispatch("mask.margins_set", &margins_json(100, 0)).unwrap();
    assert!((only_stroke(&s).points[0][0] - 0.75).abs() < 1e-9);
    assert_eq!(s.dispatch("mask.margins_set", &margins_json(100, 0)).unwrap(), json!({"changed": false}));
    // One undo step puts both the margins and the strokes back.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!((only_stroke(&s).points[0][0] - 0.25).abs() < 1e-9);
    assert_eq!(margins(&node(&s, EXPANDED)).right, 100);

    let before = s.document().clone();
    assert!(s.dispatch("mask.margins_set", &margins_json(4097, 0)).is_err());
    assert!(
        s.dispatch("mask.margins_set", &json!({"canvas": CANVAS, "id": MASK, "margins": {"left": 1, "top": 0, "right": 0, "bottom": 0}})).is_err(),
        "only Expand nodes"
    );
    assert_eq!(s.document(), &before);
}

#[test]
fn margins_refuse_to_guess_where_strokes_go_without_the_image_size() {
    let mut s = created();
    // Nothing painted: the margins change on their own.
    s.dispatch("mask.margins_set", &margins_json(0, 64)).unwrap();
    s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": MASK, "strokes": serde_json::from_str::<Json>(STROKE).unwrap()})).unwrap();
    let before = s.document().clone();
    let error = s.dispatch("mask.margins_set", &margins_json(0, 128)).unwrap_err();
    assert!(error.message.contains("run the image first"), "{}", error.message);
    assert_eq!(s.document(), &before);
}

#[test]
fn an_inverted_mask_marks_everything_and_an_erased_one_marks_nothing() {
    let mut s = created();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": MASK, "name": "invert", "value": {"type": "bool", "value": true}})).unwrap();
    assert!(s.dispatch("mask.edit", &edit("a new sky", None)).is_ok());
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    let erased = json!([{"size": 0.1, "hardness": 1.0, "points": [[0.5, 0.5]]}, {"size": 0.3, "hardness": 1.0, "erase": true, "points": [[0.5, 0.5]]}]);
    s.dispatch("node.mask_set", &json!({"canvas": CANVAS, "id": MASK, "strokes": erased})).unwrap();
    assert!(s.dispatch("mask.edit", &edit("a door", None)).unwrap_err().message.contains("paint the area"));
}
