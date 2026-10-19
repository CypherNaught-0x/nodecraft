#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{
    AssetContext, AssetStore, NativeAssetWorkspace, RecordedAssets, StorageCapability,
    asset_source::{StreamMetrics, TransferCancel},
    disk_quota::DiskQuota,
    journal::AdmittedReplay,
};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{AssetRef, Blake3, CanvasId, MediaMeta, NodeId, Value};
use std::collections::BTreeMap;
fn hash(n: u64) -> Blake3 {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    Blake3::from_bytes(bytes)
}
fn changes(hash: Blake3) -> Changeset {
    Changeset::new(vec![Change::SetParam {
        canvas: CanvasId::from_bytes([1; 16]),
        id: NodeId::from_bytes([2; 16]),
        name: "content".into(),
        value: Some(Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })),
    }])
}
#[test]
fn recorded_identity_lengths_bound_checkpoint_plus_tail_independent_of_live_gc() {
    let mut old = BTreeMap::new();
    for n in 0..1024 {
        old.insert(hash(n), 1);
    }
    let recorded = RecordedAssets::new(&old).unwrap();
    assert_eq!(recorded.len(), 1024);
    assert!(recorded.admit(&BTreeMap::from([(hash(2000), 0)])).is_err());
    assert!(recorded.admit(&BTreeMap::from([(hash(1), 2)])).is_err());
    assert_eq!(recorded.lengths(), &old);
    let big = BTreeMap::from_iter((0..8).map(|n| (hash(n), 8 * 1024 * 1024 * 1024)));
    let full = RecordedAssets::new(&big).unwrap();
    assert_eq!(full.byte_len(), 64 * 1024 * 1024 * 1024);
    assert!(full.admit(&BTreeMap::from([(hash(9), 1)])).is_err());
    let dir = tempfile::tempdir().unwrap();
    let ws = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let mut assets = AssetStore::with_context(AssetContext::native(ws.clone()));
    let actual = assets.insert(b"new").unwrap();
    let metrics = StreamMetrics::default();
    let before = ws.quota().usage().unwrap();
    assert!(
        AdmittedReplay::new(changes(actual), &assets)
            .unwrap()
            .into_plan(hash(5), 1, &recorded, StorageCapability::StreamingV2, &TransferCancel::default(), &metrics)
            .is_err()
    );
    assert_eq!(ws.quota().usage().unwrap(), before);
    #[cfg(feature = "validation-metrics")]
    assert_eq!(metrics.stats().source_bytes, 0);
    // Models the new checkpoint's reset: the old recovery ledger remains unchanged,
    // while a separately admitted fresh ledger permits re-emission of a retained hash.
    let (plan, next) = AdmittedReplay::new(changes(actual), &assets)
        .unwrap()
        .into_plan(hash(6), 1, &RecordedAssets::default(), StorageCapability::StreamingV2, &TransferCancel::default(), &metrics)
        .unwrap();
    assert_eq!(next.lengths().get(&actual), Some(&3));
    assert_eq!(recorded.lengths(), &old);
    let mut bytes = Vec::new();
    plan.write_to(&mut bytes, &TransferCancel::default(), &metrics).unwrap();
    assert_eq!(bytes.len() as u64, plan.byte_len());
}
#[test]
fn streaming_frame_exact_wire_hash_and_duplicate_references_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let ws = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let mut assets = AssetStore::with_context(AssetContext::native(ws));
    let actual = assets.insert(b"retained").unwrap();
    let identity = hash(100);
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let (plan, recorded) = AdmittedReplay::new(changes(actual), &assets)
        .unwrap()
        .into_plan(identity, 1, &RecordedAssets::default(), StorageCapability::StreamingV2, &cancel, &metrics)
        .unwrap();
    assert!(plan.small_bytes().is_err());
    let mut bytes = Vec::new();
    plan.write_to(&mut bytes, &cancel, &metrics).unwrap();
    let length = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    assert_eq!(length + 40, plan.byte_len());
    assert_eq!(&bytes[8..40], blake3::hash(&bytes[40..]).as_bytes());
    let n = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
    let record: serde_json::Value = serde_json::from_slice(&bytes[48..48 + n]).unwrap();
    assert_eq!(record["sequence"], 1);
    assert_eq!(record["checkpoint"], identity.to_string());
    assert_eq!(u64::from_le_bytes(bytes[48 + n..56 + n].try_into().unwrap()), 1);
    assert_eq!(&bytes[56 + n..88 + n], actual.as_bytes());
    assert_eq!(u64::from_le_bytes(bytes[88 + n..96 + n].try_into().unwrap()), 8);
    assert_eq!(&bytes[96 + n..], b"retained");
    let (second, same) =
        AdmittedReplay::new(changes(actual), &assets).unwrap().into_plan(identity, 2, &recorded, StorageCapability::StreamingV2, &cancel, &metrics).unwrap();
    let mut bytes = Vec::new();
    second.write_to(&mut bytes, &cancel, &metrics).unwrap();
    let n = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
    assert_eq!(u64::from_le_bytes(bytes[48 + n..56 + n].try_into().unwrap()), 0);
    assert_eq!(bytes.len(), 56 + n);
    assert_eq!(same, recorded);
    let cancelled = TransferCancel::default();
    cancelled.cancel();
    let mut untouched = Vec::new();
    assert!(plan.write_to(&mut untouched, &cancelled, &metrics).is_err());
    assert!(untouched.is_empty());
}
#[test]
fn small_frame_retains_exact_legacy_bytes() {
    let mut store = AssetStore::default();
    let h = store.insert(b"legacy").unwrap();
    let c = changes(h);
    let id = hash(77);
    let old = nodecraft_format::journal::frame(id, 3, &c, &store, &std::collections::BTreeSet::from([h])).unwrap();
    let (plan, _) = AdmittedReplay::new(c, &store)
        .unwrap()
        .into_plan(id, 3, &RecordedAssets::default(), StorageCapability::SmallV1, &TransferCancel::default(), &StreamMetrics::default())
        .unwrap();
    assert_eq!(plan.small_bytes().unwrap(), old);
}
