#![cfg(not(target_arch = "wasm32"))]
//! Public native publication/lifecycle evidence. Streamed recovery is covered separately.
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, CheckpointPlan, NativeAssetWorkspace, NativeStorage, ProjectStorage, RecordedAssets, Snapshot, StorageCapability,
    asset_source::{StreamMetrics, TransferCancel},
    disk_quota::DiskQuota,
    journal::{self, AdmittedReplay},
};
use nodecraft_graph::{Change, Changeset, Project};
use nodecraft_types::{AssetRef, Blake3, MediaMeta, ProjectId};
use std::io::Read;
fn small_snapshot(assets: AssetStore) -> Snapshot {
    Snapshot { project: Project::new(ProjectId::from_bytes([1; 16]), "Native ownership").unwrap(), assets, epoch: 1 }
}
fn title() -> Changeset {
    Changeset::new(vec![Change::SetProjectTitle { title: "Updated".into() }])
}
#[test]
fn checkpoint_plan_reserves_before_publication_binds_host_and_clones_one_claim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let mut other = NativeStorage::new(dir.path().join("other-recovery")).unwrap();
    let context = storage.asset_context().unwrap();
    let snapshot = small_snapshot(AssetStore::with_context(context.clone()));
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let before = context.storage_usage().unwrap();
    let plan = CheckpointPlan::new(archive).unwrap();
    assert_eq!(context.storage_usage().unwrap().journals, before.journals + plan.byte_len());
    assert!(!path.exists(), "reservation must precede any recovery output");
    assert!(other.checkpoint_plan(&plan, &cancel, &metrics).is_err());
    assert!(!dir.path().join("other-recovery").exists());
    let cancelled = TransferCancel::default();
    cancelled.cancel();
    assert!(storage.checkpoint_plan(&plan, &cancelled, &metrics).is_err());
    assert!(!path.exists());
    let clone = plan.clone();
    storage.checkpoint_plan(&clone, &cancel, &metrics).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut expected = Vec::new();
    plan.write_to(&mut expected, &cancel, &metrics).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(&bytes[..8], b"NCRJ0004");
    assert!(storage.checkpoint_plan(&plan, &cancel, &metrics).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
#[test]
fn standalone_native_export_context_cannot_fabricate_a_recovery_owner() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
    let snapshot = small_snapshot(AssetStore::with_context(AssetContext::native(workspace.clone())));
    let archive = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let before = workspace.quota().usage().unwrap();
    assert!(CheckpointPlan::new(archive).is_err());
    assert_eq!(workspace.quota().usage().unwrap().journals, before.journals);
}
#[test]
fn native_frames_bind_checkpoint_sequence_and_complete_recorded_ledger_before_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let mut assets = AssetStore::with_context(storage.asset_context().unwrap());
    let hash = assets.insert(b"retained but unreferenced").unwrap();
    let snapshot = small_snapshot(assets);
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let before = std::fs::read(&path).unwrap();
    let recorded = RecordedAssets::new(checkpoint.asset_lengths()).unwrap();
    for (identity, sequence, prior) in [
        (Blake3::from_bytes([9; 32]), 1, recorded.clone()),
        (checkpoint.identity(), 2, recorded.clone()),
        (checkpoint.identity(), 1, RecordedAssets::new(&std::collections::BTreeMap::from([(hash, 25)])).unwrap()),
    ] {
        let (plan, _) =
            AdmittedReplay::new(title(), &snapshot.assets).unwrap().into_plan(identity, sequence, &prior, storage.capability(), &cancel, &metrics).unwrap();
        assert!(storage.append_plan(&plan, &cancel, &metrics).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let (plan, _) = AdmittedReplay::new(title(), &snapshot.assets)
        .unwrap()
        .into_plan(checkpoint.identity(), 1, &recorded, storage.capability(), &cancel, &metrics)
        .unwrap();
    storage.append_plan(&plan, &cancel, &metrics).unwrap();
    let appended = std::fs::read(&path).unwrap();
    let mut expected = before;
    plan.write_to(&mut expected, &cancel, &metrics).unwrap();
    assert_eq!(appended, expected);
    assert!(storage.append_plan(&plan, &cancel, &metrics).is_err());
    assert!(storage.append(b"raw legacy frame").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), appended);
    storage.flush().unwrap();
}
#[test]
fn dirty_drop_same_context_reopen_keeps_actual_lock_and_one_generation_charge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let context = storage.asset_context().unwrap();
    let checkpoint = journal::checkpoint(&small_snapshot(AssetStore::default())).unwrap();
    storage.checkpoint(&checkpoint.bytes).unwrap();
    let charge = context.storage_usage().unwrap().journals;
    drop(storage);
    assert!(NativeStorage::new(path.clone()).is_err());
    assert_eq!(context.storage_usage().unwrap().journals, charge);
    let reopened = NativeStorage::reopen(context.clone()).unwrap();
    assert_eq!(reopened.asset_context().unwrap().storage_usage().unwrap().journals, charge);
    assert_eq!(reopened.recover().unwrap().snapshot.project.title(), "Native ownership");
    assert!(NativeStorage::reopen(context.clone()).is_err(), "an active owner cannot be claimed twice");
    drop(reopened);
    drop(context);
    let reopened = NativeStorage::new(path).unwrap();
    assert_eq!(reopened.recover().unwrap().snapshot.project.title(), "Native ownership");
}
#[test]
fn a_private_asset_reader_alone_keeps_dirty_recovery_lock_until_final_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let mut assets = AssetStore::with_context(storage.asset_context().unwrap());
    let hash = assets.insert(b"pinned private reader").unwrap();
    let asset = AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() };
    let verified = assets.open_verified(&asset, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let mut reader = verified.reader(StreamMetrics::default());
    storage.checkpoint(&journal::checkpoint(&small_snapshot(AssetStore::default())).unwrap().bytes).unwrap();
    drop(verified);
    drop(assets);
    drop(storage);
    assert!(NativeStorage::new(path.clone()).is_err());
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"pinned private reader");
    drop(reader);
    assert!(NativeStorage::new(path).is_ok());
}
#[test]
fn failed_reopen_association_preserves_closed_owner_charge_and_can_retry_exact_restoration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let context = storage.asset_context().unwrap();
    storage.checkpoint(&journal::checkpoint(&small_snapshot(AssetStore::default())).unwrap().bytes).unwrap();
    let before = context.storage_usage().unwrap();
    drop(storage);
    let retained = dir.path().join("retained");
    std::fs::rename(&path, &retained).unwrap();
    std::fs::write(&path, b"substitute").unwrap();
    assert!(NativeStorage::reopen(context.clone()).is_err());
    assert_eq!(context.storage_usage().unwrap(), before);
    assert!(NativeStorage::new(path.clone()).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&retained, &path).unwrap();
    let reopened = NativeStorage::reopen(context).unwrap();
    assert_eq!(reopened.recover().unwrap().snapshot.project.title(), "Native ownership");
}

#[test]
fn cancelled_read_only_recovery_keeps_an_already_admitted_journal_appendable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let snapshot = small_snapshot(AssetStore::with_context(storage.asset_context().unwrap()));
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let before = std::fs::read(&path).unwrap();
    let cancelled = TransferCancel::default();
    cancelled.cancel();
    assert!(storage.recover_with(&cancelled, &metrics).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let (plan, _) = AdmittedReplay::new(title(), &snapshot.assets)
        .unwrap()
        .into_plan(checkpoint.identity(), 1, &RecordedAssets::new(checkpoint.asset_lengths()).unwrap(), storage.capability(), &cancel, &metrics)
        .unwrap();
    storage.append_plan(&plan, &cancel, &metrics).expect("read-only cancellation must not invalidate an admitted generation");
    assert_eq!(storage.recover().unwrap().snapshot.project.title(), "Updated");
}

#[test]
fn foreign_frame_context_refusal_does_not_latch_the_active_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let snapshot = small_snapshot(AssetStore::with_context(storage.asset_context().unwrap()));
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let recorded = RecordedAssets::new(checkpoint.asset_lengths()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let other = NativeStorage::new(dir.path().join("other")).unwrap();
    let foreign = AssetStore::with_context(other.asset_context().unwrap());
    let (rejected, _) =
        AdmittedReplay::new(title(), &foreign).unwrap().into_plan(checkpoint.identity(), 1, &recorded, storage.capability(), &cancel, &metrics).unwrap();
    let error = storage.append_plan(&rejected, &cancel, &metrics).unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Conflict);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let (accepted, _) = AdmittedReplay::new(title(), &snapshot.assets)
        .unwrap()
        .into_plan(checkpoint.identity(), 1, &recorded, storage.capability(), &cancel, &metrics)
        .unwrap();
    storage.append_plan(&accepted, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    assert_eq!(storage.recover().unwrap().snapshot.project.title(), "Updated");
}
