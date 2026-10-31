//! The disk-space preflight's archive estimate (#367): it never understates what a native or
//! small save actually prepares, and stays within a few bytes of it.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, NativeAssetWorkspace, Snapshot, StorageCapability,
    asset_source::{StreamMetrics, TransferCancel},
    disk_quota::DiskQuota,
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, MediaMeta, NodeId, ProjectId, Value};

fn project(hashes: &[Blake3]) -> Project {
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Space").unwrap();
    let c = CanvasId::from_bytes([4; 16]);
    Change::AddCanvas { canvas: Canvas::new(c, "Canvas") }.apply(&mut p).unwrap();
    for (n, hash) in hashes.iter().enumerate() {
        let mut id = [0; 16];
        id[..8].copy_from_slice(&(n as u64).to_le_bytes());
        let mut node = Node::new(NodeId::from_bytes(id), "source.text");
        node.params.insert("content".into(), Value::Media(AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
        Change::AddNode { canvas: c, node }.apply(&mut p).unwrap();
    }
    p
}

#[test]
fn the_estimate_covers_the_prepared_archive_without_reading_assets() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let mut store = AssetStore::with_context(AssetContext::native(workspace));
    let mut hashes = Vec::new();
    for n in 0..12u8 {
        hashes.push(store.insert(&vec![b'a' + n; 1000 + usize::from(n) * 333]).unwrap());
    }
    // An asset nothing references isn't archived, so it isn't counted either.
    store.insert(&vec![b'z'; 50_000]).unwrap();
    let snapshot = Snapshot { project: project(&hashes), assets: store, epoch: 9 };
    let estimate = ArchivePlan::estimate_bytes(&snapshot.project, &snapshot.assets, snapshot.epoch, StorageCapability::StreamingV2).unwrap();
    let plan = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert!(estimate >= plan.byte_len(), "estimate {estimate} understates {}", plan.byte_len());
    assert!(estimate - plan.byte_len() <= 16, "estimate {estimate} vs {}", plan.byte_len());

    let mut small = AssetStore::default();
    let hash = small.insert(b"small").unwrap();
    let snapshot = Snapshot { project: project(&[hash]), assets: small, epoch: 1 };
    let estimate = ArchivePlan::estimate_bytes(&snapshot.project, &snapshot.assets, snapshot.epoch, StorageCapability::SmallV1).unwrap();
    let plan = ArchivePlan::prepare(&snapshot, StorageCapability::SmallV1, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(estimate, plan.byte_len());
}

#[test]
fn a_missing_asset_is_an_error_not_a_zero_estimate() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let store = AssetStore::with_context(AssetContext::native(workspace));
    let missing = Blake3::from_bytes([7; 32]);
    assert!(ArchivePlan::estimate_bytes(&project(&[missing]), &store, 0, StorageCapability::StreamingV2).is_err());
}
