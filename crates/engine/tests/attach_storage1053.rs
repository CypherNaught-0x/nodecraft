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

#[test]
fn a_session_held_for_storage_refuses_every_change_and_says_why() {
    let mut session = Session::new(project()).unwrap();
    let revision = session.status_revision();
    session.hold_for_storage(true);
    assert!(session.status_revision() > revision, "hosts see the status change");
    let node = Node::new(NodeId::from_bytes([2; 16]), "source.text");
    for refused in
        [session.dispatch("node.add", &json!({"canvas": C, "node": node})), session.dispatch_without_waiting("node.add", &json!({"canvas": C, "node": node}))]
    {
        let error = refused.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy);
        assert!(error.message.contains("keeping the last session's unsaved work safe"), "{}", error.message);
    }
    assert!(session.file_status().unavailable_reason.is_some_and(|reason| reason.contains("keeping the last session's unsaved work safe")));
    assert!(session.dispatch("file.status", &json!({})).is_ok(), "reads still answer");
    assert!(session.import_asset(b"held").is_err(), "nor are media imported");
    assert_eq!(session.document(), &project());
    // Attaching storage lifts it.
    session.attach_storage(Box::new(MemoryStorage::default())).unwrap();
    session.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
    // A host may also lift it without storage (the keep-aside was cancelled).
    let mut other = Session::new(project()).unwrap();
    other.hold_for_storage(true);
    other.hold_for_storage(false);
    other.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
}

/// #367: the first checkpoint is written off the UI thread. `prepare_storage` does the disk work
/// on another thread; `attach_prepared` only installs it, and only over the very document it
/// was prepared from.
#[test]
fn storage_prepared_on_another_thread_is_attached_without_writing_and_only_over_the_same_document() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("session.journal");
    let snapshot = Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 };
    let storage = NativeStorage::new(journal.clone()).unwrap();
    let prepared = std::thread::spawn(move || Session::prepare_storage(Box::new(storage), snapshot)).join().unwrap().unwrap();
    let written = std::fs::read(&journal).unwrap();
    assert!(!written.is_empty(), "the checkpoint was written on the other thread");
    let mut session = Session::new(project()).unwrap();
    session.hold_for_storage(true);
    session.attach_prepared(prepared).unwrap();
    assert_eq!(std::fs::read(&journal).unwrap(), written, "attaching wrote nothing");
    assert!(session.file_status().available);
    session.dispatch("node.add", &json!({"canvas": C, "node": Node::new(NodeId::from_bytes([3; 16]), "source.text")})).unwrap();
    let expected = session.document().clone();
    drop(session);
    assert_eq!(NativeStorage::new(journal).unwrap().recover().unwrap().snapshot.project, expected, "later edits are journalled after it");

    // A session that changed meanwhile isn't given a checkpoint without its change.
    let other = dir.path().join("other.journal");
    let snapshot = Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 };
    let prepared = Session::prepare_storage(Box::new(NativeStorage::new(other.clone()).unwrap()), snapshot).unwrap();
    let mut changed = Session::new(project()).unwrap();
    changed.dispatch("node.add", &json!({"canvas": C, "node": Node::new(NodeId::from_bytes([4; 16]), "source.text")})).unwrap();
    assert_eq!(changed.attach_prepared(prepared).unwrap_err().kind, ErrorKind::Conflict);
    assert!(!changed.file_status().available);
    assert!(NativeStorage::new(other).is_ok(), "the refused storage let its lock go");
}
