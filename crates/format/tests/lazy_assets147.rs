#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{
    AssetContext, AssetStore, NativeAssetWorkspace,
    asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel},
    disk_quota::{DiskQuota, DiskUsage},
};
use nodecraft_types::{AssetRef, Error, MediaMeta};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
struct Pattern {
    length: u64,
    value: u8,
    reads: AtomicU64,
}
impl AssetSource for Pattern {
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let n = out.len().min(usize::try_from(self.length.saturating_sub(at)).unwrap_or(usize::MAX));
        out[..n].fill(self.value);
        Ok(n)
    }
}
fn source(length: u64, value: u8) -> (Arc<Pattern>, AssetRange) {
    let source = Arc::new(Pattern { length, value, reads: AtomicU64::new(0) });
    let range = AssetRange::new(source.clone(), 0, length).unwrap();
    (source, range)
}
fn asset(hash: nodecraft_types::Blake3) -> AssetRef {
    AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }
}
fn setup(quota: DiskQuota) -> (tempfile::TempDir, NativeAssetWorkspace, AssetStore) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = NativeAssetWorkspace::new(dir.path(), quota).unwrap();
    let store = AssetStore::with_context(AssetContext::native(workspace.clone()));
    (dir, workspace, store)
}
#[test]
fn large_streams_clone_gc_and_reader_pins_do_not_materialize_asset_bodies() {
    let quota = DiskQuota::default();
    let (_dir, _workspace, mut store) = setup(quota.clone());
    let baseline = quota.usage().unwrap();
    let metrics = StreamMetrics::default();
    let length = 20 * 1024 * 1024 + 1;
    let mut hashes = Vec::new();
    for value in *b"abc" {
        let (_, range) = source(length, value);
        hashes.push(store.import_stream(&range, &TransferCancel::default(), &metrics).unwrap());
    }
    assert_eq!(store.total_logical_bytes(), length * 3);
    assert!(store.total_logical_bytes() > 48 * 1024 * 1024);
    #[cfg(feature = "validation-metrics")]
    {
        assert_eq!(metrics.stats().peak_transfer_bytes, 64 * 1024);
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
    }
    let clone = store.clone();
    let lease = store.open_verified(&asset(hashes[0]), &TransferCancel::default(), &metrics).unwrap();
    assert!(lease.read_small(16 * 1024 * 1024, &metrics).is_err());
    let mut reader = lease.reader(metrics.clone());
    reader.seek(SeekFrom::End(-4)).unwrap();
    let mut tail = [0; 4];
    reader.read_exact(&mut tail).unwrap();
    assert_eq!(tail, [b'a'; 4]);
    drop(lease);
    store.retain(&Default::default());
    store.collect_garbage().unwrap();
    assert_eq!(store.context().retained_usage().unwrap(), (3, length * 3));
    drop(clone);
    store.collect_garbage().unwrap();
    assert_eq!(store.context().retained_usage().unwrap(), (1, length));
    assert_eq!(quota.usage().unwrap().verified, length);
    drop(reader);
    store.collect_garbage().unwrap();
    assert_eq!(store.context().retained_usage().unwrap(), (0, 0));
    assert_eq!(quota.usage().unwrap(), baseline);
}
#[test]
fn more_than_32_assets_pack_without_changing_existing_reader_content() {
    let quota = DiskQuota::default();
    let (_dir, _workspace, mut store) = setup(quota.clone());
    let baseline = quota.usage().unwrap();
    let mut first = None;
    let mut hashes = Vec::new();
    for value in 1..=40 {
        let (_, range) = source(4096, value + 64);
        let hash = store.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        hashes.push(hash);
        if value == 1 {
            first = Some(store.open_verified(&asset(hash), &TransferCancel::default(), &StreamMetrics::default()).unwrap());
        }
    }
    assert_eq!(store.len(), 40);
    assert!(quota.backing_count().unwrap() < 32);
    let lease = first.unwrap();
    assert_eq!(lease.read_small(4096, &StreamMetrics::default()).unwrap(), vec![65; 4096]);
    for hash in hashes {
        assert_eq!(store.descriptor(&hash).unwrap().bytes, 4096);
    }
    store.retain(&Default::default());
    store.collect_garbage().unwrap();
    assert_eq!(store.context().retained_usage().unwrap(), (1, 4096));
    assert!(quota.usage().unwrap().verified > 0);
    drop(lease);
    store.collect_garbage().unwrap();
    assert_eq!(quota.usage().unwrap(), baseline);
    assert_eq!(quota.backing_count().unwrap(), 0);
}
#[test]
fn exhausted_pinned_backings_refuse_before_relocation_or_incoming_reads() {
    let quota = DiskQuota::default();
    let (_dir, _workspace, mut store) = setup(quota.clone());
    let mut leases = Vec::new();
    for value in 1..=30 {
        let (_, range) = source(4096, value + 64);
        let hash = store.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        leases.push(store.open_verified(&asset(hash), &TransferCancel::default(), &StreamMetrics::default()).unwrap());
    }
    let before = quota.usage().unwrap();
    let (input, range) = source(4096, 100);
    assert!(store.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(input.reads.load(Ordering::Relaxed), 0);
    assert_eq!(quota.backing_count().unwrap(), 30);
    assert_eq!(quota.usage().unwrap(), before);
    assert_eq!(store.len(), 30);
    for (n, lease) in leases.iter().enumerate() {
        assert_eq!(lease.read_small(4096, &StreamMetrics::default()).unwrap(), vec![(n + 65) as u8; 4096]);
    }
    drop(leases);
    store.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(store.len(), 31);
    assert!(quota.backing_count().unwrap() < 30);
}
#[test]
fn legacy_capability_quota_and_cancellation_refuse_without_incoming_reads() {
    let (input, range) = source(20 * 1024 * 1024, 77);
    let mut legacy = AssetStore::default();
    assert!(legacy.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(input.reads.load(Ordering::Relaxed), 0);
    let limits = DiskUsage { verified: 4096, bases: 4096, journals: 8192, staging: 4096, metadata: 1024 };
    let quota = DiskQuota::lower(limits, 32 * 1024).unwrap();
    let (_dir, _workspace, mut store) = setup(quota.clone());
    let (_, range) = source(4096, 1);
    store.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let (input, range) = source(4096, 2);
    let before = quota.usage().unwrap();
    assert!(store.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    assert_eq!(input.reads.load(Ordering::Relaxed), 0);
    assert_eq!(quota.usage().unwrap(), before);
    let cancel = TransferCancel::default();
    cancel.cancel();
    assert!(store.import_stream(&range, &cancel, &StreamMetrics::default()).is_err());
    assert_eq!(input.reads.load(Ordering::Relaxed), 0);
    assert_eq!(store.len(), 1);
}
