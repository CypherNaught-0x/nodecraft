use super::*;
use crate::native_workspace::test_uncontended as uncontended;
use crate::{ArchivePlan, AssetStore, CheckpointPlan, NativeStorage, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, ProjectId};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct ChildOwner {
    child: Child,
    reaped: bool,
    stderr: PathBuf,
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl ChildOwner {
    fn spawn(parent: &Path, pause: Option<&str>, sweep: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        let stderr = parent.join(format!("child-stderr-{}.log", pause.unwrap_or("crash")));
        command
            .args(["--exact", "native_assets::orphan_regressions::workspace363_child", "--nocapture"])
            .env("NODECRAFT_TEST_WORKSPACE363_ROOT", parent)
            .stdout(Stdio::null())
            .stderr(Stdio::from(File::create(&stderr).unwrap()));
        if let Some(point) = pause {
            command.env("NODECRAFT_TEST_WORKSPACE363_PAUSE", point);
        }
        if let Some(name) = sweep {
            command.env("NODECRAFT_TEST_WORKSPACE363_SWEEP", name);
        }
        Self { child: command.spawn().unwrap(), reaped: false, stderr }
    }
    fn log(&self) -> String {
        let mut text = String::new();
        File::open(&self.stderr).unwrap().take(16 * 1024).read_to_string(&mut text).unwrap();
        text
    }
    fn wait(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                assert!(status.success(), "isolated workspace child failed: {status}: {}", self.log());
                return;
            }
            assert!(Instant::now() < deadline, "isolated workspace child exceeded deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn ready(&mut self, parent: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                panic!("child exited before handshake: {status}: {}", self.log());
            }
            if let Ok(name) = std::fs::read_to_string(parent.join("pause-ready")) {
                assert!(name.len() < 128);
                return name;
            }
            assert!(Instant::now() < deadline, "isolated workspace handshake exceeded deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn exit(&mut self, parent: &Path) {
        std::fs::write(parent.join("pause-exit"), b"exit").unwrap();
        self.wait();
    }
}
fn child(parent: &Path) {
    ChildOwner::spawn(parent, None, None).wait();
}
#[test]
fn workspace363_child() {
    let Some(parent) = std::env::var_os("NODECRAFT_TEST_WORKSPACE363_ROOT") else { return };
    let parent = PathBuf::from(parent);
    assert_eq!(std::fs::read(parent.join("fixture-owner")).unwrap(), b"public isolated workspace363 fixture");
    if let Ok(name) = std::env::var("NODECRAFT_TEST_WORKSPACE363_SWEEP") {
        uncontended(|| crate::native_workspace::sweep_named_orphan(&parent, &name, &TransferCancel::default())).unwrap();
        return;
    }
    let mut storage = uncontended(|| NativeStorage::new(parent.join("recovery"))).unwrap();
    let context = storage.asset_context().unwrap();
    let workspace = context.0.workspace.as_ref().unwrap().clone();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut project = Project::new(ProjectId::from_bytes([61; 16]), "Public orphan fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([62; 16]), "Canvas") }.apply(&mut project).unwrap();
    let mut snapshot = Snapshot { project, assets: AssetStore::with_context(context), epoch: 1 };
    let plan = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(plan).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    storage.save_plan(&StorageLocation::Path(parent.join("destination.ncraft")), checkpoint.archive(), &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    let frame_bytes = b"frame body bytes";
    let frame_source =
        AssetRange::new(Arc::new(crate::asset_source::MemorySource::new(Arc::from(frame_bytes.as_slice()))), 0, frame_bytes.len() as u64).unwrap();
    let frame_hash = snapshot.assets.import_stream(&frame_source, &cancel, &metrics).unwrap();
    let reference = nodecraft_types::AssetRef { hash: frame_hash, mime: "text/plain".parse().unwrap(), meta: Default::default() };
    let mut node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([63; 16]), "source.text");
    node.params.insert("content".into(), nodecraft_types::Value::Media(reference.clone()));
    let change = Change::AddNode { canvas: CanvasId::from_bytes([62; 16]), node };
    change.apply(&mut snapshot.project).unwrap();
    let recorded = crate::RecordedAssets::new(checkpoint.archive().asset_lengths()).unwrap();
    let replay = crate::journal::AdmittedReplay::new(nodecraft_graph::Changeset::new(vec![change]), &snapshot.assets).unwrap();
    let (frame, _) = replay.into_plan(checkpoint.identity(), 1, &recorded, storage.capability(), &cancel, &metrics).unwrap();
    storage.append_plan(&frame, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    // Preserve the already published valid small archive; the current post-frame store is lazy.
    let original = std::fs::read(parent.join("destination.ncraft")).unwrap();
    assert!(original.len() < 64 * 1024);
    std::fs::write(parent.join("source.ncraft"), original).unwrap();
    let base = workspace.snapshot_file(File::open(parent.join("source.ncraft")).unwrap(), &cancel, &metrics).unwrap();
    let memory = AssetRange::new(Arc::new(crate::asset_source::MemorySource::new(Arc::from(b"abc".as_slice()))), 0, 3).unwrap();
    let imported = workspace.seal_copy(&memory, DiskPool::Verified, None, &cancel, &metrics).unwrap();
    let operation = workspace.quota().stage_operation().unwrap();
    let mut capture = workspace.capture(&operation).unwrap();
    capture.reserve_asset(3).unwrap();
    capture.write_bytes(b"xyz").unwrap();
    let names: Vec<String> = std::fs::read_dir(&workspace.0.root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert!(names.len() <= 8);
    assert!(
        names.iter().any(|n| n.starts_with("stage-")) && names.iter().any(|n| n.starts_with("archive-")) && names.iter().any(|n| n.starts_with("capture-"))
    );
    let report = serde_json::json!({"root":workspace.0.root.file_name().unwrap().to_str().unwrap(),"files":names,"project":snapshot.project,"asset":reference});
    std::fs::write(parent.join("child-report.json"), serde_json::to_vec(&report).unwrap()).unwrap();
    crate::native_workspace::pause("live-workspace", &parent.canonicalize().unwrap(), Some(&workspace.0.root));
    std::hint::black_box((&base, &imported, &capture));
    // Deliberate process exit: none of the live Rust workspace/source/capture destructors run.
    std::process::exit(0);
}
#[test]
fn crashed_workspace_payloads_are_reclaimed_before_next_creation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fixture-owner"), b"public isolated workspace363 fixture").unwrap();
    child(dir.path());
    let report_bytes = std::fs::read(dir.path().join("child-report.json")).unwrap();
    assert!(report_bytes.len() < 4096);
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    let name = report["root"].as_str().unwrap();
    assert!(name.starts_with("nodecraft-assets-") && !name.contains(['/', '\\']));
    let root = dir.path().join(name);
    assert!(root.is_dir());
    let preserved: Vec<_> =
        ["source.ncraft", "destination.ncraft", "recovery", "fixture-owner"].into_iter().map(|n| (n, std::fs::read(dir.path().join(n)).unwrap())).collect();
    let next = uncontended(|| NativeStorage::new(dir.path().join("next-recovery"))).unwrap();
    for (name, bytes) in preserved {
        assert_eq!(std::fs::read(dir.path().join(name)).unwrap(), bytes);
    }
    eprintln!("actual isolated crash residue: {report}");
    assert!(!root.exists(), "the next native constructor must reclaim a proven orphan's disposable payloads");
    drop(next);
    verify_recovery(dir.path(), &report);
}

#[test]
fn retained_capture_closes_payload_before_the_final_workspace_lifetime_guard() {
    let parent = tempfile::tempdir().unwrap();
    let workspace = uncontended(|| NativeAssetWorkspace::new(parent.path(), DiskQuota::default())).unwrap();
    let root = workspace.0.root.clone();
    let operation = workspace.quota().stage_operation().unwrap();
    let mut capture = workspace.capture(&operation).unwrap();
    capture.reserve_asset(3).unwrap();
    capture.write_bytes(b"abc").unwrap();
    drop(operation);
    drop(workspace);
    let report =
        uncontended(|| crate::native_workspace::sweep_named_orphan(parent.path(), root.file_name().unwrap().to_str().unwrap(), &TransferCancel::default()))
            .unwrap();
    assert_eq!(report.live, 1);
    assert_eq!(report.removed_files, 0);
    drop(capture);
    assert!(!root.exists(), "final capture must close File/permit before the last Arc retires the empty marker/root");
}
#[test]
fn retained_range_remains_readable_and_locked_after_workspace_handle_drop() {
    let parent = tempfile::tempdir().unwrap();
    let workspace = uncontended(|| NativeAssetWorkspace::new(parent.path(), DiskQuota::default())).unwrap();
    let root = workspace.0.root.clone();
    let input = AssetRange::new(Arc::new(crate::asset_source::MemorySource::new(Arc::from(b"public pinned bytes".as_slice()))), 0, 19).unwrap();
    let sealed = workspace.seal_copy(&input, DiskPool::Verified, None, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let range = sealed.range().unwrap();
    drop((sealed, workspace));
    let report =
        uncontended(|| crate::native_workspace::sweep_named_orphan(parent.path(), root.file_name().unwrap().to_str().unwrap(), &TransferCancel::default()))
            .unwrap();
    assert_eq!(report.live, 1);
    assert_eq!(range.read_small(1024, &StreamMetrics::default()).unwrap(), b"public pinned bytes");
    drop(range);
    assert!(!root.exists());
}

fn verify_recovery(parent: &Path, report: &serde_json::Value) {
    let storage = uncontended(|| NativeStorage::new(parent.join("recovery"))).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.applied, 1);
    assert!(recovered.damage.is_none());
    assert_eq!(serde_json::to_value(&recovered.snapshot.project).unwrap(), report["project"]);
    let reference: nodecraft_types::AssetRef = serde_json::from_value(report["asset"].clone()).unwrap();
    let lease = recovered.snapshot.assets.open_verified(&reference, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(lease.read_small(1024, &StreamMetrics::default()).unwrap(), b"frame body bytes");
}
fn isolated_parent() -> tempfile::TempDir {
    let parent = tempfile::tempdir().unwrap();
    std::fs::write(parent.path().join("fixture-owner"), b"public isolated workspace363 fixture").unwrap();
    parent
}
#[test]
fn creation_windows_are_guarded_and_crashes_before_marker_never_publish_payloads() {
    for point in ["after-root", "after-marker"] {
        let parent = isolated_parent();
        let mut child = ChildOwner::spawn(parent.path(), Some(point), None);
        let name = child.ready(parent.path());
        let root = parent.path().join(&name);
        assert!(matches!(NativeAssetWorkspace::new(parent.path(), DiskQuota::default()), Err(Error { kind: ErrorKind::Busy, .. })));
        if point == "after-root" {
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        } else {
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        }
        child.exit(parent.path());
        let next = uncontended(|| NativeAssetWorkspace::new(parent.path(), DiskQuota::default())).unwrap();
        if point == "after-root" {
            assert!(root.is_dir());
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        } else {
            assert!(!root.exists());
            assert_eq!(next.maintenance_report().removed_roots, 1);
        }
    }
}
#[test]
fn crashing_immediately_after_gate_create_reuses_the_same_content_agnostic_file() {
    for bytes in [b"".as_slice(), b"arbitrary partial proposed magic".as_slice()] {
        let parent = isolated_parent();
        let mut child = ChildOwner::spawn(parent.path(), Some("after-gate-open"), None);
        assert_eq!(child.ready(parent.path()), "");
        let gate = parent.path().join(".nodecraft-assets-v1.lock");
        let identity = same_file::Handle::from_path(&gate).unwrap();
        std::fs::write(&gate, bytes).unwrap();
        child.exit(parent.path());
        let workspace = uncontended(|| NativeAssetWorkspace::new(parent.path(), DiskQuota::default())).unwrap();
        assert_eq!(std::fs::read(&gate).unwrap(), bytes);
        assert_eq!(same_file::Handle::from_path(&gate).unwrap(), identity);
        drop(workspace);
    }
}
#[test]
fn live_cross_process_readers_are_skipped_then_the_crash_orphan_recovers_without_private_paths() {
    let parent = isolated_parent();
    let mut child = ChildOwner::spawn(parent.path(), Some("live-workspace"), None);
    let name = child.ready(parent.path());
    let root = parent.path().join(&name);
    let next = uncontended(|| NativeStorage::new(parent.path().join("other-recovery"))).unwrap();
    let report = next.workspace_maintenance_report().unwrap();
    assert_eq!(report.live, 1);
    assert!(root.join("workspace.v1.lock").exists());
    assert!(std::fs::read_dir(&root).unwrap().count() > 1);
    drop(next);
    child.exit(parent.path());
    let report = uncontended(|| crate::native_workspace::sweep_named_orphan(parent.path(), &name, &TransferCancel::default())).unwrap();
    assert_eq!(report.removed_roots, 1);
    assert!(!root.exists());
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(parent.path().join("child-report.json")).unwrap()).unwrap();
    verify_recovery(parent.path(), &report);
}
#[test]
fn interrupted_sweep_retains_admission_across_partial_unlink_and_marker_close() {
    for point in ["after-unlink", "after-marker-close"] {
        let parent = isolated_parent();
        child(parent.path());
        let report: serde_json::Value = serde_json::from_slice(&std::fs::read(parent.path().join("child-report.json")).unwrap()).unwrap();
        let name = report["root"].as_str().unwrap();
        let root = parent.path().join(name);
        let mut sweeper = ChildOwner::spawn(parent.path(), Some(point), Some(name));
        assert_eq!(sweeper.ready(parent.path()), name);
        assert!(matches!(
            crate::native_workspace::sweep_named_orphan(parent.path(), name, &TransferCancel::default()),
            Err(Error { kind: ErrorKind::Busy, .. })
        ));
        assert!(root.join("workspace.v1.lock").exists());
        sweeper.exit(parent.path());
        let next = uncontended(|| crate::native_workspace::sweep_named_orphan(parent.path(), name, &TransferCancel::default())).unwrap();
        assert_eq!(next.removed_roots, 1);
        assert!(!root.exists());
        verify_recovery(parent.path(), &report);
    }
}
#[test]
fn interrupted_after_marker_unlink_leaves_empty_unmarked_residue_without_adopting_it() {
    let parent = isolated_parent();
    child(parent.path());
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(parent.path().join("child-report.json")).unwrap()).unwrap();
    let name = report["root"].as_str().unwrap();
    let root = parent.path().join(name);
    let mut sweeper = ChildOwner::spawn(parent.path(), Some("after-marker-unlink"), Some(name));
    sweeper.ready(parent.path());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    sweeper.exit(parent.path());
    let report = uncontended(|| crate::native_workspace::sweep_named_orphan(parent.path(), name, &TransferCancel::default())).unwrap();
    assert_eq!(report.removed_roots, 0);
    assert!(root.is_dir());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn live_cleanup_debt_keeps_its_charge_and_cannot_be_swept_by_another_workspace() {
    let parent = tempfile::tempdir().unwrap();
    let mut workspace = uncontended(|| NativeAssetWorkspace::new(parent.path(), DiskQuota::default())).unwrap();
    workspace.test_cleanup_capacity(1).unwrap();
    let root = workspace.0.root.clone();
    let source = AssetRange::new(Arc::new(crate::asset_source::MemorySource::new(Arc::from(b"abc".as_slice()))), 0, 3).unwrap();
    let sealed = workspace.seal_copy(&source, DiskPool::Verified, None, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    workspace.fail_removals(2);
    drop(sealed);
    assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 3);
    let usage = workspace.quota().usage().unwrap();
    let other = uncontended(|| NativeAssetWorkspace::new(parent.path(), DiskQuota::default())).unwrap();
    assert_eq!(other.maintenance_report().live, 1);
    assert_eq!(workspace.quota().usage().unwrap(), usage);
    assert!(workspace.cleanup().is_err());
    workspace.cleanup().unwrap();
    assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 0);
    drop(workspace);
    // Drop retires an empty root synchronously, but its nonblocking parent admission
    // can be Busy. Retry the same exact-name maintenance used by the app instead of
    // assuming that a contended Drop removed the root.
    let deadline = Instant::now() + Duration::from_secs(30);
    while root.exists() {
        let name = root.file_name().unwrap().to_str().unwrap();
        match crate::native_workspace::sweep_named_orphan(parent.path(), name, &TransferCancel::default()) {
            Ok(_) | Err(Error { kind: ErrorKind::Busy, .. }) => {}
            Err(error) => panic!("could not sweep retired workspace root {root:?}: {error}"),
        }
        assert!(Instant::now() < deadline, "retired workspace root {root:?} still exists after 30 seconds of exact-name maintenance");
        if root.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(!root.exists(), "retired workspace root must be removed after owner cleanup");
    drop(other);
}
