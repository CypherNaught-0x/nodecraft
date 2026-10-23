use super::*;
use crate::Session;
use nodecraft_graph::Node;
use nodecraft_types::ProjectId;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const MODEL: NodeId = NodeId::from_bytes([3; 16]);
const IMAGE: NodeId = NodeId::from_bytes([4; 16]);
const CAPTURE: NodeId = NodeId::from_bytes([5; 16]);
const EDGE: EdgeId = EdgeId::from_bytes([6; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": MODEL, "kind": "source.mesh", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": IMAGE, "kind": "source.image", "pos": {"x": 0, "y": 400}})).unwrap();
    s
}

fn capture(shot: &str, view: Json) -> Json {
    let mut p = json!({"canvas": CANVAS, "from": {"node": MODEL, "port": "out"}, "shot": shot, "id": CAPTURE, "edge": EDGE, "pos": {"x": 400, "y": 0}});
    if let (Some(p), Json::Object(view)) = (p.as_object_mut(), view) {
        p.extend(view);
    }
    p
}

fn node(s: &Session, id: NodeId) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&id).unwrap().clone()
}

fn param(node: &Node, name: &str) -> Option<f64> {
    match node.params.get(name) {
        Some(Value::Number(n)) => Some(n.get()),
        _ => None,
    }
}

#[test]
fn a_photo_takes_the_viewers_camera_and_wires_to_the_model_in_one_step() {
    let mut s = session();
    let view = json!({"yaw": 200.4, "pitch": 30.6, "zoom": 2.33, "style": "wireframe"});
    assert_eq!(s.dispatch("mesh.capture", &capture("photo", view)).unwrap(), json!({"id": CAPTURE}));
    let photo = node(&s, CAPTURE);
    assert_eq!(photo.kind, PHOTO);
    // 200° is -160°: the angle setting runs from -180 to 180, in whole degrees.
    assert_eq!((param(&photo, "yaw"), param(&photo, "pitch"), param(&photo, "zoom")), (Some(-160.0), Some(31.0), Some(2.35)));
    assert_eq!(photo.params.get("style"), Some(&Value::Text("wireframe".into())));
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    let edge = canvas.edges().get(&EDGE).unwrap();
    assert_eq!((edge.from.node, edge.to.node, edge.to.port.as_str()), (MODEL, CAPTURE, "model"));
    s.dispatch("edit.undo", &json!({})).unwrap();
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    assert!(canvas.nodes().get(&CAPTURE).is_none() && canvas.edges().is_empty(), "one undo step");
}

#[test]
fn a_view_sheet_starts_at_the_viewers_angle_and_steep_or_close_views_are_kept_in_range() {
    let mut s = session();
    s.dispatch("mesh.capture", &capture("views", json!({"yaw": -30.0, "pitch": 89.0, "zoom": 99.0}))).unwrap();
    let sheet = node(&s, CAPTURE);
    assert_eq!(sheet.kind, VIEWS);
    assert_eq!((param(&sheet, "start"), param(&sheet, "pitch"), param(&sheet, "zoom")), (Some(-30.0), Some(MAX_PITCH), Some(16.0)));
    assert_eq!(sheet.params.get("count"), Some(&Value::Text("8".into())), "the default count");
}

#[test]
fn omitted_view_settings_keep_the_defaults() {
    let mut s = session();
    s.dispatch("mesh.capture", &capture("photo", json!({}))).unwrap();
    let photo = node(&s, CAPTURE);
    assert_eq!((param(&photo, "yaw"), param(&photo, "zoom")), (Some(34.0), Some(1.0)));
}

#[test]
fn only_a_3d_output_is_captured_and_hostile_numbers_are_refused() {
    let mut s = session();
    let image = json!({"canvas": CANVAS, "from": {"node": IMAGE, "port": "out"}, "shot": "photo", "id": CAPTURE, "edge": EDGE, "pos": {"x": 0, "y": 0}});
    assert_eq!(s.dispatch("mesh.capture", &image).unwrap_err().kind, ErrorKind::InvalidInput);
    let missing = json!({"canvas": CANVAS, "from": {"node": MODEL, "port": "nope"}, "shot": "photo", "id": CAPTURE, "edge": EDGE, "pos": {"x": 0, "y": 0}});
    assert_eq!(s.dispatch("mesh.capture", &missing).unwrap_err().kind, ErrorKind::NotFound);
    assert!(s.dispatch("mesh.capture", &capture("photo", json!({"pos": {"x": 1e30, "y": 0}}))).is_err());
    assert!(s.document().canvases().get(&CANVAS).unwrap().nodes().get(&CAPTURE).is_none());
    // A huge but finite angle is still turned into range rather than refused.
    s.dispatch("mesh.capture", &capture("photo", json!({"yaw": 1e300, "pitch": -1e300, "zoom": -5.0}))).unwrap();
    let photo = node(&s, CAPTURE);
    let yaw = param(&photo, "yaw").unwrap();
    assert!((-180.0..=180.0).contains(&yaw), "{yaw}");
    assert_eq!((param(&photo, "pitch"), param(&photo, "zoom")), (Some(-MAX_PITCH), Some(0.25)));
}
#[test]
fn a_turntable_saves_the_view_and_undo_redo_restore_its_node_and_edge_together() {
    let mut s = session();
    s.dispatch("mesh.capture", &capture("turntable", json!({"yaw":200.4,"pitch":30.6,"zoom":2.33,"style":"wireframe"}))).unwrap();
    let saved = node(&s, CAPTURE);
    assert_eq!(saved.kind, TURNTABLE);
    assert_eq!(saved.outputs.first().unwrap().port_type.modality, Modality::Video);
    assert_eq!((param(&saved, "start"), param(&saved, "pitch"), param(&saved, "zoom")), (Some(-160.0), Some(31.0), Some(2.35)));
    assert_eq!(saved.params.get("style"), Some(&Value::Text("wireframe".into())));
    s.dispatch("edit.undo", &json!({})).unwrap();
    let canvas = s.document().canvases().get(&CANVAS).unwrap();
    assert!(canvas.nodes().get(&CAPTURE).is_none() && canvas.edges().is_empty());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(node(&s, CAPTURE), saved);
    assert_eq!(s.document().canvases().get(&CANVAS).unwrap().edges().len(), 1);
}
