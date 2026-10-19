use super::*;
use nodecraft_types::{AssetRef, MediaMeta, Value};
use serde_json::json;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([10; 16]);
const BLUR: NodeId = NodeId::from_bytes([11; 16]);

fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(width, height, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let hash = s.import_asset(&png(16, 12)).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() };
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": SOURCE, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": SOURCE, "name": "asset", "value": Value::Media(asset)})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": BLUR, "kind": "image.blur", "pos": {"x": 400, "y": 0}})).unwrap();
    s
}

fn connect(s: &mut Session) {
    let edge = nodecraft_types::EdgeId::from_bytes([12; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": SOURCE, "port": "out"}, "to": {"node": BLUR, "port": "image"}, "order": 0}}),
    )
    .unwrap();
}

/// Plans, runs and records whatever is pending; whether something was recorded.
fn settle(s: &mut Session) -> bool {
    let Some(job) = pending(s, CANVAS, &BTreeSet::new()).unwrap() else { return false };
    let kinds = Registry::builtins().unwrap();
    let outcome = compute(job, &kinds).unwrap();
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    apply(s, outcome).unwrap()
}

fn result(s: &Session) -> Option<nodecraft_types::Blake3> {
    let node = s.document().canvases().get(&CANVAS)?.nodes().get(&BLUR)?;
    let active = node.active?;
    match node.history.iter().find(|g| g.id == active)?.outputs.first()? {
        Value::Media(asset) => Some(asset.hash),
        _ => None,
    }
}

fn sigma(s: &Session) -> Value {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&BLUR).unwrap().params.get("sigma").cloned().unwrap_or(Value::Bool(false))
}

#[test]
fn an_unconnected_action_waits_and_a_connected_one_settles_into_the_edit() {
    let mut s = session();
    assert!(!settle(&mut s), "nothing connected: nothing to run");
    connect(&mut s);
    assert!(settle(&mut s));
    let first = result(&s).expect("the blur has a result");
    assert!(s.assets().get(&first).is_some(), "its image is in the project");
    assert!(pending(&s, CANVAS, &BTreeSet::new()).unwrap().is_none(), "settled: nothing stale");
    // One undo removes the connection and its result together.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(result(&s).is_none());
    assert!(s.document().canvases().get(&CANVAS).unwrap().edges().is_empty());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(result(&s), Some(first));
}

#[test]
fn a_parameter_change_recomputes_and_undo_restores_value_and_result_together() {
    let mut s = session();
    connect(&mut s);
    settle(&mut s);
    let first = result(&s).unwrap();
    let before = sigma(&s);
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(6.0).unwrap())}))
        .unwrap();
    assert!(settle(&mut s));
    let second = result(&s).unwrap();
    assert_ne!(first, second);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!((sigma(&s), result(&s)), (before, Some(first)), "old value, old result");
    assert!(pending(&s, CANVAS, &BTreeSet::new()).unwrap().is_none(), "and nothing to recompute");
    // Results wait while there is something to redo; redo brings the newer result back.
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(result(&s), Some(second));
}

#[test]
fn results_for_an_older_revision_are_dropped() {
    let mut s = session();
    connect(&mut s);
    let job = pending(&s, CANVAS, &BTreeSet::new()).unwrap().unwrap();
    let outcome = compute(job, &Registry::builtins().unwrap()).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(3.0).unwrap())}))
        .unwrap();
    let revision = s.revision();
    assert!(!apply(&mut s, outcome).unwrap());
    assert_eq!(s.revision(), revision, "nothing recorded");
    assert!(settle(&mut s), "the next round records the current state");
}

#[test]
fn a_text_chain_settles_in_one_round() {
    let mut s = session();
    let (text, split, join) = (NodeId::from_bytes([20; 16]), NodeId::from_bytes([21; 16]), NodeId::from_bytes([22; 16]));
    for (id, kind) in [(text, "source.text"), (split, "text.split"), (join, "text.concat")] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 300}})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": text, "name": "text", "value": Value::Text("a,b,c".into())})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": join, "name": "separator", "value": Value::Text("+".into())})).unwrap();
    for (n, from, to, port) in [(30u8, text, split, "text"), (31, split, join, "texts")] {
        let edge = nodecraft_types::EdgeId::from_bytes([n; 16]);
        s.dispatch(
            "edge.connect",
            &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": from, "port": "out"}, "to": {"node": to, "port": port}, "order": 0}}),
        )
        .unwrap();
    }
    assert!(settle(&mut s));
    let joined = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&join).unwrap();
    let active = joined.history.iter().find(|g| Some(g.id) == joined.active).unwrap();
    assert_eq!(active.outputs, vec![Value::Text("a+b+c".into())]);
}
