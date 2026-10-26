use crate::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::folder;
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, Modality, NodeId, ProjectId, Value};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const FOLDER: NodeId = NodeId::from_bytes([9; 16]);

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Folder").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": C, "id": FOLDER, "kind": folder::ID, "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

fn fill(modality: &str, files: serde_json::Value) -> serde_json::Value {
    json!({"canvas": C, "id": FOLDER, "folder": "Notes", "modality": modality, "files": files})
}

#[test]
fn fill_replaces_files_in_one_undo_step_and_checks_them() {
    let mut s = session();
    let before = s.document().clone();
    let result = s
        .dispatch(
            "folder.fill",
            &fill("text", json!([{"name": "b.txt", "value": {"type": "text", "value": "two"}}, {"name": "a.txt", "value": {"type": "text", "value": "one"}}])),
        )
        .unwrap();
    assert_eq!(result, json!({"files": 2, "modality": "text"}));
    let card = s_node(&s);
    assert_eq!(folder::values(&card, folder::ALL_FILES).unwrap(), [Value::Text("one".into()), Value::Text("two".into())]);
    assert_eq!(card.outputs.first().map(|p| p.port_type.modality), Some(Modality::Text));
    // The same files again change nothing.
    let revision = s.revision();
    s.dispatch(
        "folder.fill",
        &fill("text", json!([{"name": "b.txt", "value": {"type": "text", "value": "two"}}, {"name": "a.txt", "value": {"type": "text", "value": "one"}}])),
    )
    .unwrap();
    assert_eq!(s.revision(), revision);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);

    for (params, kind) in [
        (fill("style", json!([])), ErrorKind::InvalidInput),
        (
            fill("text", json!([{"name": "a", "value": {"type": "text", "value": "x"}}, {"name": "a", "value": {"type": "text", "value": "y"}}])),
            ErrorKind::InvalidInput,
        ),
        (fill("image", json!([{"name": "a", "value": {"type": "text", "value": "not media"}}])), ErrorKind::InvalidInput),
        (json!({"canvas": C, "id": NodeId::from_bytes([3; 16]), "folder": "x", "modality": "text", "files": []}), ErrorKind::NotFound),
    ] {
        assert_eq!(s.dispatch("folder.fill", &params).unwrap_err().kind, kind, "{params}");
    }
    assert_eq!(s.document(), &before);
}

#[test]
fn a_wired_folder_keeps_its_kind_of_file() {
    let mut s = session();
    s.dispatch("folder.fill", &fill("text", json!([{"name": "a.txt", "value": {"type": "text", "value": "one"}}]))).unwrap();
    let text = NodeId::from_bytes([7; 16]);
    s.dispatch("node.create", &json!({"canvas": C, "id": text, "kind": "text.concat", "pos": {"x": 400, "y": 0}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([5; 16]), "from": {"node": FOLDER, "port": "names"}, "to": {"node": text, "port": "texts"}, "order": 0});
    s.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    let error = s.dispatch("folder.fill", &fill("audio", json!([]))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    s.dispatch("folder.fill", &fill("text", json!([{"name": "z.txt", "value": {"type": "text", "value": "last"}}]))).unwrap();
    assert_eq!(folder::values(&s_node(&s), folder::NAME).unwrap(), [Value::Text("z.txt".into())]);
}

fn s_node(s: &Session) -> nodecraft_graph::Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&FOLDER).unwrap().clone()
}
