//! Manual physical fault proof; no public/test-feature storage hook.
use super::*;
use crate::{
    ArchivePlan, AssetStore, CheckpointPlan, Snapshot,
    asset_source::{StreamDigest, TRANSFER_BYTES},
    zip_index::ArchiveIndex,
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, CanvasId, NodeId, ProjectId, Value};
use std::time::Instant;
const LARGE: u64 = u32::MAX as u64 + 2;
const SMALL: u64 = 2 * 1024 * 1024 + 1;
struct Generated {
    left: u64,
    actual: u64,
    maximum: usize,
    hash: blake3::Hasher,
}
impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.maximum = self.maximum.max(out.len());
        let n = out.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        out[..n].fill(b'x');
        self.hash.update(&out[..n]);
        self.left -= n as u64;
        self.actual = self.actual.checked_add(n as u64).unwrap();
        Ok(n)
    }
}
fn snapshot(store: AssetStore) -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([31; 16]), "Physical atomic proof").unwrap();
    let canvas = CanvasId::from_bytes([32; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(NodeId::from_bytes([33; 16]), "test.media");
    node.params.insert(
        "assets".into(),
        Value::List(store.hashes().map(|hash| Value::Media(AssetRef { hash: *hash, mime: "text/plain".parse().unwrap(), meta: Default::default() })).collect()),
    );
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    Snapshot { project, assets: store, epoch: 1 }
}
fn digest(path: &Path, metrics: &StreamMetrics) -> StreamDigest {
    let file = File::open(path).unwrap();
    let bytes = file.metadata().unwrap().len();
    let range = crate::NativeAssetWorkspace::input_range(file, bytes).unwrap();
    crate::asset_source::copy_and_hash(&range, &mut std::io::sink(), &TransferCancel::default(), metrics).unwrap()
}
fn header(path: &Path) -> [u8; 80] {
    let mut out = [0; 80];
    File::open(path).unwrap().read_exact(&mut out).unwrap();
    out
}
fn run_fault(bytes: u64, heavy: bool) {
    assert_eq!(bytes, if heavy { LARGE } else { SMALL });
    let dir = if heavy {
        assert_eq!(std::env::var("NODECRAFT_PHYSICAL147").as_deref(), Ok("1"), "large I/O requires resource approval");
        let parent = PathBuf::from(std::env::var_os("NODECRAFT_PHYSICAL147_ROOT").expect("operator-selected existing parent"));
        assert!(parent.is_dir());
        tempfile::Builder::new().prefix("nodecraft147-atomic-").tempdir_in(parent).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let start = Instant::now();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let recovery = dir.path().join("recovery");
    let destination = dir.path().join("old-valid.ncraft");
    let mut storage = NativeStorage::new(recovery.clone()).unwrap();
    let context = storage.asset_context().unwrap();
    let workspace = context.0.workspace.as_ref().unwrap().clone();
    let mut store = AssetStore::with_context(context.clone());
    let mut generated = Generated { left: bytes, actual: 0, maximum: 0, hash: blake3::Hasher::new() };
    let hash = store.import_reader(&mut generated, &cancel, &metrics).unwrap();
    assert_eq!(generated.actual, bytes);
    assert_eq!(hash.as_bytes(), generated.hash.finalize().as_bytes());
    assert!(generated.maximum <= TRANSFER_BYTES);
    let mut old = snapshot(store);
    let old_archive = ArchivePlan::prepare(&old, StorageCapability::StreamingV2, &cancel, &metrics).unwrap();
    let a = old_archive.byte_len();
    let old_checkpoint = CheckpointPlan::new(old_archive).unwrap();
    let identity = old_checkpoint.identity();
    let j = old_checkpoint.byte_len();
    assert_eq!(j, a + 80);
    assert!(j <= bytes + 16 * 1024 * 1024);
    storage.save_plan(&StorageLocation::Path(destination.clone()), old_checkpoint.archive(), &cancel, &metrics).unwrap();
    storage.checkpoint_plan(&old_checkpoint, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    let old_destination = digest(&destination, &metrics);
    let old_recovery = digest(&recovery, &metrics);
    let old_header = header(&recovery);
    assert_eq!(old_destination, old_checkpoint.archive().digest());
    assert_eq!(crate::checkpoint_plan::read_header(&old_header).unwrap().2, identity);
    {
        let file = File::open(&destination).unwrap();
        let range = crate::NativeAssetWorkspace::input_range(file, a).unwrap();
        let index = ArchiveIndex::read(&range, &cancel, &metrics).unwrap();
        // Calibration is small-compatible; the separately approved physical path remains wire2.
        assert_eq!(index.version(), if heavy { 4 } else { 3 });
        assert!(index.entries().iter().any(|e| e.name == crate::zip_index::EntryName::Asset(hash) && e.data.byte_len() == bytes));
    }
    drop(old_checkpoint);
    old.epoch = 2;
    let new = CheckpointPlan::new(ArchivePlan::prepare(&old, StorageCapability::StreamingV2, &cancel, &metrics).unwrap()).unwrap();
    assert_ne!(new.identity(), identity);
    let before = workspace.quota().usage().unwrap();
    let backings = workspace.quota().backing_count().unwrap();
    let threshold = if heavy { u32::MAX as u64 + 1 } else { 1024 * 1024 };
    // This wrapper forwards to native_atomic's real File writer before it injects an error.
    workspace.fail_next_atomic_after(threshold).unwrap();
    assert!(storage.save_plan(&StorageLocation::Path(destination.clone()), new.archive(), &cancel, &metrics).is_err());
    let written = workspace.atomic_written();
    assert_eq!(written, threshold);
    if heavy {
        assert!(written > u32::MAX as u64);
    }
    assert_eq!(workspace.quota().usage().unwrap(), before);
    assert_eq!(workspace.quota().backing_count().unwrap(), backings);
    assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 0);
    assert_eq!(digest(&destination, &metrics), old_destination);
    assert_eq!(digest(&recovery, &metrics), old_recovery);
    assert_eq!(header(&recovery), old_header);
    {
        let recovered = storage.recover_with(&cancel, &metrics).unwrap();
        assert_eq!(recovered.snapshot.epoch, 1);
        assert_eq!(recovered.applied, 0);
        assert!(!recovered.truncated_tail);
    }
    // Calibration additionally exercises the identical hook on checkpoint publication.
    if !heavy {
        workspace.fail_next_atomic_after(threshold).unwrap();
        assert!(storage.checkpoint_plan(&new, &cancel, &metrics).is_err());
        assert_eq!(workspace.atomic_written(), threshold);
        assert_eq!(header(&recovery), old_header);
        assert_eq!(digest(&recovery, &metrics), old_recovery);
    }
    // A safely reconciled failed generation permits a fresh publication, never a fake post-rename error.
    storage.checkpoint_plan(&new, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    assert_eq!(crate::checkpoint_plan::read_header(&header(&recovery)).unwrap().2, new.identity());
    assert_eq!(digest(&destination, &metrics), old_destination, "failed Save did not replace the complete old destination");
    let usage = workspace.quota().usage().unwrap();
    let stats = metrics.stats();
    println!(
        "PHYSICAL147 atomic phase=complete heavy={heavy} bytes={bytes} archive={a} checkpoint={j} actual_forwarded_write_bytes={written} old_identity={identity} usage={usage:?} backings={} cleanup_debt={} source_stats={stats:?} elapsed_ms={}",
        workspace.quota().backing_count().unwrap(),
        workspace.quota().cleanup_retained_bytes().unwrap(),
        start.elapsed().as_millis()
    );
    assert_eq!(usage.staging, 0);
    assert!(stats.maximum_read <= TRANSFER_BYTES as u64 && stats.peak_transfer_bytes <= 2 * TRANSFER_BYTES as u64);
    workspace.cleanup().unwrap();
    // Simulate a stopped process, rather than hostile mutation of a live generation.
    drop((new, old, storage, context, workspace));
    {
        let mut file = OpenOptions::new().append(true).open(&recovery).unwrap();
        file.write_all(b"torn").unwrap();
        file.sync_all().unwrap();
    }
    let mut restarted = NativeStorage::new(recovery).unwrap();
    {
        let recovered = restarted.recover_with(&cancel, &metrics).unwrap();
        assert_eq!(recovered.snapshot.epoch, 2);
        assert_eq!(recovered.applied, 0);
        assert!(recovered.truncated_tail);
    }
    assert!(restarted.discard().is_err(), "damaged generation remains latched instead of silently discarding evidence");
    drop(restarted);
    println!("PHYSICAL147 atomic phase=restart-torn-prefix-validated heavy={heavy} final_stats={:?}", metrics.stats());
    // Test-only fixture disposal after every host/context/reader handle has ended.
    dir.close().unwrap();
}
#[test]
fn physical_zip64_atomic_small_calibration() {
    run_fault(SMALL, false);
}
#[test]
#[ignore = "manual >4GiB actual atomic-writer fault; requires independent resource approval and NODECRAFT_PHYSICAL147=1"]
fn physical_zip64_atomic_large() {
    run_fault(LARGE, true);
}
