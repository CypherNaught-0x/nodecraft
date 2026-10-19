use crate::Session;
use crate::live_actions::{apply, compute, pending};
use nodecraft_nodes::Registry;
use nodecraft_types::{AssetRef, CanvasId, MediaMeta, NodeId, Number, Value};
use serde_json::json;
use std::collections::BTreeMap;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([10; 16]);
const BLUR: NodeId = NodeId::from_bytes([11; 16]);
const TWIN: NodeId = NodeId::from_bytes([12; 16]);

fn png() -> Vec<u8> {
    let pixels: Vec<u8> = (0..16 * 12).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(16, 12, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

fn connect(s: &mut Session, edge: u8, to: NodeId) {
    let edge = nodecraft_types::EdgeId::from_bytes([edge; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": SOURCE, "port": "out"}, "to": {"node": to, "port": "image"}, "order": 0}}),
    )
    .unwrap();
}

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let hash = s.import_asset(&png()).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() };
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": SOURCE, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": SOURCE, "name": "asset", "value": Value::Media(asset)})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": BLUR, "kind": "image.blur", "pos": {"x": 400, "y": 0}})).unwrap();
    connect(&mut s, 20, BLUR);
    s
}

/// Live rounds until nothing is pending; how many rounds ran.
fn settle(s: &mut Session) -> usize {
    let kinds = Registry::builtins().unwrap();
    let mut rounds = 0;
    while let Some(job) = pending(s, CANVAS, &BTreeMap::new()).unwrap() {
        let outcome = compute(job, &kinds).unwrap();
        assert!(apply(s, outcome).unwrap(), "the round recorded");
        rounds += 1;
        assert!(rounds < 10);
    }
    rounds
}

fn node(s: &Session, id: NodeId) -> nodecraft_graph::Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&id).unwrap().clone()
}

fn sigma(s: &mut Session, value: f64) {
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(Number::new(value).unwrap())})).unwrap();
}

#[test]
fn a_setting_changed_back_reuses_its_earlier_result() {
    let mut s = session();
    settle(&mut s);
    let first = node(&s, BLUR).active.unwrap();
    sigma(&mut s, 7.0);
    settle(&mut s);
    assert_ne!(node(&s, BLUR).active, Some(first));
    sigma(&mut s, 2.0);
    let assets = s.assets().len();
    settle(&mut s);
    let blur = node(&s, BLUR);
    assert_eq!(blur.active, Some(first), "the earlier result, selected");
    assert_eq!(blur.history.len(), 2, "nothing new made");
    assert_eq!(s.assets().len(), assets, "no new image");
}

#[test]
fn a_twin_node_copies_the_result_and_the_switch_never_makes_it_stale() {
    let mut s = session();
    settle(&mut s);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": TWIN, "kind": "image.blur", "pos": {"x": 400, "y": 300}})).unwrap();
    connect(&mut s, 21, TWIN);
    let result = s.dispatch("run.reuse", &json!({"canvas": CANVAS})).unwrap();
    assert_eq!(result, json!({"reused": [TWIN]}));
    let (blur, twin) = (node(&s, BLUR), node(&s, TWIN));
    let outputs = |n: &nodecraft_graph::Node| n.history.iter().find(|g| Some(g.id) == n.active).unwrap().outputs.clone();
    assert_eq!(outputs(&twin), outputs(&blur), "same image, no work");
    assert_ne!(twin.active, blur.active, "its own result");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, TWIN).active.is_none(), "one undo step");
    s.dispatch("edit.redo", &json!({})).unwrap();
    // Turning caching off doesn't make anything stale.
    s.dispatch("node.cache_set", &json!({"canvas": CANVAS, "id": TWIN, "enabled": false})).unwrap();
    let canvas = s.document().canvases().get(&CANVAS).unwrap().clone();
    assert!(nodecraft_exec::staleness::stale(&canvas, s.kinds()).unwrap().is_empty());
    // And with it off, nothing is reused for that node.
    s.dispatch("history.clear", &json!({"canvas": CANVAS, "id": TWIN})).unwrap();
    assert_eq!(s.dispatch("run.reuse", &json!({"canvas": CANVAS, "nodes": [TWIN]})).unwrap(), json!({"reused": []}));
    s.dispatch("node.cache_set", &json!({"canvas": CANVAS, "id": TWIN, "enabled": null})).unwrap();
    assert_eq!(s.dispatch("run.reuse", &json!({"canvas": CANVAS, "nodes": [TWIN]})).unwrap(), json!({"reused": [TWIN]}));
}

#[test]
fn model_kinds_are_off_unless_asked_and_unknown_kinds_never_reuse() {
    let kinds = Registry::builtins().unwrap();
    let mut node = nodecraft_graph::Node::new(NodeId::from_bytes([1; 16]), "gen.unknown");
    assert!(!super::enabled(&node, &kinds));
    node.config.insert(nodecraft_exec::staleness::CACHE_CONFIG.into(), Value::Bool(true));
    assert!(super::enabled(&node, &kinds), "the switch wins");
    let action = nodecraft_graph::Node::new(NodeId::from_bytes([2; 16]), "image.blur");
    assert!(super::enabled(&action, &kinds), "actions reuse by default");
}
