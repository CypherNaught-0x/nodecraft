#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, NativeAssetWorkspace, Snapshot, StorageCapability,
    asset_source::{AssetRange, AssetSource, MemorySource, StreamMetrics, TransferCancel},
    disk_quota::DiskQuota,
    zip_index::ArchiveIndex,
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, Error, MediaMeta, NodeId, ProjectId, Value};
use std::{
    io::{Cursor, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
fn project(hashes: &[Blake3]) -> Project {
    let mut p = Project::new(ProjectId::from_bytes([1; 16]), "Streaming").unwrap();
    let c = CanvasId::from_bytes([2; 16]);
    Change::AddCanvas { canvas: Canvas::new(c, "Canvas") }.apply(&mut p).unwrap();
    for (n, hash) in hashes.iter().enumerate() {
        let mut id = [0; 16];
        id[..8].copy_from_slice(&(n as u64).to_le_bytes());
        let mut node = Node::new(NodeId::from_bytes(id), "source.text");
        node.params.insert("content".into(), Value::Media(asset(*hash)));
        Change::AddNode { canvas: c, node }.apply(&mut p).unwrap();
    }
    p
}
fn asset(hash: Blake3) -> AssetRef {
    AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }
}
fn setup() -> (tempfile::TempDir, NativeAssetWorkspace, AssetStore) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let store = AssetStore::with_context(AssetContext::native(workspace.clone()));
    (dir, workspace, store)
}
fn range(bytes: Vec<u8>) -> AssetRange {
    let len = bytes.len() as u64;
    AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, len).unwrap()
}
#[test]
fn native_small_plan_is_v1_exact_reusable_and_pins_content_through_gc() {
    let (_dir, workspace, mut store) = setup();
    let baseline = workspace.quota().usage().unwrap();
    let mut hashes = Vec::new();
    // Exercise output compaction before freezing all leases, not just the import threshold.
    for n in 0..28 {
        hashes.push(store.insert(&vec![65 + n; 4096]).unwrap());
    }
    let snapshot = Snapshot { project: project(&hashes), assets: store.clone(), epoch: 7 };
    let metrics = StreamMetrics::default();
    let plan = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &TransferCancel::default(), &metrics).unwrap();
    assert_eq!(plan.capability(), StorageCapability::StreamingV2);
    assert!(plan.small_bytes().is_err());
    assert_eq!(plan.asset_lengths().len(), 28);
    assert!(workspace.quota().backing_count().unwrap() < 24);
    let mut first = Vec::new();
    plan.write_to(&mut first, &TransferCancel::default(), &metrics).unwrap();
    assert_eq!(first.len() as u64, plan.byte_len());
    assert_eq!(*blake3::hash(&first).as_bytes(), *plan.digest().hash.as_bytes());
    let index = ArchiveIndex::read(&range(first.clone()), &TransferCancel::default(), &metrics).unwrap();
    assert_eq!(index.version(), 1);
    let resident = nodecraft_format::decode(&first).unwrap();
    assert_eq!(resident.project, snapshot.project);
    assert_eq!(resident.epoch, snapshot.epoch);
    for hash in &hashes {
        assert_eq!(resident.assets.get(hash).unwrap().len(), 4096);
    }
    let decoded = nodecraft_format::decode_indexed(&index, store.context().clone()).unwrap();
    assert_eq!(decoded.project, snapshot.project);
    assert_eq!(decoded.epoch, 7);
    drop((decoded, index, snapshot));
    store.retain(&Default::default());
    store.collect_garbage().unwrap();
    assert_eq!(store.context().retained_usage().unwrap(), (28, 28 * 4096));
    let mut second = Vec::new();
    plan.write_to(&mut second, &TransferCancel::default(), &metrics).unwrap();
    assert_eq!(first, second);
    let cancel = TransferCancel::default();
    cancel.cancel();
    let mut empty = Vec::new();
    assert!(plan.write_to(&mut empty, &cancel, &metrics).is_err());
    assert!(empty.is_empty());
    drop(plan);
    store.collect_garbage().unwrap();
    assert_eq!(workspace.quota().usage().unwrap(), baseline);
}
#[test]
fn small_plan_preserves_frozen_v1_and_browser_capability_refuses_lazy_before_reads() {
    let mut store = AssetStore::default();
    let h = store.insert(b"small").unwrap();
    let snapshot = Snapshot { project: project(&[h]), assets: store, epoch: 1 };
    let plan = ArchivePlan::prepare(&snapshot, StorageCapability::SmallV1, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(plan.small_bytes().unwrap(), nodecraft_format::encode(&snapshot).unwrap());
    assert!(ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    let (_dir, _workspace, native) = setup();
    let bytes = archive(&project(&[h]), h, b"small");
    let source = Arc::new(Meter { bytes, reads: AtomicU64::new(0) });
    let input = AssetRange::new(source.clone(), 0, source.byte_len()).unwrap();
    let index = ArchiveIndex::read(&input, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let lazy = nodecraft_format::decode_indexed(&index, native.context().clone()).unwrap();
    source.reads.store(0, Ordering::Relaxed);
    assert!(ArchivePlan::prepare(&lazy, StorageCapability::SmallV1, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
}
struct Meter {
    bytes: Vec<u8>,
    reads: AtomicU64,
}
impl AssetSource for Meter {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let at = usize::try_from(at).unwrap();
        let bytes = self.bytes.get(at..).unwrap_or_default();
        let n = bytes.len().min(out.len());
        out[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }
}
fn archive(project: &Project, hash: Blake3, body: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("project.json", options).unwrap();
    writer.write_all(&serde_json::to_vec(&serde_json::json!({"version":4,"epoch":0,"project":project})).unwrap()).unwrap();
    writer.start_file(format!("assets/{hash}"), options).unwrap();
    writer.write_all(body).unwrap();
    writer.finish().unwrap().into_inner()
}
#[test]
fn deferred_corruption_and_cached_crc_alias_refuse_before_output_publication() {
    let (_dir, workspace, mut store) = setup();
    let hash = store.insert(b"valid").unwrap();
    let p = project(&[hash]);
    // Same canonical name and length but another well-formed ZIP body: its CRC must not
    // borrow the existing verified cache's identity just because that hash is cached.
    let input = range(archive(&p, hash, b"wrong"));
    let index = ArchiveIndex::read(&input, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let lazy = nodecraft_format::decode_indexed(&index, store.context().clone()).unwrap();
    let before = workspace.quota().usage().unwrap();
    assert!(lazy.assets.open_verified(&asset(hash), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert!(ArchivePlan::prepare(&lazy, StorageCapability::StreamingV2, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(workspace.quota().usage().unwrap(), before);
    // A fresh context cannot borrow any cache: hash verification still rejects the body.
    let (_dir2, workspace2, store2) = setup();
    let baseline = workspace2.quota().usage().unwrap();
    let lazy = nodecraft_format::decode_indexed(&index, store2.context().clone()).unwrap();
    assert_eq!(workspace2.quota().usage().unwrap(), baseline);
    assert!(lazy.assets.open_verified(&asset(hash), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(workspace2.quota().usage().unwrap(), baseline);
}

#[test]
fn empty_native_saved_bytes_open_in_frozen_small_decoder_without_changing_capability() {
    let dir = tempfile::tempdir().unwrap();
    let storage = nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap();
    let store = AssetStore::with_context(nodecraft_format::ProjectStorage::asset_context(&storage).unwrap());
    let snapshot = Snapshot { project: project(&[]), assets: store, epoch: 11 };
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let plan = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &cancel, &metrics).unwrap();
    assert_eq!(plan.capability(), StorageCapability::StreamingV2);
    assert!(plan.small_bytes().is_err());
    let mut bytes = Vec::new();
    plan.write_to(&mut bytes, &cancel, &metrics).unwrap();
    let decoded = nodecraft_format::decode(&bytes).unwrap();
    assert_eq!(decoded.project, snapshot.project);
    assert_eq!(decoded.epoch, 11);
    let checkpoint = nodecraft_format::CheckpointPlan::new(plan.clone()).unwrap();
    let mut journal = Vec::new();
    checkpoint.write_to(&mut journal, &cancel, &metrics).unwrap();
    assert_eq!(&journal[..8], b"NCRJ0004");
    assert_eq!(&journal[80..], bytes);
    assert_eq!(checkpoint.archive().digest(), plan.digest());
}

#[test]
fn native_wire_selection_does_not_bypass_shared_manifest_or_asset_count_caps() {
    let (_dir, workspace, mut store) = setup();
    let mut hashes = Vec::new();
    for n in 0..nodecraft_format::MAX_ASSETS {
        hashes.push(store.insert(&(n as u64).to_le_bytes()).unwrap());
    }
    let before = store.len();
    assert!(store.insert(&(nodecraft_format::MAX_ASSETS as u64).to_le_bytes()).is_err());
    assert_eq!(store.len(), before);
    let snapshot = Snapshot { project: project(&hashes), assets: store, epoch: 1 };
    let plan = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(plan.asset_lengths().len(), nodecraft_format::MAX_ASSETS);
    drop((plan, snapshot));
    workspace.cleanup().unwrap();

    let (_dir, workspace, store) = setup();
    let mut project = project(&[]);
    for n in 0..5 {
        let mut id = [0; 16];
        id[0] = n;
        let mut node = Node::new(NodeId::from_bytes(id), "source.text");
        node.params.insert("text".into(), Value::Text("x".repeat(1024 * 1024)));
        Change::AddNode { canvas: CanvasId::from_bytes([2; 16]), node }.apply(&mut project).unwrap();
    }
    let snapshot = Snapshot { project, assets: store, epoch: 1 };
    let usage = workspace.quota().usage().unwrap();
    assert!(ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(workspace.quota().usage().unwrap(), usage);
}
