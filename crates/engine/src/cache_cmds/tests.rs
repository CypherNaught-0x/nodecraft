use crate::Session;
use serde_json::json;

#[test]
fn bad_requests_are_errors_and_nothing_to_reuse_changes_nothing() {
    let canvas = nodecraft_types::CanvasId::from_bytes([1; 16]);
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let revision = s.revision();
    assert_eq!(s.dispatch("run.reuse", &json!({"canvas": canvas})).unwrap(), json!({"reused": []}));
    assert_eq!(s.revision(), revision, "no edit when nothing is reused");
    assert!(s.dispatch("run.reuse", &json!({"canvas": nodecraft_types::CanvasId::from_bytes([9; 16])})).is_err());
    assert!(s.dispatch("node.cache_set", &json!({"canvas": canvas, "id": nodecraft_types::NodeId::from_bytes([3; 16]), "enabled": true})).is_err());
    assert!(s.dispatch("node.cache_set", &json!({"canvas": canvas})).is_err());
}
