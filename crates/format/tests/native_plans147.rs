#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{
    ArchivePlan, AssetStore, CheckpointPlan, NativeStorage, ProjectStorage, Snapshot, StorageCapability, StorageLocation,
    asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel},
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, CanvasId, Error, MediaMeta, NodeId, ProjectId, Value};
use std::{cell::Cell, sync::Arc};
struct Pattern {
    n: u64,
    value: u8,
}
impl AssetSource for Pattern {
    fn byte_len(&self) -> u64 {
        self.n
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        let n = out.len().min(usize::try_from(self.n.saturating_sub(at)).unwrap_or(usize::MAX));
        out[..n].fill(self.value);
        Ok(n)
    }
}
fn snapshot(store: AssetStore) -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Native plans").unwrap();
    let canvas = CanvasId::from_bytes([2; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    for (n, hash) in store.hashes().enumerate() {
        let mut id = [0; 16];
        id[..8].copy_from_slice(&(n as u64).to_le_bytes());
        let mut node = Node::new(NodeId::from_bytes(id), "source.text");
        node.params.insert("content".into(), Value::Media(AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    Snapshot { project, assets: store, epoch: 5 }
}
struct Legacy {
    writes: Cell<u64>,
}
impl ProjectStorage for Legacy {
    fn load(&mut self, _: &StorageLocation) -> Result<Vec<u8>, Error> {
        unreachable!()
    }
    fn save(&mut self, _: &StorageLocation, _: &[u8]) -> Result<(), Error> {
        self.writes.set(self.writes.get() + 1);
        Ok(())
    }
    fn checkpoint(&mut self, _: &[u8]) -> Result<(), Error> {
        self.writes.set(self.writes.get() + 1);
        Ok(())
    }
    fn append(&mut self, _: &[u8]) -> Result<(), Error> {
        unreachable!()
    }
}
#[test]
fn native_large_save_and_lazy_open_use_private_immutable_bytes_after_original_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut storage = NativeStorage::new(dir.path().join("recovery-a")).unwrap();
    let mut store = AssetStore::with_context(storage.asset_context().unwrap());
    for value in [65, 66, 67] {
        let n = 20 * 1024 * 1024 + 1;
        let range = AssetRange::new(Arc::new(Pattern { n, value }), 0, n).unwrap();
        store.import_stream(&range, &cancel, &metrics).unwrap();
    }
    let snapshot = snapshot(store);
    let plan = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    assert!(plan.asset_lengths().values().all(|n| *n > 16 * 1024 * 1024));
    assert!(snapshot.assets.byte_len() > 48 * 1024 * 1024);
    let path = dir.path().join("large.ncraft");
    let location = StorageLocation::Path(path.clone());
    storage.save_plan(&location, &plan, &cancel, &metrics).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), plan.byte_len());
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let manifest: serde_json::Value = serde_json::from_reader(zip.by_name("project.json").unwrap()).unwrap();
    assert_eq!(manifest["version"], 4);
    drop(zip);
    drop(storage);
    let mut reopened = NativeStorage::new(dir.path().join("recovery-b")).unwrap();
    let context = reopened.asset_context().unwrap();
    let prepared = reopened.prepare_open_snapshot(&location, &cancel, &metrics).unwrap();
    let (loaded, checkpoint, _) = prepared.into_parts();
    assert_eq!(loaded.project, snapshot.project);
    assert_eq!(loaded.epoch, 5);
    assert_eq!(context.retained_usage().unwrap(), (3, 3 * (20 * 1024 * 1024 + 1)));
    // Index admission and whole private checkpoint transfer are distinct from per-asset verification.
    let workspace_usage = context_disk(&context, &loaded);
    assert_eq!(workspace_usage, 0);
    std::fs::write(&path, b"changed").unwrap();
    std::fs::remove_file(path).unwrap();
    reopened.commit_open();
    for hash in loaded.assets.hashes() {
        let asset = AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() };
        let lease = loaded.assets.open_verified(&asset, &cancel, &metrics).unwrap();
        let mut reader = lease.reader(metrics.clone());
        let mut first = [0; 1];
        std::io::Read::read_exact(&mut reader, &mut first).unwrap();
        assert!([65, 66, 67].contains(&first[0]));
    }
    let mut recovery = Vec::new();
    checkpoint.write_to(&mut recovery, &cancel, &metrics).unwrap();
    assert_eq!(&recovery[..8], b"NCRJ0004");
    assert_eq!(recovery.len() as u64, checkpoint.byte_len());
    assert_eq!(checkpoint.archive().digest(), plan.digest());
    // Raw archive bytes are preserved in Open's checkpoint, never re-encoded after source deletion.
    assert_eq!(*blake3::hash(&recovery[80..]).as_bytes(), *checkpoint.archive().digest().hash.as_bytes());
    let mut legacy = Legacy { writes: Cell::new(0) };
    assert!(legacy.save_plan(&location, &plan, &cancel, &metrics).is_err());
    assert!(legacy.checkpoint_plan(&checkpoint, &cancel, &metrics).is_err());
    assert_eq!(legacy.writes.get(), 0);
}
fn context_disk(context: &nodecraft_format::AssetContext, loaded: &Snapshot) -> u64 {
    // No public descriptor/get access may quietly materialize indexed bodies.
    assert!(loaded.assets.hashes().all(|hash| loaded.assets.get(hash).is_none()));
    assert!(context.supports_large_assets());
    // Actual quota is exposed on the host-owned workspace in production; this test's
    // separate allocation proof is the pre-verification backing count on context.
    context.storage_usage().unwrap().verified
}
#[test]
fn legacy_checkpoint_bytes_and_identity_remain_frozen() {
    let mut store = AssetStore::default();
    store.insert(b"unreferenced").unwrap();
    let snapshot = Snapshot { project: Project::new(ProjectId::from_bytes([8; 16]), "Small").unwrap(), assets: store, epoch: 9 };
    let archive = ArchivePlan::prepare(&snapshot, StorageCapability::SmallV1, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let plan = CheckpointPlan::new(archive).unwrap();
    let old = nodecraft_format::journal::checkpoint(&snapshot).unwrap();
    assert_eq!(plan.small_bytes().unwrap(), old.bytes);
    assert_eq!(plan.identity(), old.identity);
}
