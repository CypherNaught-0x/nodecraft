use crate::Session;
use nodecraft_exec::variations::VARIATIONS_CONFIG;
use nodecraft_types::{CanvasId, NodeId, Unsigned, Value as Wire};
use serde_json::json;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMAGE: NodeId = NodeId::from_bytes([3; 16]);
const TEXT: NodeId = NodeId::from_bytes([4; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": IMAGE, "kind": "generation.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": TEXT, "kind": "source.text", "pos": {"x": 400, "y": 0}})).unwrap();
    s
}

fn stored(s: &Session) -> Option<Wire> {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&IMAGE).unwrap().config.get(VARIATIONS_CONFIG).cloned()
}

#[test]
fn the_count_is_set_undone_and_reset_as_one_edit_each() {
    let mut s = session();
    assert_eq!(s.dispatch("node.variations_set", &json!({"canvas": CANVAS, "id": IMAGE, "count": 4})).unwrap(), json!({"count": 4}));
    assert_eq!(stored(&s), Some(Wire::Unsigned(Unsigned(4))));
    let canvas = s.document().canvases().get(&CANVAS).unwrap().clone();
    assert!(nodecraft_exec::staleness::stale(&canvas, s.kinds()).unwrap().is_empty(), "the count never makes a result stale");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(stored(&s), None);
    s.dispatch("edit.redo", &json!({})).unwrap();
    // One is the default, so it is stored as nothing, like null.
    let revision = s.revision();
    assert_eq!(s.dispatch("node.variations_set", &json!({"canvas": CANVAS, "id": IMAGE, "count": 1})).unwrap(), json!({"count": 1}));
    assert_eq!(stored(&s), None);
    assert_ne!(s.revision(), revision);
    let revision = s.revision();
    assert_eq!(s.dispatch("node.variations_set", &json!({"canvas": CANVAS, "id": IMAGE, "count": null})).unwrap(), json!({"count": 1}));
    assert_eq!(s.revision(), revision, "no edit when nothing changes");
}

#[test]
fn bad_requests_are_errors_that_change_nothing() {
    let mut s = session();
    let revision = s.revision();
    for params in [
        json!({"canvas": CANVAS, "id": IMAGE, "count": 0}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": 21}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": u64::MAX}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": -1}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": "3"}),
        json!({"canvas": CANVAS, "id": TEXT, "count": 3}),
        json!({"canvas": CANVAS, "id": NodeId::from_bytes([9; 16]), "count": 3}),
        json!({"canvas": CanvasId::from_bytes([9; 16]), "id": IMAGE, "count": 3}),
    ] {
        assert!(s.dispatch("node.variations_set", &params).is_err(), "{params}");
    }
    assert_eq!(s.revision(), revision);
}
