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
use std::collections::BTreeSet;
use std::sync::Arc;

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

#[test]
fn output_space_predicts_the_compaction_an_archive_runs_and_counts_no_copies_for_verified_media() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource};
    let dir = tempfile::tempdir().unwrap();
    let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let quota = workspace.quota().clone();
    let mut store = AssetStore::with_context(AssetContext::native(workspace));
    let (cancel, metrics) = (TransferCancel::default(), StreamMetrics::default());
    let mut hashes = Vec::new();
    // Streamed imports are a backing file each; 25 is past the 24 at which output packs them.
    for n in 0..25u8 {
        let body: Arc<[u8]> = Arc::from(vec![n; 4096 + usize::from(n)]);
        let range = AssetRange::new(Arc::new(MemorySource::new(body.clone())), 0, body.len() as u64).unwrap();
        hashes.push(store.import_stream(&range, &cancel, &metrics).unwrap());
    }
    assert!(quota.backing_count().unwrap() >= 24);
    let snapshot = Snapshot { project: project(&hashes), assets: store, epoch: 2 };
    let output = snapshot.assets.output_space(&snapshot.project, &BTreeSet::new()).unwrap().unwrap();
    // Imports are verified as they arrive: nothing is copied again, but all of it is packed.
    assert_eq!(output.verified_copies, 0);
    let cached: u64 = (0..25u64).map(|n| 4096 + n).sum();
    assert_eq!(output.compaction, Some(cached));
    assert_eq!(output.cleanup_debt, 0);
    ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &cancel, &metrics).unwrap();
    assert!(quota.backing_count().unwrap() < 24, "the archive did compact, as predicted");
    // Packed, the next output needs no pack.
    assert_eq!(snapshot.assets.output_space(&snapshot.project, &BTreeSet::new()).unwrap().unwrap().compaction, None);
}

#[test]
fn a_classic_open_checks_its_asset_copies_once_the_file_is_read() {
    use nodecraft_format::{
        asset_source::{AssetRange, MemorySource},
        space::{SpaceProbe, check_open_copies},
        zip_index::ArchiveIndex,
    };
    let mut store = AssetStore::default();
    let hashes: Vec<_> = (0..4u8).map(|n| store.insert(&vec![n; 100_000]).unwrap()).collect();
    let bytes: Arc<[u8]> = Arc::from(nodecraft_format::encode(&Snapshot { project: project(&hashes), assets: store, epoch: 1 }).unwrap());
    let file = bytes.len() as u64;
    let range = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, file).unwrap();
    let index = ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let context = AssetContext::native(NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap());
    let probe = |free| SpaceProbe { recovery_free: Some(free), ..SpaceProbe::default() };
    // A journal at least as large as the new checkpoint: its replace adds nothing anywhere.
    let journal = Some(file + 80);
    // Four 100 kB copies and a checkpoint of the file, each with its slack.
    let needed = 400_000 + file + 80 + 2 * nodecraft_format::space::EXTENT_SLACK;
    assert!(check_open_copies(&index, &context, StorageCapability::StreamingV2, &probe(needed), journal).is_ok());
    let error = check_open_copies(&index, &context, StorageCapability::StreamingV2, &probe(needed - 1), journal).unwrap_err();
    assert!(error.message.contains("open this project") && error.message.contains("current project is unchanged"), "{}", error.message);
    // Unknown space, or a host that doesn't copy, never refuses.
    assert!(check_open_copies(&index, &context, StorageCapability::StreamingV2, &SpaceProbe::default(), journal).is_ok());
    assert!(check_open_copies(&index, &context, StorageCapability::SmallV1, &probe(1), journal).is_ok());
    // Media this workspace already holds verified (the open project has them) aren't copied.
    let mut held = AssetStore::with_context(context.clone());
    for n in 0..4u8 {
        let body: Arc<[u8]> = Arc::from(vec![n; 100_000]);
        held.import_stream(&AssetRange::new(Arc::new(MemorySource::new(body)), 0, 100_000).unwrap(), &TransferCancel::default(), &StreamMetrics::default())
            .unwrap();
    }
    assert!(
        check_open_copies(&index, &context, StorageCapability::StreamingV2, &probe(file + 80 + 2 * nodecraft_format::space::EXTENT_SLACK), journal).is_ok()
    );
    // Where the replace briefly keeps two checkpoints (Windows), a smaller journal adds the
    // second copy less the journal it frees, as the save and open preflights charge it (#367).
    let slack = nodecraft_format::space::EXTENT_SLACK;
    let small = Some(1_000);
    let extra = if cfg!(windows) { file + 80 - 1_000 + slack } else { 0 };
    let fits = file + 80 + slack + extra;
    assert!(check_open_copies(&index, &context, StorageCapability::StreamingV2, &probe(fits), small).is_ok());
    let short = check_open_copies(&index, &context, StorageCapability::StreamingV2, &probe(fits - 1), small);
    assert!(short.is_err(), "the in-open check charges the journal replace too");
    drop(held);
}
