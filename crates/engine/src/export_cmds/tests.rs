use crate::Session;
use nodecraft_graph::{Canvas, Change, Generation, Node, Port, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, GenerationId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
const IMAGE: GenerationId = GenerationId::from_bytes([3; 16]);
const TEXT: GenerationId = GenerationId::from_bytes([4; 16]);
const BYTES: &[u8] = b"\x89PNG not really, but bytes all the same";

/// A node with an image result (its bytes in the session's store) and a text result.
fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Export").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(N, "test.gen");
    node.outputs = vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }];
    let asset = AssetRef { hash: Blake3::from_bytes(*blake3::hash(BYTES).as_bytes()), mime: "image/png".parse().unwrap(), meta: Default::default() };
    node.history.push(Generation::new(IMAGE, vec![Value::Media(asset)]));
    node.history.push(Generation::new(TEXT, vec![Value::Text("words".into())]));
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.import_asset(BYTES).unwrap();
    session
}

fn export(path: &std::path::Path, extra: Json) -> Json {
    let mut params = json!({"canvas": C, "id": N, "generation": IMAGE, "path": path});
    if let (Json::Object(params), Json::Object(extra)) = (&mut params, extra) {
        params.extend(extra);
    }
    params
}

#[test]
fn a_result_is_written_once_and_replaced_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.png");
    let mut s = session();
    let revision = s.revision();
    assert_eq!(s.dispatch("history.export", &export(&path, json!({}))).unwrap(), json!({"bytes": BYTES.len()}));
    assert_eq!(std::fs::read(&path).unwrap(), BYTES);
    assert_eq!(s.revision(), revision, "exporting doesn't edit the document");
    assert!(!s.can_undo());

    std::fs::write(&path, b"mine").unwrap();
    let kept = s.dispatch("history.export", &export(&path, json!({}))).unwrap_err();
    assert_eq!(kept.kind, ErrorKind::Conflict);
    assert!(kept.message.contains("overwrite") && !kept.message.contains("take.png"), "{}", kept.message);
    assert_eq!(std::fs::read(&path).unwrap(), b"mine");
    s.dispatch("history.export", &export(&path, json!({"overwrite": true}))).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), BYTES);
}

#[test]
fn missing_results_text_and_bad_paths_are_refused_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.png");
    let mut s = session();
    let missing = json!({"canvas": C, "id": N, "generation": GenerationId::from_bytes([9; 16]), "path": path});
    assert_eq!(s.enabled("history.export", &missing).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(s.dispatch("history.export", &missing).unwrap_err().kind, ErrorKind::NotFound);
    let text = s.dispatch("history.export", &export(&path, json!({"generation": TEXT}))).unwrap_err();
    assert_eq!(text.kind, ErrorKind::Unsupported);
    assert_eq!(s.dispatch("history.export", &export(&path, json!({"item": 0}))).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("history.export", &export(std::path::Path::new("relative.png"), json!({}))).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("history.export", &export(dir.path(), json!({}))).unwrap_err().kind, ErrorKind::InvalidInput);
    let nowhere = dir.path().join("no-such-folder").join("out.png");
    assert_eq!(s.dispatch("history.export", &export(&nowhere, json!({}))).unwrap_err().kind, ErrorKind::NotFound);
    assert!(s.dispatch("history.export", &export(&path, json!({"surprise": 1}))).is_err());
    assert!(!path.exists());
}

#[test]
fn a_result_whose_bytes_are_gone_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.png");
    // An asset the store never held is refused, and no file appears.
    let mut fresh = {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Export").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(N, "test.gen");
        node.outputs = vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }];
        let asset = AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
        node.history.push(Generation::new(IMAGE, vec![Value::Media(asset)]));
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        Session::new(project).unwrap()
    };
    assert!(fresh.dispatch("history.export", &export(&path, json!({}))).is_err());
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn a_folder_it_cannot_write_to_is_reported_and_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut s = session();
    let error = s.dispatch("history.export", &export(&locked.join("out.png"), json!({}))).unwrap_err();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(error.message.contains("can't write there"), "{error:?}");
    assert_eq!(std::fs::read_dir(&locked).unwrap().count(), 0);
}

#[test]
fn one_item_of_a_list_output_on_a_named_port_exports() {
    const OTHER: &[u8] = b"\x89PNG another image, also not really";
    let dir = tempfile::tempdir().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Export").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(N, "test.gen");
    let port = |name: &str, many| Port { name: name.into(), port_type: PortType { modality: Modality::Image, many } };
    node.outputs = vec![port("cover", false), port("takes", true)];
    let media = |bytes: &[u8]| {
        Value::Media(AssetRef { hash: Blake3::from_bytes(*blake3::hash(bytes).as_bytes()), mime: "image/png".parse().unwrap(), meta: Default::default() })
    };
    node.history.push(Generation::new(IMAGE, vec![media(BYTES), Value::List(vec![media(BYTES), media(OTHER)])]));
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.import_asset(BYTES).unwrap();
    s.import_asset(OTHER).unwrap();

    let path = dir.path().join("second.png");
    s.dispatch("history.export", &export(&path, json!({"port": "takes", "item": 1}))).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), OTHER);
    let list = s.dispatch("history.export", &export(&dir.path().join("all.png"), json!({"port": "takes"}))).unwrap_err();
    assert_eq!(list.kind, ErrorKind::InvalidInput, "a list needs an item");
    let past = s.dispatch("history.export", &export(&dir.path().join("third.png"), json!({"port": "takes", "item": 2}))).unwrap_err();
    assert_eq!(past.kind, ErrorKind::NotFound);
    let unknown = s.dispatch("history.export", &export(&dir.path().join("x.png"), json!({"port": "nope"}))).unwrap_err();
    assert_eq!(unknown.kind, ErrorKind::NotFound);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "only the one export was written");
}
