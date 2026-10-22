use crate::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId};
use serde_json::json;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const NODE: NodeId = NodeId::from_bytes([2; 16]);

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Migrate").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": NODE, "kind": "source.text", "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

#[test]
fn a_current_card_has_nothing_to_migrate_and_adds_no_undo_step() {
    let mut s = session();
    let revision = s.revision();
    let before = s.document().clone();
    for params in [json!({"canvas": CANVAS, "node": NODE}), json!({"canvas": CANVAS, "node": NODE, "dry_run": true})] {
        assert!(s.enabled("node.migrate", &params).is_ok());
        assert_eq!(s.dispatch("node.migrate", &params).unwrap(), json!({"migrated": false, "warnings": []}));
    }
    assert_eq!((s.revision(), s.document()), (revision, &before));
}

#[test]
fn a_missing_card_or_kind_is_an_error() {
    let mut s = session();
    let missing = json!({"canvas": CANVAS, "node": NodeId::from_bytes([9; 16])});
    assert_eq!(s.dispatch("node.migrate", &missing).unwrap_err().kind, ErrorKind::NotFound);
    let canvas = json!({"canvas": CanvasId::from_bytes([9; 16]), "node": NODE});
    assert_eq!(s.dispatch("node.migrate", &canvas).unwrap_err().kind, ErrorKind::NotFound);
    assert!(s.dispatch("node.migrate", &json!({"canvas": CANVAS, "node": NODE, "extra": 1})).is_err());
}
