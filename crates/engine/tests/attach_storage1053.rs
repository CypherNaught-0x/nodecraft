//! A session started without storage takes its storage over later (#1053): the desktop starts
//! its window while an earlier session's journal is still being kept aside, and only then lets
//! the session checkpoint over it. What the session holds by then (edits, imported media) is
//! in that first checkpoint, and every later edit is journalled as usual.
use nodecraft_engine::Session;
use nodecraft_format::{
    AssetStore, MemoryStorage, NativeStorage, Snapshot,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, CanvasId, ErrorKind, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([7; 16]);

fn project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([7; 16]), "Starter").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    project
}

#[test]
fn storage_attached_later_checkpoints_what_the_session_already_holds_and_journals_what_follows() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("session.journal");
    let mut session = Session::new(project()).unwrap();
    assert!(!session.file_status().available, "no storage yet");
    // Made before storage arrives (by a client, say): media and an edit that uses it.
    let hash = session.import_asset(&vec![5u8; 200 * 1024]).unwrap();
    let mut node = Node::new(NodeId::from_bytes([1; 16]), "source.text");
    node.params.insert("content".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    session.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
    let revision = session.status_revision();

    session.attach_storage(Box::new(NativeStorage::new(journal.clone()).unwrap())).unwrap();
    let status = session.file_status();
    assert!(status.available && status.dirty && status.location.is_none(), "{status:?}");
    assert!(session.status_revision() > revision, "hosts see the status change");
    assert!(session.can_undo(), "the edit made before stays undoable");
    assert_eq!(session.attach_storage(Box::new(MemoryStorage::default())).unwrap_err().kind, ErrorKind::Conflict, "storage is attached once");
    // A later edit lands in the journal, after the checkpoint holding the first.
    session.dispatch("node.move", &json!({"canvas": C, "id": NodeId::from_bytes([1; 16]), "pos": {"x": 40, "y": 0}})).unwrap();
    let expected = session.document().clone();
    drop(session);
    let replayed = NativeStorage::new(journal).unwrap().recover().unwrap();
    assert_eq!(replayed.snapshot.project, expected);
    assert!(replayed.applied >= 1, "the later edit was journalled: {}", replayed.applied);
    let media = replayed.snapshot.assets.verified_content(&hash, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(media.byte_len(), 200 * 1024, "the media imported before storage is in the checkpoint");
}

#[test]
fn a_session_built_over_storage_refuses_another() {
    let snapshot = Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(MemoryStorage::default())).unwrap();
    let error = session.attach_storage(Box::new(MemoryStorage::default())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(session.file_status().available);
}
