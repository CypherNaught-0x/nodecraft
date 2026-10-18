use nodecraft_engine::{
    Session,
    commands::{CommandSpec, Effect, Registry, Resources},
};
use nodecraft_format::asset_source::{AssetRange, AssetSource, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Position, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde_json::json;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Imports").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(C, "C") }, Change::AddNode { canvas: C, node: Node::new(N, "source.text") }])
        .apply(&mut p)
        .unwrap();
    p
}
fn range(bytes: &[u8]) -> AssetRange {
    AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64).unwrap()
}
fn movement() -> serde_json::Value {
    json!({"canvas":C,"id":N,"pos":{"x":1,"y":0}})
}
fn session() -> Session {
    Session::new(project()).unwrap()
}
#[test]
fn one_candidate_store_supplies_atomic_paste_and_one_undo_step() {
    let mut s = session();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let prepared = s.prepare_imports(&[range(b"one"), range(b"two"), range(b"one")], &cancel, &metrics).unwrap();
    assert_eq!(prepared.hashes()[0], prepared.hashes()[2]);
    assert_ne!(prepared.hashes()[0], prepared.hashes()[1]);
    assert!(s.assets().is_empty());
    let hash = prepared.hashes()[0];
    let mut node = Node::new(NodeId::from_bytes([9; 16]), "test.media");
    // Unrecognized signatures are still allowed, but a recognized PNG cannot claim JPEG.
    node.params.insert(
        "asset".into(),
        nodecraft_types::Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }),
    );
    let params = json!({"canvas":C,"nodes":[node],"edges":[]});
    s.dispatch_with_imports("canvas.paste", &params, prepared).unwrap();
    assert_eq!(s.assets().len(), 2);
    assert_eq!(s.revision(), 1);
    assert!(s.can_undo());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document().canvases()[&C].nodes().len(), 1);
    assert!(!s.can_undo());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document().canvases()[&C].nodes().len(), 2);
}
#[test]
fn rejected_paste_and_non_edit_effect_do_not_publish_imports_or_invoke_services() {
    let mut s = session();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let prepared = s.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    assert!(s.dispatch_with_imports("canvas.paste", &json!({"canvas":C,"nodes":[Node::new(N,"source.text")]}), prepared).is_err());
    assert!(s.assets().is_empty());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert!(!s.can_undo());
    let called = Rc::new(Cell::new(0));
    let seen = called.clone();
    s.register_host_service("test.service", move |_| {
        seen.set(seen.get() + 1);
        Ok(json!({}))
    })
    .unwrap();
    let mut registry = Registry::builtins().unwrap();
    registry
        .register(CommandSpec {
            id: "test.service",
            label: "Test",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({}),
            params_doc: "{}",
            enabled: |_, _| Ok(()),
            run: |_, _| Ok(Effect::Service { capability: "test.service", request: json!({}) }),
        })
        .unwrap();
    // Services belong to the new session and are deliberately not invoked by this entry.
    let mut s = Session::with_registry(project(), registry).unwrap();
    let seen = called.clone();
    s.register_host_service("test.service", move |_| {
        seen.set(seen.get() + 1);
        Ok(json!({}))
    })
    .unwrap();
    let prepared = s.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    assert_eq!(s.dispatch_with_imports("test.service", &json!({}), prepared).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(called.get(), 0);
    assert!(s.assets().is_empty());
    assert_eq!(s.revision(), 0);
}
fn instrumented() -> (Session, Rc<Cell<usize>>) {
    let called = Rc::new(Cell::new(0));
    let mut registry = Registry::builtins().unwrap();
    registry
        .register(CommandSpec {
            id: "test.edit",
            label: "Test",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({}),
            params_doc: "{}",
            enabled: |c, _| {
                let n = c.resources.require::<Rc<Cell<usize>>>()?;
                n.set(n.get() + 1);
                Ok(())
            },
            run: |_, _| {
                Ok(Effect::Edit {
                    changes: Changeset::new(vec![Change::MoveNode { canvas: C, id: N, pos: Position { x: 2.0, y: 0.0 } }]),
                    result: json!({}),
                    gesture: None,
                })
            },
        })
        .unwrap();
    let mut resources = Resources::default();
    resources.insert(called.clone());
    (Session::with_registry(project(), registry).unwrap().with_resources(resources), called)
}
#[test]
fn foreign_and_stale_batch_stamp_is_checked_before_handler() {
    let (mut a, called_a) = instrumented();
    let (mut b, called_b) = instrumented();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let prepared = a.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    assert_eq!(b.dispatch_with_imports("test.edit", &json!({}), prepared).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(called_b.get(), 0);
    assert!(b.assets().is_empty());
    let prepared = a.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    a.dispatch("node.move", &movement()).unwrap();
    assert_eq!(a.dispatch_with_imports("test.edit", &json!({}), prepared).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(called_a.get(), 0);
    let prepared = a.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    a.import_asset(b"other").unwrap();
    assert!(a.dispatch_with_imports("test.edit", &json!({}), prepared).is_err());
    assert_eq!(called_a.get(), 0);
    assert_eq!(a.assets().len(), 1);
}
struct Counted {
    reads: Arc<AtomicUsize>,
    length: u64,
}
impl AssetSource for Counted {
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, Error> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Err(Error::new(ErrorKind::Conflict, "unexpected source read"))
    }
}
#[test]
fn all_descriptors_are_preflighted_before_any_source_read() {
    let s = session();
    let reads = Arc::new(AtomicUsize::new(0));
    let a = AssetRange::new(Arc::new(Counted { reads: reads.clone(), length: 1 }), 0, 1).unwrap();
    let length = nodecraft_format::MAX_ASSET_BYTES as u64 + 1;
    let b = AssetRange::new(Arc::new(Counted { reads: reads.clone(), length }), 0, length).unwrap();
    assert!(s.prepare_imports(&[a, b], &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert!(s.assets().is_empty());
}
struct ChangesBetweenPasses {
    reads: AtomicUsize,
}
impl AssetSource for ChangesBetweenPasses {
    fn byte_len(&self) -> u64 {
        3
    }
    fn read_at(&self, _: u64, out: &mut [u8]) -> Result<usize, Error> {
        let old = self.reads.fetch_add(1, Ordering::Relaxed) == 0;
        out[..3].copy_from_slice(if old { b"one" } else { b"two" });
        Ok(3)
    }
}
#[test]
fn mutable_second_pass_and_cancellation_leave_no_live_import() {
    let mut s = session();
    let source = AssetRange::new(Arc::new(ChangesBetweenPasses { reads: AtomicUsize::new(0) }), 0, 3).unwrap();
    assert!(s.import_asset_from(&source, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert!(s.assets().is_empty());
    assert_eq!(s.revision(), 0);
    let cancel = TransferCancel::default();
    cancel.cancel();
    let metrics = StreamMetrics::default();
    assert!(s.prepare_imports(&[range(b"cancel")], &cancel, &metrics).is_err());
    assert_eq!(metrics.stats().source_bytes, 0);
    assert!(s.assets().is_empty());
}
#[test]
fn resident_count_and_byte_saturation_accept_only_existing_candidate_duplicates() {
    let mut s = session();
    for n in 0..nodecraft_format::MAX_ASSETS {
        s.import_asset(format!("asset {n}").as_bytes()).unwrap();
    }
    assert_eq!(s.assets().len(), 1024);
    let hash = s.import_asset(b"asset 0").unwrap();
    assert!(s.assets().contains(&hash));
    assert!(s.import_asset(b"new identity").is_err());
    assert_eq!(s.assets().len(), 1024);
    let mut s = session();
    for fill in [1, 2, 3] {
        s.import_asset(&vec![fill; nodecraft_format::MAX_ASSET_BYTES]).unwrap();
    }
    assert_eq!(s.assets().byte_len(), nodecraft_format::MAX_TOTAL_ASSET_BYTES as u64);
    let same = vec![1; nodecraft_format::MAX_ASSET_BYTES];
    let prepared = s.prepare_imports(&[range(&same), range(&same)], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(prepared.hashes()[0], prepared.hashes()[1]);
    s.dispatch_with_imports("node.move", &movement(), prepared).unwrap();
    assert_eq!(s.assets().len(), 3);
    assert!(s.import_asset(b"new").is_err());
}
#[test]
fn logical_collection_retains_undo_redo_roots_and_only_changed_map_stales_batches() {
    let mut s = session();
    let hash = s.import_asset(b"referenced").unwrap();
    let orphan = s.import_asset(b"orphan").unwrap();
    let mut node = Node::new(NodeId::from_bytes([9; 16]), "test.media");
    node.params.insert(
        "asset".into(),
        nodecraft_types::Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }),
    );
    s.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let prepared = s.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    let status = s.file_status();
    let status_revision = s.status_revision();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    assert!(s.collect_assets().unwrap());
    assert!(s.assets().contains(&hash), "redo-only media remains rooted");
    assert!(!s.assets().contains(&orphan));
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(s.file_status(), status);
    assert_eq!(s.status_revision(), status_revision);
    assert!(s.can_redo());
    assert_eq!(s.dispatch_with_imports("node.move", &movement(), prepared).unwrap_err().kind, ErrorKind::Conflict);
    s.dispatch("edit.redo", &json!({})).unwrap();
    s.dispatch("node.remove", &json!({"canvas":C,"id":NodeId::from_bytes([9;16])})).unwrap();
    let prepared = s.prepare_imports(&[range(b"candidate")], &cancel, &metrics).unwrap();
    assert!(!s.collect_assets().unwrap(), "undo-only media remains rooted");
    s.dispatch_with_imports("node.move", &movement(), prepared).unwrap();
    assert!(s.assets().contains(&hash));
}
