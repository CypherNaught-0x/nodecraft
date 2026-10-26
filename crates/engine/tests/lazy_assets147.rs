#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::Session;
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, CheckpointPlan, FramePlan, LoadedProject, NativeStorage, PreparedSnapshot, ProjectAccess, ProjectStorage, Snapshot,
    StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId, Value};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
struct Host {
    native: NativeStorage,
    checkpoints: usize,
    appends: usize,
    saves: usize,
    opens: usize,
    fail_checkpoint: bool,
    fail_append: bool,
    saved_readonly_once: bool,
    metrics: StreamMetrics,
}
#[derive(Clone)]
struct TypedOnly(Rc<RefCell<Host>>);
fn legacy() -> Error {
    Error::new(ErrorKind::Internal, "legacy whole-byte route must not be used by native Session")
}
impl ProjectStorage for TypedOnly {
    fn capability(&self) -> StorageCapability {
        StorageCapability::StreamingV2
    }
    fn asset_context(&self) -> Option<AssetContext> {
        self.0.borrow().native.asset_context()
    }
    fn prepare_open_snapshot(&mut self, l: &StorageLocation, c: &TransferCancel, m: &StreamMetrics) -> Result<PreparedSnapshot, Error> {
        let mut host = self.0.borrow_mut();
        host.opens += 1;
        host.metrics = m.clone();
        host.native.prepare_open_snapshot(l, c, m)
    }
    fn save_plan(&mut self, l: &StorageLocation, p: &ArchivePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<ProjectAccess, Error> {
        let mut host = self.0.borrow_mut();
        host.saves += 1;
        host.metrics = m.clone();
        let access = host.native.save_plan(l, p, c, m)?;
        if host.saved_readonly_once {
            host.saved_readonly_once = false;
            Ok(ProjectAccess::ReadOnly)
        } else {
            Ok(access)
        }
    }
    fn checkpoint_plan(&mut self, p: &CheckpointPlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        let mut host = self.0.borrow_mut();
        host.checkpoints += 1;
        host.metrics = m.clone();
        if host.fail_checkpoint {
            host.fail_checkpoint = false;
            return Err(Error::new(ErrorKind::Conflict, "injected checkpoint failure"));
        }
        host.native.checkpoint_plan(p, c, m)
    }
    fn append_plan(&mut self, p: &FramePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        let mut host = self.0.borrow_mut();
        host.appends += 1;
        host.metrics = m.clone();
        if host.fail_append {
            return Err(Error::new(ErrorKind::Conflict, "injected ambiguous append failure"));
        }
        host.native.append_plan(p, c, m)
    }
    fn commit_open(&mut self) {
        self.0.borrow_mut().native.commit_open();
    }
    fn cancel_open(&mut self) {
        self.0.borrow_mut().native.cancel_open();
    }
    fn load(&mut self, _: &StorageLocation) -> Result<Vec<u8>, Error> {
        Err(legacy())
    }
    fn prepare_open(&mut self, _: &StorageLocation) -> Result<LoadedProject, Error> {
        Err(legacy())
    }
    fn save(&mut self, _: &StorageLocation, _: &[u8]) -> Result<(), Error> {
        Err(legacy())
    }
    fn checkpoint(&mut self, _: &[u8]) -> Result<(), Error> {
        Err(legacy())
    }
    fn append(&mut self, _: &[u8]) -> Result<(), Error> {
        Err(legacy())
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().native.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().native.discard()
    }
}
fn host(dir: &std::path::Path) -> TypedOnly {
    TypedOnly(Rc::new(RefCell::new(Host {
        native: NativeStorage::new(dir.join("recovery")).unwrap(),
        checkpoints: 0,
        appends: 0,
        saves: 0,
        opens: 0,
        fail_checkpoint: false,
        fail_append: false,
        saved_readonly_once: false,
        metrics: StreamMetrics::default(),
    })))
}
fn snapshot(assets: AssetStore) -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Lazy Session").unwrap();
    let mut node = Node::new(N, "source.text");
    node.params.insert("text".into(), Value::Text("text".into()));
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(C, "C") }, Change::AddNode { canvas: C, node }]).apply(&mut project).unwrap();
    Snapshot { project, assets, epoch: 0 }
}
fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":C,"id":N,"pos":{"x":x,"y":0}})
}
#[test]
fn session_uses_typed_native_routes_before_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot(AssetStore::default()), Box::new(host.clone())).unwrap();
    session.dispatch("node.move", &movement(1)).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    session.dispatch("edit.redo", &json!({})).unwrap();
    let destination = dir.path().join("saved.ncraft");
    session.dispatch("file.save_as", &json!({"location":destination})).unwrap();
    session.dispatch("node.move", &movement(2)).unwrap();
    session.dispatch("file.open", &json!({"location":destination,"discard_dirty":true})).unwrap();
    assert_eq!(session.document().canvases()[&C].nodes()[&N].pos.x, 1.0);
    assert!(!session.can_undo());
    assert!(!session.file_status().dirty);
    let host = host.0.borrow();
    assert_eq!(host.appends, 4);
    assert_eq!(host.saves, 1);
    assert_eq!(host.opens, 1);
    assert_eq!(host.checkpoints, 3);
    assert_eq!(host.native.recover().unwrap().snapshot.project, *session.document());
}

#[test]
fn published_save_with_failed_checkpoint_adopts_savepoint_and_requires_repair() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot(AssetStore::default()), Box::new(host.clone())).unwrap();
    session.dispatch("node.move", &movement(1)).unwrap();
    let destination = dir.path().join("published.ncraft");
    host.0.borrow_mut().fail_checkpoint = true;
    let result = session.dispatch("file.save_as", &json!({"location":destination})).unwrap();
    assert_eq!(result, json!({"saved":true,"recovery_ready":false,"read_only":false}));
    assert!(destination.is_file());
    assert!(!session.file_status().dirty);
    assert!(session.file_status().recovery_required);
    assert_eq!(session.file_status().location, Some(StorageLocation::Path(destination)));
    let revision = session.revision();
    assert!(session.enabled("node.move", &movement(2)).is_err());
    assert!(session.dispatch("node.move", &movement(2)).is_err());
    assert_eq!(session.revision(), revision);
    assert_eq!(session.dispatch("file.save", &json!({})).unwrap()["recovery_ready"], true);
    session.dispatch("node.move", &movement(2)).unwrap();
    assert_eq!(host.0.borrow().native.recover().unwrap().snapshot.project, *session.document());
}

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 6, 0, 0, 0, 185, 234, 222, 129, 0, 0, 0, 17, 73, 68, 65, 84, 120,
    156, 99, 248, 207, 192, 240, 31, 132, 25, 48, 24, 0, 161, 121, 11, 245, 77, 196, 154, 7, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
struct Padded {
    bytes: u64,
    fill: u8,
}
impl nodecraft_format::asset_source::AssetSource for Padded {
    fn byte_len(&self) -> u64 {
        self.bytes
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        let n = out.len().min(usize::try_from(self.bytes.saturating_sub(at)).unwrap_or(usize::MAX));
        for (offset, byte) in out[..n].iter_mut().enumerate() {
            *byte = PNG.get((at as usize).saturating_add(offset)).copied().unwrap_or(self.fill);
        }
        Ok(n)
    }
}
fn asset(hash: nodecraft_types::Blake3) -> nodecraft_types::AssetRef {
    nodecraft_types::AssetRef {
        hash,
        mime: "image/png".parse().unwrap(),
        meta: nodecraft_types::MediaMeta { width: Some(2), height: Some(3), ..Default::default() },
    }
}
fn generation(n: u8) -> nodecraft_types::GenerationId {
    nodecraft_types::GenerationId::from_bytes([n; 16])
}
fn history_snapshot(assets: AssetStore, hashes: &[nodecraft_types::Blake3]) -> Snapshot {
    let mut snapshot = snapshot(assets);
    let mut node = Node::new(N, "test.gen");
    node.outputs
        .push(nodecraft_graph::Port { name: "out".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: false } });
    for (i, hash) in hashes.iter().enumerate() {
        node.history.push(nodecraft_graph::Generation::new(generation(i as u8 + 1), vec![Value::Media(asset(*hash))]));
    }
    node.active = Some(generation(hashes.len() as u8));
    Change::SetNode { canvas: C, node }.apply(&mut snapshot.project).unwrap();
    snapshot
}
#[test]
fn real_large_indexed_history_is_lazy_until_extract_and_survives_smaller_save_and_undo() {
    use nodecraft_format::asset_source::AssetRange;
    use std::{io::Read, sync::Arc};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("large.ncraft");
    let producer = NativeStorage::new(dir.path().join("producer-recovery")).unwrap();
    let mut assets = AssetStore::with_context(producer.asset_context().unwrap());
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let bytes = 20 * 1024 * 1024 + 1;
    let mut hashes = Vec::new();
    for fill in [1, 2, 3] {
        hashes.push(assets.import_stream(&AssetRange::new(Arc::new(Padded { bytes, fill }), 0, bytes).unwrap(), &cancel, &metrics).unwrap());
    }
    let snapshot = history_snapshot(assets, &hashes);
    let archive = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &cancel, &metrics).unwrap();
    let mut producer = producer;
    producer.save_plan(&StorageLocation::Path(input.clone()), &archive, &cancel, &metrics).unwrap();
    assert!(std::fs::metadata(&input).unwrap().len() > 48 * 1024 * 1024);
    drop((archive, snapshot, producer));
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    session.dispatch("file.open", &json!({"location":input,"discard_dirty":true})).unwrap();
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, 0);
    let baseline = host.0.borrow().metrics.stats().source_bytes;
    session.dispatch("history.previous", &json!({"canvas":C,"id":N})).unwrap();
    session.dispatch("history.next", &json!({"canvas":C,"id":N})).unwrap();
    session.dispatch("history.select", &json!({"canvas":C,"id":N,"generation":generation(1)})).unwrap();
    let extracted = NodeId::from_bytes([7; 16]);
    let params = json!({"canvas":C,"id":N,"generation":generation(1),"node":extracted,"pos":{"x":10,"y":10}});
    session.enabled("history.extract", &params).unwrap();
    assert_eq!(host.0.borrow().metrics.stats().source_bytes, baseline, "selection and enabled must not read bodies");
    session.dispatch("history.extract", &params).unwrap();
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, bytes, "extract only authenticates chosen body");
    assert!(host.0.borrow().metrics.stats().source_bytes > baseline);
    session.dispatch("history.clear", &json!({"canvas":C,"id":N})).unwrap();
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, bytes, "clear keeps inactive bodies deferred");
    let smaller = dir.path().join("smaller.ncraft");
    session.dispatch("file.save_as", &json!({"location":smaller})).unwrap();
    assert!(std::fs::metadata(&smaller).unwrap().len() < 2 * bytes);
    std::fs::remove_file(&input).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document().canvases()[&C].nodes()[&N].history.len(), 3);
    let lease = session.assets().open_verified(&asset(hashes[0]), &cancel, &metrics).unwrap();
    assert!(lease.read_small(nodecraft_format::MAX_ASSET_BYTES, &metrics).is_err());
    struct Export {
        bytes: u64,
        max_write: usize,
        hash: blake3::Hasher,
    }
    impl std::io::Write for Export {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.bytes += data.len() as u64;
            self.max_write = self.max_write.max(data.len());
            self.hash.update(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut exported = Export { bytes: 0, max_write: 0, hash: blake3::Hasher::new() };
    assert_eq!(session.assets().copy_verified_to(&asset(hashes[0]), &mut exported, &cancel, &metrics).unwrap(), bytes);
    assert_eq!(exported.bytes, bytes);
    assert!(exported.max_write <= nodecraft_format::asset_source::TRANSFER_BYTES);
    assert_eq!(exported.hash.finalize().as_bytes(), hashes[0].as_bytes());
    let mut reader = lease.reader(metrics.clone());
    let mut prefix = [0; 80];
    reader.read_exact(&mut prefix).unwrap();
    assert_eq!(&prefix[..PNG.len()], PNG);
    drop((lease, session, host));
    reader.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, [1; 80], "retained reader survives Session/store drop");
}
fn snapshot_empty() -> Snapshot {
    snapshot(AssetStore::default())
}

#[test]
fn native_duplicate_at_count_saturation_reuses_candidate_but_not_context_only_reader_pins() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let mut hashes = Vec::new();
    for n in 0..nodecraft_format::MAX_ASSETS {
        hashes.push(session.import_asset(format!("native {n}").as_bytes()).unwrap());
    }
    assert_eq!(session.assets().len(), 1024);
    assert_eq!(session.import_asset(b"native 0").unwrap(), hashes[0]);
    assert!(session.import_asset(b"new native").is_err());
    assert_eq!(session.assets().len(), 1024);
    let keep = hashes.iter().copied().collect();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    session.assets().verify_retained(&keep, &cancel, &metrics).unwrap();
    let readers = hashes
        .iter()
        .map(|hash| {
            session
                .assets()
                .open_verified(&nodecraft_types::AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }, &cancel, &metrics)
                .unwrap()
        })
        .collect::<Vec<_>>();
    session.dispatch("file.save_as", &json!({"location":dir.path().join("empty.ncraft")})).unwrap();
    assert!(session.assets().is_empty());
    assert_eq!(session.assets().context().retained_usage().unwrap().0, 1024);
    assert!(session.import_asset(b"native 0").is_err(), "context-only identities do not grant candidate content lookup at saturation");
    drop(readers);
    session.assets().collect_garbage().unwrap();
    assert_eq!(session.assets().context().retained_usage().unwrap().0, 0);
    session.import_asset(b"native 0").unwrap();
}

#[test]
fn imported_edit_append_failure_preserves_live_state_and_latches_until_save() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let source = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"new asset".as_slice()))), 0, 9).unwrap();
    let prepared = session.prepare_imports(&[source], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let hash = prepared.hashes()[0];
    let mut node = Node::new(NodeId::from_bytes([8; 16]), "test.media");
    node.params.insert("asset".into(), Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }));
    let before = session.document().clone();
    let bytes = std::fs::read(dir.path().join("recovery")).unwrap();
    host.0.borrow_mut().fail_append = true;
    assert!(session.dispatch_with_imports("canvas.paste", &json!({"canvas":C,"nodes":[node]}), prepared).is_err());
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
    assert!(session.assets().is_empty());
    assert!(session.file_status().recovery_required);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), bytes);
    assert!(session.dispatch("node.move", &movement(1)).is_err());
    host.0.borrow_mut().fail_append = false;
    session.dispatch("file.save_as", &json!({"location":dir.path().join("repair.ncraft")})).unwrap();
    session.dispatch("node.move", &movement(1)).unwrap();
}
#[test]
fn save_open_stale_batches_and_non_edit_file_requests_do_not_use_host_routes() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let source = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"batch".as_slice()))), 0, 5).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let before_save = session.prepare_imports(std::slice::from_ref(&source), &cancel, &metrics).unwrap();
    let path = dir.path().join("saved.ncraft");
    session.dispatch("file.save_as", &json!({"location":path})).unwrap();
    let calls = host.0.borrow().appends;
    assert!(session.dispatch_with_imports("node.move", &movement(1), before_save).is_err());
    assert_eq!(host.0.borrow().appends, calls);
    let before_open = session.prepare_imports(std::slice::from_ref(&source), &cancel, &metrics).unwrap();
    session.dispatch("file.open", &json!({"location":path})).unwrap();
    assert!(session.dispatch_with_imports("node.move", &movement(1), before_open).is_err());
    let batch = session.prepare_imports(&[source], &cancel, &metrics).unwrap();
    let saves = host.0.borrow().saves;
    assert_eq!(
        session.dispatch_with_imports("file.save_as", &json!({"location":dir.path().join("must-not-write")}), batch).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(host.0.borrow().saves, saves);
    assert!(!dir.path().join("must-not-write").exists());
    assert!(session.assets().is_empty());
}
#[test]
fn published_readonly_capability_preserves_local_edits_and_save_as() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    host.0.borrow_mut().saved_readonly_once = true;
    session.dispatch("file.save_as", &json!({"location":dir.path().join("readonly.ncraft")})).unwrap();
    assert!(session.file_status().read_only);
    let calls = host.0.borrow().saves;
    assert!(session.dispatch("file.save", &json!({})).is_err());
    assert_eq!(host.0.borrow().saves, calls);
    session.dispatch("node.move", &movement(1)).unwrap();
    assert!(session.file_status().dirty);
    session.dispatch("file.save_as", &json!({"location":dir.path().join("writable.ncraft")})).unwrap();
    assert!(!session.file_status().read_only);
    assert!(!session.file_status().dirty);
    session.close_recovery(false).unwrap();
    assert!(!session.file_status().available);
}
#[test]
fn corrupt_chosen_and_restored_history_refuse_before_append_but_other_inactive_body_stays_lazy() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource};
    use std::{
        io::{Cursor, Write},
        sync::Arc,
    };
    let good = nodecraft_types::Blake3::from_bytes(*blake3::hash(PNG).as_bytes());
    let bad = nodecraft_types::Blake3::from_bytes(*blake3::hash(b"good").as_bytes());
    let graph = history_snapshot(AssetStore::default(), &[good, bad]).project;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("project.json", options).unwrap();
    writer.write_all(&serde_json::to_vec(&json!({"version":4,"epoch":0,"project":graph})).unwrap()).unwrap();
    for (hash, body) in [(good, PNG), (bad, b"evil".as_slice())] {
        writer.start_file(format!("assets/{hash}"), options).unwrap();
        writer.write_all(body).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("corrupt-unused.ncraft");
    std::fs::write(&input, writer.finish().unwrap().into_inner()).unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    session.dispatch("file.open", &json!({"location":input,"discard_dirty":true})).unwrap();
    session.dispatch("history.select", &json!({"canvas":C,"id":N,"generation":generation(1)})).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let source = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"candidate".as_slice()))), 0, 9).unwrap();
    let batch = session.prepare_imports(&[source], &cancel, &metrics).unwrap();
    // Cache verification alone leaves the Session source stamp live.
    session.assets().open_verified(&asset(good), &cancel, &metrics).unwrap();
    let params = json!({"canvas":C,"id":N,"generation":generation(2),"node":NodeId::from_bytes([9;16]),"pos":{"x":0,"y":0}});
    session.enabled("history.extract", &params).unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    let status = session.file_status();
    let appends = host.0.borrow().appends;
    let journal = std::fs::read(dir.path().join("recovery")).unwrap();
    assert!(session.dispatch("history.extract", &params).is_err());
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.file_status(), status);
    assert_eq!(host.0.borrow().appends, appends);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal);
    struct Counter(u64);
    impl Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len() as u64;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Counter(0);
    assert!(session.assets().copy_verified_to(&asset(bad), &mut output, &cancel, &metrics).is_err());
    assert_eq!(output.0, 0, "corrupt content must not reach consumer output");
    session.dispatch_with_imports("node.move", &movement(1), batch).unwrap();
    let selected = json!({"canvas":C,"id":N,"generation":generation(1),"node":NodeId::from_bytes([7;16]),"pos":{"x":10,"y":10}});
    session.dispatch("history.extract", &selected).unwrap();
    session.dispatch("history.clear", &json!({"canvas":C,"id":N})).unwrap();
    let before = session.document().clone();
    let appends = host.0.borrow().appends;
    session.enabled("edit.undo", &json!({})).unwrap();
    assert!(session.dispatch("edit.undo", &json!({})).is_err(), "AddGeneration restores the full touched-node invariant including inactive history");
    assert_eq!(session.document(), &before);
    assert_eq!(host.0.borrow().appends, appends);
    assert!(!session.file_status().recovery_required);
}

#[test]
fn legacy_or_missing_context_hosts_refuse_native_authority_before_legacy_calls() {
    use std::cell::Cell;
    struct Legacy {
        calls: Rc<Cell<usize>>,
        streaming: bool,
    }
    impl ProjectStorage for Legacy {
        fn capability(&self) -> StorageCapability {
            if self.streaming { StorageCapability::StreamingV2 } else { StorageCapability::SmallV1 }
        }
        fn load(&mut self, _: &StorageLocation) -> Result<Vec<u8>, Error> {
            self.calls.set(self.calls.get() + 1);
            Err(legacy())
        }
        fn save(&mut self, _: &StorageLocation, _: &[u8]) -> Result<(), Error> {
            self.calls.set(self.calls.get() + 1);
            Err(legacy())
        }
        fn checkpoint(&mut self, _: &[u8]) -> Result<(), Error> {
            self.calls.set(self.calls.get() + 1);
            Err(legacy())
        }
        fn append(&mut self, _: &[u8]) -> Result<(), Error> {
            self.calls.set(self.calls.get() + 1);
            Err(legacy())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let native = host(dir.path());
    let mut assets = AssetStore::with_context(native.asset_context().unwrap());
    assets.insert(b"native").unwrap();
    let calls = Rc::new(Cell::new(0));
    let error = Session::with_storage(snapshot(assets), Box::new(Legacy { calls: calls.clone(), streaming: false })).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(calls.get(), 0);
    let error = Session::with_storage(snapshot_empty(), Box::new(Legacy { calls: calls.clone(), streaming: true })).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(calls.get(), 0);
}

#[test]
fn logical_collection_preserves_native_reader_and_preparation_pins() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let hash = session.import_asset(b"reader").unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let reference = nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() };
    let reader = session.assets().open_verified(&reference, &cancel, &metrics).unwrap();
    let input = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"prepared".as_slice()))), 0, 8).unwrap();
    let prepared = session.prepare_imports(&[input], &cancel, &metrics).unwrap();
    let status = session.file_status();
    let appends = host.0.borrow().appends;
    assert!(session.collect_assets().unwrap());
    assert!(session.assets().is_empty());
    assert_eq!(session.assets().context().retained_usage().unwrap().0, 2);
    assert_eq!(reader.read_small(16, &metrics).unwrap(), b"reader");
    assert_eq!(session.file_status(), status);
    assert_eq!(host.0.borrow().appends, appends);
    assert_eq!(session.dispatch_with_imports("node.move", &movement(1), prepared).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(session.assets().context().retained_usage().unwrap().0, 1);
    drop(reader);
    assert!(!session.collect_assets().unwrap());
    assert_eq!(session.assets().context().retained_usage().unwrap().0, 0);
}

#[test]
fn logical_collection_does_not_shrink_recorded_journal_budget_and_only_checkpoint_resets_it() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let mut values = Vec::new();
    for n in 0..nodecraft_format::MAX_ASSETS {
        let hash = session.import_asset(format!("recorded {n}").as_bytes()).unwrap();
        values.push(Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }));
    }
    let media = NodeId::from_bytes([80; 16]);
    let mut node = Node::new(media, "test.media");
    node.params.insert("assets".into(), Value::List(values));
    session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    session.dispatch("node.remove", &json!({"canvas":C,"id":media})).unwrap();
    assert!(!session.collect_assets().unwrap(), "undo still retains every identity");
    // Exercise the actual public bounded history eviction, without an invented discard API.
    for x in 1..=nodecraft_engine::session::MAX_HISTORY_STEPS {
        session.dispatch("node.move", &movement(x as i32)).unwrap();
    }
    let status = session.file_status();
    let revision = session.revision();
    let journal = std::fs::read(dir.path().join("recovery")).unwrap();
    assert!(session.collect_assets().unwrap());
    assert!(session.assets().is_empty());
    assert_eq!(session.file_status(), status);
    assert_eq!(session.revision(), revision);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal);
    let hash = session.import_asset(b"new recorded identity").unwrap();
    let mut node = Node::new(media, "test.media");
    node.params.insert("asset".into(), Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }));
    let edit = json!({"canvas":C,"node":node});
    let appends = host.0.borrow().appends;
    assert!(session.dispatch("node.add", &edit).is_err(), "GC must not erase checkpoint-plus-tail accounting");
    assert_eq!(host.0.borrow().appends, appends);
    assert!(!session.file_status().recovery_required);
    // An actual failed destination publication cannot reset either copy of the ledger.
    assert!(session.dispatch("file.save_as", &json!({"location":dir.path()})).is_err());
    assert!(session.dispatch("node.add", &edit).is_err());
    assert_eq!(host.0.borrow().appends, appends);
    assert!(!session.file_status().recovery_required);
    // Published bytes with a failed replacement checkpoint are saved but edits stay latched.
    host.0.borrow_mut().fail_checkpoint = true;
    let saved = session.dispatch("file.save_as", &json!({"location":dir.path().join("reset.ncraft")})).unwrap();
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["recovery_ready"], false);
    assert!(session.dispatch("node.add", &edit).is_err());
    assert_eq!(session.dispatch("file.save", &json!({})).unwrap()["recovery_ready"], true);
    assert_eq!(session.import_asset(b"new recorded identity").unwrap(), hash);
    session.dispatch("node.add", &edit).unwrap();
    assert_eq!(host.0.borrow().native.recover().unwrap().snapshot.project, *session.document());
}

#[test]
fn unknown_reader_large_native_batch_pastes_once_and_never_recopies_caller() {
    use std::io::{self, Read};
    struct Generated {
        left: u64,
        observed: u64,
        maximum: usize,
        hash: blake3::Hasher,
    }
    impl Read for Generated {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.maximum = self.maximum.max(out.len());
            let n = self.left.min(out.len() as u64) as usize;
            out[..n].fill(b'x');
            self.hash.update(&out[..n]);
            self.left -= n as u64;
            self.observed += n as u64;
            Ok(n)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let bytes = nodecraft_format::MAX_ASSET_BYTES as u64 + 1025;
    let mut generated = Generated { left: bytes, observed: 0, maximum: 0, hash: blake3::Hasher::new() };
    let mut a = io::Cursor::new(b"small");
    let mut b = io::Cursor::new(b"small");
    let metrics = StreamMetrics::default();
    let cancel = TransferCancel::default();
    let batch = session.prepare_reader_imports(&mut [&mut generated, &mut a, &mut b], &cancel, &metrics).unwrap();
    let hashes = batch.hashes().to_vec();
    assert_eq!(hashes[1], hashes[2]);
    assert_ne!(hashes[0], hashes[1]);
    assert_eq!(hashes[0].as_bytes(), generated.hash.finalize().as_bytes());
    assert_eq!(generated.observed, bytes);
    assert!(generated.maximum <= 64 * 1024);
    assert_eq!(metrics.stats().live_transfer_bytes, 0);
    assert_eq!(metrics.stats().peak_transfer_bytes, 64 * 1024);
    assert!(metrics.stats().source_bytes < bytes + 2 * 64 * 1024, "one caller pass plus bounded intrinsic sniff, no full spool recopy");
    assert!(session.assets().is_empty());
    let mut node = Node::new(NodeId::from_bytes([90; 16]), "test.media");
    node.params.insert(
        "assets".into(),
        Value::List(
            hashes[..2]
                .iter()
                .map(|hash| Value::Media(nodecraft_types::AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }))
                .collect(),
        ),
    );
    session.dispatch_with_imports("canvas.paste", &json!({"canvas":C,"nodes":[node]}), batch).unwrap();
    assert_eq!(session.revision(), 1);
    assert_eq!(session.assets().len(), 2);
    assert_eq!(session.assets().descriptor(&hashes[0]).unwrap().bytes, bytes);
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!session.can_undo());
    session.dispatch("edit.redo", &json!({})).unwrap();
    let lease = session
        .assets()
        .open_verified(&nodecraft_types::AssetRef { hash: hashes[0], mime: "text/plain".parse().unwrap(), meta: Default::default() }, &cancel, &metrics)
        .unwrap();
    assert!(lease.read_small(nodecraft_format::MAX_ASSET_BYTES, &metrics).is_err());
    let mut reader = lease.reader(metrics.clone());
    drop((session, host, generated));
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    let mut actual = 0u64;
    loop {
        let n = reader.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        actual += n as u64;
    }
    assert_eq!(actual, bytes);
    assert_eq!(hash.finalize().as_bytes(), hashes[0].as_bytes());
}

#[test]
fn unknown_reader_late_failure_and_stale_receipts_never_publish_candidate_assets() {
    use std::io::{self, Read};
    struct Failed;
    impl Read for Failed {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("private detail"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let before = session.document().clone();
    let status = session.file_status();
    assert!(session.prepare_reader_imports(&mut [&mut io::Cursor::new(b"first"), &mut Failed], &cancel, &metrics).is_err());
    assert!(session.assets().is_empty());
    assert_eq!(session.document(), &before);
    assert_eq!(session.file_status(), status);
    session.import_asset(b"orphan").unwrap();
    let batch = session.prepare_reader_imports(&mut [&mut io::Cursor::new(b"candidate")], &cancel, &metrics).unwrap();
    assert!(session.collect_assets().unwrap());
    let appends = host.0.borrow().appends;
    assert_eq!(session.dispatch_with_imports("node.move", &movement(1), batch).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(host.0.borrow().appends, appends);
    let batch = session.prepare_reader_imports(&mut [&mut io::Cursor::new(b"candidate")], &cancel, &metrics).unwrap();
    let foreign_dir = tempfile::tempdir().unwrap();
    let foreign_host = self::host(foreign_dir.path());
    let mut foreign = Session::with_storage(snapshot_empty(), Box::new(foreign_host.clone())).unwrap();
    assert_eq!(foreign.dispatch_with_imports("node.move", &movement(1), batch).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(foreign_host.0.borrow().appends, 0);
    let path = dir.path().join("saved.ncraft");
    session.dispatch("file.save_as", &json!({"location":path})).unwrap();
    for open in [false, true] {
        let batch = session.prepare_reader_imports(&mut [&mut io::Cursor::new(b"candidate")], &cancel, &metrics).unwrap();
        if open {
            session.dispatch("file.open", &json!({"location":path})).unwrap();
        } else {
            session.dispatch("file.save", &json!({})).unwrap();
        }
        let appends = host.0.borrow().appends;
        assert_eq!(session.dispatch_with_imports("node.move", &movement(1), batch).unwrap_err().kind, ErrorKind::Conflict);
        assert_eq!(host.0.borrow().appends, appends);
        assert!(session.assets().is_empty());
    }
    // The same touched-reference admission rejects a mislabelled reader input atomically.
    let batch = session.prepare_reader_imports(&mut [&mut io::Cursor::new(PNG)], &cancel, &metrics).unwrap();
    let mut node = Node::new(NodeId::from_bytes([91; 16]), "test.media");
    node.params.insert(
        "asset".into(),
        Value::Media(nodecraft_types::AssetRef { hash: batch.hashes()[0], mime: "image/jpeg".parse().unwrap(), meta: Default::default() }),
    );
    assert!(session.dispatch_with_imports("node.add", &json!({"canvas":C,"node":node}), batch).is_err());
    assert!(session.assets().is_empty());
    assert!(!session.can_undo());
    assert_eq!(metrics.stats().live_transfer_bytes, 0);
}

#[test]
fn unknown_reader_candidate_duplicates_at_count_saturation_need_real_spools() {
    use std::io::Cursor;
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host)).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    for n in 0..nodecraft_format::MAX_ASSETS {
        session.import_asset_reader(&mut Cursor::new(format!("reader {n}")), &cancel, &metrics).unwrap();
    }
    let hash = session.assets().hashes().next().copied().unwrap();
    let expected = nodecraft_types::Blake3::from_bytes(*blake3::hash(b"reader 0").as_bytes());
    let mut duplicate = Cursor::new(b"reader 0");
    assert_eq!(session.import_asset_reader(&mut duplicate, &cancel, &metrics).unwrap(), expected);
    assert_eq!(duplicate.position(), 8);
    assert_eq!(session.assets().len(), 1024);
    let mut new = Cursor::new(b"new reader");
    assert!(session.import_asset_reader(&mut new, &cancel, &metrics).is_err());
    assert_eq!(new.position(), 10);
    assert!(session.import_asset_reader(&mut Cursor::new(b""), &cancel, &metrics).is_err(), "zero-byte new identity still requires a count slot");
    assert!(session.assets().contains(&hash));
    assert_eq!(session.assets().len(), 1024);
    let pins = session
        .assets()
        .hashes()
        .map(|hash| {
            session
                .assets()
                .open_verified(&nodecraft_types::AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }, &cancel, &metrics)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(session.collect_assets().unwrap());
    assert_eq!(session.assets().context().retained_usage().unwrap().0, 1024);
    let mut context_only = Cursor::new(b"reader 0");
    assert!(session.import_asset_reader(&mut context_only, &cancel, &metrics).is_err());
    assert_eq!(context_only.position(), 8, "unknown intake spools before conservative context-only identity refusal");
    drop(pins);
    assert!(!session.collect_assets().unwrap());
    session.import_asset_reader(&mut Cursor::new(b"reader 0"), &cancel, &metrics).unwrap();
}

#[test]
fn unknown_reader_duplicate_never_repairs_corrupt_indexed_content() {
    use std::io::{Cursor, Write};
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(b"good").as_bytes());
    let graph = history_snapshot(AssetStore::default(), &[hash]).project;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("project.json", options).unwrap();
    zip.write_all(&serde_json::to_vec(&json!({"version":4,"epoch":0,"project":graph})).unwrap()).unwrap();
    zip.start_file(format!("assets/{hash}"), options).unwrap();
    zip.write_all(b"evil").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("corrupt.ncraft");
    std::fs::write(&input, zip.finish().unwrap().into_inner()).unwrap();
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    session.dispatch("file.open", &json!({"location":input,"discard_dirty":true})).unwrap();
    let before = session.document().clone();
    let journal = std::fs::read(dir.path().join("recovery")).unwrap();
    let status = session.file_status();
    let metrics = StreamMetrics::default();
    assert!(session.prepare_reader_imports(&mut [&mut Cursor::new(b"good")], &TransferCancel::default(), &metrics).is_err());
    assert_eq!(session.document(), &before);
    assert_eq!(session.file_status(), status);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal);
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, 0, "good duplicate spool was dropped, old corrupt descriptor not replaced");
    assert!(session.assets().open_verified(&asset(hash), &TransferCancel::default(), &metrics).is_err());
}

mod import_composition147;

mod split_composition342;

#[test]
fn background_export_admission_never_verifies_an_indexed_body_or_waits_before_editing() {
    use nodecraft_engine::{
        export_job::{ExportPhase, ExportResult},
        save_runner::{SaveJob, SaveRunner},
    };
    use nodecraft_format::asset_source::AssetRange;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Held(Mutex<Option<SaveJob>>);
    impl SaveRunner for Held {
        fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
            *self.0.lock().unwrap() = Some(job);
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut producer = NativeStorage::new(dir.path().join("producer-recovery")).unwrap();
    let mut assets = AssetStore::with_context(producer.asset_context().unwrap());
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let bytes = 20 * 1024 * 1024 + 1;
    let hash = assets.import_stream(&AssetRange::new(Arc::new(Padded { bytes, fill: 47 }), 0, bytes).unwrap(), &cancel, &metrics).unwrap();
    let snapshot = history_snapshot(assets, &[hash]);
    let archive = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &cancel, &metrics).unwrap();
    let input = dir.path().join("large.ncraft");
    producer.save_plan(&StorageLocation::Path(input.clone()), &archive, &cancel, &metrics).unwrap();
    drop((snapshot, archive, producer));
    let native = host(dir.path());
    let mut session = Session::with_storage(snapshot_empty(), Box::new(native)).unwrap();
    session.dispatch("file.open", &json!({"location":input,"discard_dirty":true})).unwrap();
    let runner = Arc::new(Held::default());
    session.set_export_runner(runner.clone());
    let output = dir.path().join("result.png");
    let params = json!({"canvas":C,"id":N,"generation":generation(1),"path":output,"background":true});
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, 0);
    session.enabled("history.export", &params).unwrap();
    let receipt = session.dispatch("history.export", &params).unwrap();
    assert_eq!(session.export_status().unwrap().phase, ExportPhase::Queued);
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, 0, "enabled and admission must not copy or authenticate indexed media");
    assert!(!output.exists());
    let revision = session.revision();
    session.dispatch("node.move", &movement(17)).unwrap();
    assert_eq!(session.revision(), revision + 1, "an edit must not settle this read-only export");
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, 0);
    runner.0.lock().unwrap().take().unwrap().run();
    let status = session.export_status().unwrap();
    assert_eq!(serde_json::json!(status.id), receipt["pending"]);
    assert_eq!(status.result, Some(ExportResult::Exported { bytes }));
    assert_eq!(std::fs::metadata(&output).unwrap().len(), bytes);
    let mut file = std::fs::File::open(&output).unwrap();
    let mut hash_out = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        use std::io::Read;
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hash_out.update(&buffer[..read]);
    }
    assert_eq!(hash_out.finalize().as_bytes(), hash.as_bytes());
}

#[test]
fn native_small_save_opens_in_distinct_memory_session_and_keeps_asset_history() {
    let dir = tempfile::tempdir().unwrap();
    let native = host(dir.path());
    let mut original = snapshot(AssetStore::default());
    let hash = original.assets.insert(b"portable small payload").unwrap();
    let mut media = Node::new(NodeId::from_bytes([81; 16]), "test.media");
    media.params.insert("asset".into(), Value::Media(nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }));
    Change::AddNode { canvas: C, node: media }.apply(&mut original.project).unwrap();
    let mut session = Session::with_storage(original, Box::new(native.clone())).unwrap();
    session.dispatch("node.move", &movement(7)).unwrap();
    let destination = dir.path().join("portable.ncraft");
    session.dispatch("file.save_as", &json!({"location":destination})).unwrap();
    let bytes = std::fs::read(&destination).unwrap();
    let decoded = nodecraft_format::decode(&bytes).unwrap();
    assert_eq!(decoded.project, *session.document());
    assert_eq!(decoded.assets.get(&hash).unwrap(), b"portable small payload");
    let mut memory = nodecraft_format::MemoryStorage::default();
    let location = StorageLocation::Path("portable".into());
    memory.files.insert(location, bytes);
    let mut other = Session::with_storage(snapshot(AssetStore::default()), Box::new(memory)).unwrap();
    other.dispatch("file.open", &json!({"location":"portable","discard_dirty":true})).unwrap();
    assert_eq!(other.document(), session.document());
    assert!(!other.can_undo());
    assert!(!other.can_redo());
    assert_eq!(other.assets().get(&hash).unwrap(), b"portable small payload");
    other.dispatch("node.move", &movement(8)).unwrap();
    other.dispatch("edit.undo", &json!({})).unwrap();
    other.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(other.assets().get(&hash).unwrap(), b"portable small payload");
    assert_eq!(native.0.borrow().native.recover().unwrap().snapshot.project, *session.document());
}

#[test]
fn desktop_recovery_adopts_classic_comments_into_native_context_before_next_edit() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("old-recovery");
    std::fs::write(&journal_path, include_bytes!("../../format/tests/fixtures/v2/comments-v2.ncrj")).unwrap();
    let previous = NativeStorage::new(journal_path).unwrap();
    let recovered = previous.recover().unwrap();
    assert_eq!(recovered.applied, 3);
    let host = host(dir.path());
    let mut session = Session::with_storage(snapshot(AssetStore::default()), Box::new(host.clone())).unwrap();
    session.recover(recovered.snapshot, None).unwrap();
    assert!(session.document().canvases().values().any(|canvas| !canvas.comments().is_empty()));
    session.dispatch("node.move", &movement(80)).unwrap();
    host.0.borrow_mut().native.flush().unwrap();
    let replayed = host.0.borrow().native.recover().unwrap();
    assert!(!replayed.legacy);
    assert_eq!(replayed.applied, 1);
    assert_eq!(replayed.snapshot.project, *session.document());
}
