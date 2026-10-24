use super::*;
use test_faults::{Guard, Point};

#[test]
fn startup445_qualified_payload_refusal_keeps_partial_report_and_admits_new_workspace() {
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 445, 3);
    let fault = test_faults::Guard::arm(&root, test_faults::Point::PayloadUnlink, 1);
    let result = test_uncontended(|| crate::NativeAssetWorkspace::new(parent.path(), crate::disk_quota::DiskQuota::default()));
    println!("constructor after qualified payload refusal: {:?}", result.as_ref().map(|_| ()).map_err(|error| (&error.kind, &error.message)));
    assert!(fault.hits() > 0, "the owned deterministic refusal was reached");
    assert!(root.join(MARKER).is_file());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 3, "one successful payload removal plus retained marker/two payloads");
    let workspace = result.expect("incidental old-candidate failure must not disable a separate new workspace");
    let report = workspace.maintenance_report();
    assert_eq!(report.removed_files, 1);
    assert_eq!(report.removed_roots, 0);
    assert_eq!(report.retained_failures, 1);
    assert_eq!(report.status(), SweepStatus::RetainedFailure);
    drop(fault);
    assert_eq!(named(parent.path(), &root).unwrap().removed_roots, 1);
}

#[test]
fn marker_and_final_directory_refusals_keep_truthful_residue_and_admit_independent_roots() {
    for point in [Point::MarkerUnlink, Point::DirectoryRetire] {
        let parent = tempfile::tempdir().unwrap();
        let root = fixture(parent.path(), 1, 1);
        let fault = Guard::arm(&root, point, 0);
        let workspace = test_uncontended(|| crate::NativeAssetWorkspace::new(parent.path(), crate::disk_quota::DiskQuota::default())).unwrap();
        let report = workspace.maintenance_report();
        assert_eq!(fault.hits(), 1);
        assert_eq!(report.removed_files, 1);
        assert_eq!(report.removed_roots, 0);
        assert_eq!(report.retained_failures, 1);
        assert_eq!(report.status(), SweepStatus::RetainedFailure);
        assert!(root.is_dir());
        assert!(!root.join("stage-0").exists());
        assert_eq!(root.join(MARKER).is_file(), point == Point::MarkerUnlink);
        drop(fault);
        let retried = named(parent.path(), &root).unwrap();
        if point == Point::MarkerUnlink {
            assert_eq!(retried.removed_roots, 1);
            assert!(!root.exists());
        } else {
            assert_eq!(retried.unknown_or_legacy, 1);
            assert!(root.is_dir(), "unmarked residue is not adopted or given replacement authority");
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        }
    }
}

#[test]
fn changed_payload_after_inventory_is_retained_without_weakening_the_allowlist() {
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 2);
    let fault = Guard::arm(&root, Point::PayloadChanged, 0);
    let workspace = test_uncontended(|| crate::NativeAssetWorkspace::new(parent.path(), crate::disk_quota::DiskQuota::default())).unwrap();
    let report = workspace.maintenance_report();
    assert_eq!(report.retained_failures, 1);
    assert_eq!(report.removed_files, 0);
    assert!(root.join(MARKER).is_file());
    let changed: Vec<_> = std::fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.is_dir()).collect();
    assert_eq!(changed.len(), 1, "the real post-inventory type replacement happened");
    for n in 0..2 {
        let path = root.join(format!("stage-{n}"));
        if path.is_file() {
            assert_eq!(std::fs::read(path).unwrap(), b"public disposable fixture");
        }
    }
    drop(fault);
    assert_eq!(named(parent.path(), &root).unwrap().removed_files, 0, "nested content still refuses entire inventory");
    let path = changed.first().unwrap();
    std::fs::remove_dir(path).unwrap();
    file(path, b"public disposable fixture");
    assert_eq!(named(parent.path(), &root).unwrap().removed_roots, 1);
}

#[test]
fn automatic_pass_stops_at_first_retained_failure_and_preserves_all_attempt_budgets() {
    let parent = tempfile::tempdir().unwrap();
    let first = fixture(parent.path(), 1, 1);
    let second = fixture(parent.path(), 2, 1);
    let fault = Guard::arm(&first, Point::PayloadUnlink, 0);
    let admission = test_uncontended(|| Admission::acquire(parent.path())).unwrap();
    let consumed = std::cell::Cell::new(0);
    let names = [first.file_name().unwrap().to_os_string(), second.file_name().unwrap().to_os_string()];
    let report = scan_entries_with_policy(
        &admission,
        &TransferCancel::default(),
        names.into_iter().map(|name| {
            consumed.set(consumed.get() + 1);
            Ok(name)
        }),
        MaintenancePolicy::Constructor,
    )
    .unwrap();
    assert_eq!(consumed.get(), 1);
    assert_eq!(report.visited, 1);
    assert_eq!(report.inventory_attempts, 1);
    assert_eq!(report.removed_files, 0);
    assert_eq!(report.retained_failures, 1);
    assert!(!report.scan_window_exhausted && !report.cleanup_budget_exhausted);
    assert_eq!(std::fs::read(second.join("stage-0")).unwrap(), b"public disposable fixture");
    drop((fault, admission));
    assert_eq!(named(parent.path(), &first).unwrap().removed_roots, 1);
    assert_eq!(named(parent.path(), &second).unwrap().removed_roots, 1);
}

#[test]
fn explicit_maintenance_preserves_original_errors_at_each_failure_boundary() {
    for (point, message) in [
        (Point::PayloadUnlink, "workspace cleanup failed; check permissions and retry maintenance"),
        (Point::PayloadChanged, "workspace payload changed; maintenance retained its marker"),
        (Point::MarkerUnlink, "cannot retire workspace marker; retry maintenance"),
        (Point::DirectoryRetire, "workspace directory remains after marker retirement; inspect it manually"),
    ] {
        let parent = tempfile::tempdir().unwrap();
        let root = fixture(parent.path(), 1, 1);
        let _fault = Guard::arm(&root, point, 0);
        assert_eq!(named(parent.path(), &root).unwrap_err().message, message);
    }
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 1);
    let _fault = Guard::arm(&root, Point::PayloadUnlink, 0);
    assert_eq!(
        test_uncontended(|| sweep_orphans(parent.path(), &TransferCancel::default())).unwrap_err().message,
        "workspace cleanup failed; check permissions and retry maintenance"
    );
}

#[test]
fn automatic_policy_does_not_swallow_parent_scan_cancellation_or_new_marker_authority_errors() {
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 1);
    let admission = test_uncontended(|| Admission::acquire(parent.path())).unwrap();
    let bad = std::iter::once(Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
    assert!(scan_entries_with_policy(&admission, &TransferCancel::default(), bad, MaintenancePolicy::Constructor).is_err());
    let cancel = TransferCancel::default();
    cancel.cancel();
    let names = std::iter::once(Ok(root.file_name().unwrap().to_os_string()));
    assert!(scan_entries_with_policy(&admission, &cancel, names, MaintenancePolicy::Constructor).is_err());
    assert_eq!(std::fs::read(root.join("stage-0")).unwrap(), b"public disposable fixture");
    assert!(
        matches!(crate::NativeAssetWorkspace::new(parent.path(), crate::disk_quota::DiskQuota::default()), Err(Error { kind: ErrorKind::Busy, .. })),
        "live parent exclusion remains immediate"
    );
    drop(admission);
    let parent = tempfile::tempdir().unwrap();
    let fault = Guard::arm(parent.path(), Point::NewMarker, 0);
    assert!(crate::NativeAssetWorkspace::new(parent.path(), crate::disk_quota::DiskQuota::default()).is_err());
    assert_eq!(fault.hits(), 1);
    assert_eq!(
        std::fs::read_dir(parent.path()).unwrap().filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_dir()).count(),
        0,
        "failed new authority is fatal and its exclusively created root is retired"
    );
}

#[test]
fn unrelated_retained_failure_preserves_real_native_save_open_and_kept_journal_replay() {
    use crate::asset_source::StreamMetrics;
    use crate::{ArchivePlan, AssetStore, CheckpointPlan, NativeStorage, ProjectStorage, Snapshot, StorageLocation};
    use nodecraft_graph::{Canvas, Change, Changeset, Project};
    use nodecraft_types::{CanvasId, ProjectId};
    let parent = tempfile::tempdir().unwrap();
    let stale = fixture(parent.path(), 1, 1);
    let fault = Guard::arm(&stale, Point::PayloadUnlink, 0);
    let live = parent.path().join("session.journal");
    let mut storage = test_uncontended(|| NativeStorage::new(live.clone())).unwrap();
    assert_eq!(storage.workspace_maintenance_report().unwrap().retained_failures, 1);
    let mut project = Project::new(ProjectId::from_bytes([44; 16]), "Public saved project").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([45; 16]), "Canvas") }.apply(&mut project).unwrap();
    let assets = AssetStore::with_context(storage.asset_context().unwrap());
    let snapshot = Snapshot { project: project.clone(), assets, epoch: 0 };
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let plan = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let destination = StorageLocation::Path(parent.path().join("public.ncraft"));
    storage.save_plan(&destination, &plan, &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(plan).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let changes = Changeset::new(vec![Change::SetProjectTitle { title: "Public recovered work".into() }]);
    let admitted = crate::journal::AdmittedReplay::new(changes.clone(), &snapshot.assets).unwrap();
    let recorded = crate::RecordedAssets::new(checkpoint.asset_lengths()).unwrap();
    let (frame, _) = admitted.into_plan(checkpoint.identity(), 1, &recorded, storage.capability(), &cancel, &metrics).unwrap();
    storage.append_plan(&frame, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    drop((storage, snapshot, checkpoint, frame));
    let kept = parent.path().join("session.journal.unrecovered-1");
    std::fs::copy(&live, &kept).unwrap();
    let replay_host = test_uncontended(|| NativeStorage::new(kept.clone())).unwrap();
    assert_eq!(replay_host.workspace_maintenance_report().unwrap().retained_failures, 1);
    let recovered = replay_host.recover().unwrap();
    let mut unsaved = project.clone();
    changes.apply(&mut unsaved).unwrap();
    assert_eq!(recovered.snapshot.project, unsaved);
    let mut opener = test_uncontended(|| NativeStorage::new(parent.path().join("other.journal"))).unwrap();
    let (loaded, _, _) = opener.prepare_open_snapshot(&destination, &cancel, &metrics).unwrap().into_parts();
    assert_eq!(loaded.project, project);
    assert!(fault.hits() >= 3);
    assert_eq!(std::fs::read(stale.join("stage-0")).unwrap(), b"public disposable fixture");
    drop(fault);
    assert_eq!(named(parent.path(), &stale).unwrap().removed_roots, 1);
    assert_eq!(recovered.snapshot.project, unsaved, "cleanup of unrelated stale root cannot invalidate recovery");
}
