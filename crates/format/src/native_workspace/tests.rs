use super::*;

#[test]
fn another_parent_admits_before_the_first_parent_is_released() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let held = test_uncontended(|| Admission::acquire(a.path())).unwrap();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let start = std::sync::Barrier::new(2);
    let observed = std::thread::scope(|scope| {
        let start = &start;
        let b = b.path();
        let worker = scope.spawn(move || {
            start.wait();
            let result = crate::NativeAssetWorkspace::new(b, crate::disk_quota::DiskQuota::default());
            result_tx.send(result.as_ref().map(|_| ()).map_err(|e| e.kind)).unwrap();
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            drop(result);
        });
        start.wait();
        let result = result_rx.recv_timeout(std::time::Duration::from_secs(5));
        // The observation is obtained while A's actual admission remains held.
        drop(held);
        let _ = release_tx.send(());
        worker.join().unwrap();
        result
    })
    .unwrap();
    assert_eq!(observed, Ok(()), "different-parent construction must finish before releasing A");
}

fn file(path: &Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}
fn fixture(parent: &Path, id: u64, payloads: usize) -> PathBuf {
    let name = format!("{PREFIX}123-{id}");
    let root = parent.join(&name);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root).unwrap();
    let (bytes, n) = marker_bytes(&name).unwrap();
    file(&root.join(MARKER), &bytes[..n]);
    for n in 0..payloads {
        file(&root.join(format!("stage-{n}")), b"public disposable fixture");
    }
    root
}
fn named(parent: &Path, root: &Path) -> Result<SweepReport, Error> {
    test_uncontended(|| sweep_named_orphan(parent, root.file_name().unwrap().to_str().unwrap(), &TransferCancel::default()))
}
#[test]
fn supported_names_and_fixed_inventory_cover_exact_numeric_boundaries() {
    assert!(root_name("nodecraft-assets-v1-4294967295-18446744073709551615"));
    for name in [
        "nodecraft-assets-1-1",
        "nodecraft-assets-v1-01-1",
        "nodecraft-assets-v1-1-01",
        "nodecraft-assets-v1-4294967296-1",
        "nodecraft-assets-v1-1-18446744073709551616",
        "nodecraft-assets-v1-1-1/child",
        "nodecraft-assets-v1-1-1-extra",
        "nodecraft-assets-v1-١-1",
    ] {
        assert!(!root_name(name), "{name}");
    }
    for prefix in ["stage", "archive", "capture"] {
        let name = format!("{prefix}-18446744073709551615");
        let record = Record::parse(&name).unwrap();
        assert_eq!(record.name().unwrap().text().unwrap(), name);
    }
    for name in ["stage--1", "stage-01", "stage-18446744073709551616", "spool-1", "base-1", "capture-1.extra", "archive-١"] {
        assert!(Record::parse(name).is_none());
    }
    assert!(std::mem::size_of::<Record>() <= 32);
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 1);
    let records = inventory(&root, &TransferCancel::default()).unwrap().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records.capacity() * std::mem::size_of::<Record>() <= MAX_INVENTORY_BYTES);
    let over = MAX_INVENTORY_BYTES / std::mem::size_of::<Record>() + 1;
    assert!(inventory_with_capacity(&root, &TransferCancel::default(), over).is_err());
    assert!(root.join("stage-0").exists(), "rejected actual empty backing cannot authorize deletion");
}
#[test]
fn same_parent_fresh_opens_and_canonical_alias_refuse_then_admit_after_release() {
    let parent = tempfile::tempdir().unwrap();
    let held = test_uncontended(|| Admission::acquire(parent.path())).unwrap();
    assert!(matches!(Admission::acquire(parent.path()), Err(Error { kind: ErrorKind::Busy, .. })));
    let alias = parent.path().join(".");
    assert!(matches!(Admission::acquire(&alias), Err(Error { kind: ErrorKind::Busy, .. })));
    drop(held);
    assert!(test_uncontended(|| Admission::acquire(&alias)).is_ok());
}
#[test]
fn permanent_gate_ignores_contents_and_preserves_its_file_identity() {
    for content in [b"".as_slice(), b"partial arbitrary old bytes\0\xff".as_slice()] {
        let parent = tempfile::tempdir().unwrap();
        let gate = parent.path().join(GATE);
        file(&gate, content);
        let identity = same_file::Handle::from_path(&gate).unwrap();
        let admission = test_uncontended(|| Admission::acquire(parent.path())).unwrap();
        drop(admission);
        assert_eq!(std::fs::read(&gate).unwrap(), content);
        assert_eq!(same_file::Handle::from_path(&gate).unwrap(), identity);
    }
}
#[test]
fn invalid_gate_types_refuse_without_modifying_their_targets() {
    let parent = tempfile::tempdir().unwrap();
    std::fs::create_dir(parent.path().join(GATE)).unwrap();
    assert!(Admission::acquire(parent.path()).is_err());
    assert!(parent.path().join(GATE).is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("sentinel");
        file(&target, b"untouched");
        symlink(&target, parent.path().join(GATE)).unwrap();
        assert!(Admission::acquire(parent.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        std::fs::remove_file(parent.path().join(GATE)).unwrap();
        file(&parent.path().join(GATE), b"contents");
        std::fs::set_permissions(parent.path().join(GATE), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Admission::acquire(parent.path()).is_err());
        assert_eq!(std::fs::read(parent.path().join(GATE)).unwrap(), b"contents");
    }
}
#[test]
fn unknown_entries_and_over_limit_inventories_refuse_the_whole_candidate() {
    let parent = tempfile::tempdir().unwrap();
    for (id, unknown) in ["unknown", "stage-01", "stage-18446744073709551616"].into_iter().enumerate() {
        let root = fixture(parent.path(), id as u64, 2);
        file(&root.join(unknown), b"sentinel");
        let report = named(parent.path(), &root).unwrap();
        assert_eq!(report.removed_files, 0);
        assert_eq!(report.inventory_attempts, 1);
        assert!(root.join("stage-0").exists() && root.join(unknown).exists());
    }
    let root = fixture(parent.path(), 10, 1);
    std::fs::create_dir(root.join("stage-2")).unwrap();
    assert_eq!(named(parent.path(), &root).unwrap().removed_files, 0);
    assert!(root.join("stage-0").exists());
    let root = fixture(parent.path(), 11, MAX_PAYLOADS + 1);
    assert_eq!(named(parent.path(), &root).unwrap().removed_files, 0);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), MAX_PAYLOADS + 2);
}
#[test]
fn exact_inventory_reclaims_in_bounded_batches_with_marker_retained_until_final_retirement() {
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, MAX_PAYLOADS);
    let mut total = 0;
    for pass in 0..16 {
        let report = named(parent.path(), &root).unwrap();
        assert_eq!(report.inventory_attempts, 1);
        assert_eq!(report.removed_files, MAX_UNLINKS);
        total += report.removed_files;
        if pass < 15 {
            assert!(report.cleanup_budget_exhausted);
            assert_eq!(report.removed_roots, 0);
            assert!(root.join(MARKER).is_file());
        } else {
            assert_eq!(report.removed_roots, 1);
            assert!(!root.exists());
        }
    }
    assert_eq!(total, MAX_PAYLOADS as u32);
}
#[test]
fn rejected_inventories_consume_the_attempt_budget_without_deleting_any_payload() {
    let parent = tempfile::tempdir().unwrap();
    for id in 0..65 {
        let root = fixture(parent.path(), id, 1);
        file(&root.join("unknown"), b"sentinel");
    }
    let report = test_uncontended(|| sweep_orphans(parent.path(), &TransferCancel::default())).unwrap();
    assert_eq!(report.inventory_attempts, MAX_ATTEMPTS);
    assert!(report.cleanup_budget_exhausted);
    assert_eq!(report.removed_files, 0);
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir()).count(), 65);
}
#[test]
fn legacy_partial_and_mismatched_markers_are_preserved_without_adoption() {
    let parent = tempfile::tempdir().unwrap();
    let legacy = parent.path().join("nodecraft-assets-1-2");
    std::fs::create_dir(&legacy).unwrap();
    file(&legacy.join("stage-1"), b"legacy");
    for (id, content) in
        [b"NCASSET1".as_slice(), b"NCASSET2\nwrong\n".as_slice(), b"NCASSET1\nnodecraft-assets-v1-123-999\n".as_slice()].into_iter().enumerate()
    {
        let root = fixture(parent.path(), id as u64, 1);
        std::fs::write(root.join(MARKER), content).unwrap();
        assert_eq!(named(parent.path(), &root).unwrap().removed_files, 0);
        assert!(root.join("stage-0").exists());
    }
    let report = test_uncontended(|| sweep_orphans(parent.path(), &TransferCancel::default())).unwrap();
    assert_eq!(report.removed_files, 0);
    assert_eq!(std::fs::read(legacy.join("stage-1")).unwrap(), b"legacy");
    for wrong in ["../nodecraft-assets-v1-1-1", "nodecraft-assets-1-1", "/nodecraft-assets-v1-1-1"] {
        assert!(sweep_named_orphan(parent.path(), wrong, &TransferCancel::default()).is_err());
    }
}
#[cfg(unix)]
#[test]
fn symlink_payload_and_symlink_root_preserve_external_targets() {
    use std::os::unix::fs::symlink;
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("source.ncraft");
    file(&target, b"user source sentinel");
    let root = fixture(parent.path(), 1, 1);
    symlink(&target, root.join("archive-2")).unwrap();
    assert_eq!(named(parent.path(), &root).unwrap().removed_files, 0);
    assert!(root.join("stage-0").exists());
    let alias = parent.path().join("nodecraft-assets-v1-123-2");
    symlink(&root, &alias).unwrap();
    assert_eq!(named(parent.path(), &alias).unwrap().removed_files, 0);
    assert_eq!(std::fs::read(target).unwrap(), b"user source sentinel");
}
#[test]
fn cancellation_and_borrowed_oversized_paths_refuse_before_namespace_mutation() {
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 2);
    let cancel = TransferCancel::default();
    cancel.cancel();
    assert!(sweep_named_orphan(parent.path(), root.file_name().unwrap().to_str().unwrap(), &cancel).is_err());
    assert!(root.join("stage-0").exists());
    let huge = "x".repeat(MAX_PATH_BYTES);
    assert!(Admission::acquire(Path::new(&huge)).is_err());
}

#[cfg(unix)]
#[test]
fn qualified_unlink_failure_keeps_marker_and_refuses_another_constructor_until_repaired() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 1);
    let restore = Restore(root.clone());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(crate::NativeAssetWorkspace::new(parent.path(), crate::disk_quota::DiskQuota::default()).is_err());
    assert!(root.join(MARKER).is_file() && root.join("stage-0").is_file());
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir()).count(), 1);
    drop(restore);
    assert_eq!(named(parent.path(), &root).unwrap().removed_roots, 1);
}

#[test]
fn exact_length_but_invalid_marker_bytes_never_authorize_deletion() {
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 1);
    let marker = root.join(MARKER);
    let mut bytes = std::fs::read(&marker).unwrap();
    bytes[0] = b'X';
    std::fs::write(&marker, &bytes).unwrap();
    let report = named(parent.path(), &root).unwrap();
    assert_eq!(report.removed_files, 0);
    assert_eq!(report.inventory_attempts, 0);
    assert!(root.join("stage-0").exists());
    assert_eq!(std::fs::read(marker).unwrap(), bytes);
}
#[test]
fn live_prefix_uses_scan_work_but_does_not_spend_inventory_attempts() {
    let parent = tempfile::tempdir().unwrap();
    let admission = test_uncontended(|| Admission::acquire(parent.path())).unwrap();
    let mut handles = Vec::new();
    let mut names = Vec::new();
    for id in 0..64 {
        let root = fixture(parent.path(), id, 0);
        let handle = open_regular(&root.join(MARKER), false, false).unwrap();
        handle.as_file().try_lock().unwrap();
        handles.push(handle);
        names.push(root.file_name().unwrap().to_os_string());
    }
    let orphan = fixture(parent.path(), 100, 1);
    names.push(orphan.file_name().unwrap().to_os_string());
    let report = scan_entries(&admission, &TransferCancel::default(), names.into_iter().map(Ok)).unwrap();
    assert_eq!(report.live, 64);
    assert_eq!(report.inventory_attempts, 1);
    assert_eq!(report.removed_roots, 1);
    assert!(!orphan.exists());
    drop(handles);
}
#[test]
fn fixed_scan_window_is_honest_and_named_fallback_reaches_a_root_beyond_it() {
    let parent = tempfile::tempdir().unwrap();
    for n in 0..MAX_PARENT {
        file(&parent.path().join(format!("sentinel-{n}")), b"keep");
    }
    let root = fixture(parent.path(), 1, 1);
    let admission = test_uncontended(|| Admission::acquire(parent.path())).unwrap();
    let consumed = std::cell::Cell::new(0);
    let names = (0..MAX_PARENT).map(|n| std::ffi::OsString::from(format!("sentinel-{n}"))).chain(std::iter::once(root.file_name().unwrap().to_os_string()));
    let report = scan_entries(
        &admission,
        &TransferCancel::default(),
        names.map(|n| {
            consumed.set(consumed.get() + 1);
            Ok(n)
        }),
    )
    .unwrap();
    assert_eq!(consumed.get(), MAX_PARENT);
    assert_eq!(report.visited, MAX_PARENT as u32);
    assert!(report.scan_window_exhausted);
    assert!(root.exists());
    drop(admission);
    assert_eq!(named(parent.path(), &root).unwrap().removed_roots, 1);
    for n in 0..MAX_PARENT {
        assert_eq!(std::fs::read(parent.path().join(format!("sentinel-{n}"))).unwrap(), b"keep");
    }
}

#[test]
fn exhausted_creation_collisions_never_reuse_or_clean_the_colliding_directory() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join(format!("{PREFIX}{}-0", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    file(&root.join("human-sentinel"), b"keep collision");
    let mut attempts = 0;
    assert!(
        create(parent.path(), || {
            attempts += 1;
            Ok(0)
        })
        .is_err()
    );
    assert_eq!(attempts, 8);
    assert_eq!(std::fs::read(root.join("human-sentinel")).unwrap(), b"keep collision");
    assert!(!root.join(MARKER).exists());
}
#[cfg(windows)]
#[test]
fn windows_payload_sharing_failure_keeps_marker_until_the_foreign_handle_closes() {
    use std::os::windows::fs::OpenOptionsExt;
    let parent = tempfile::tempdir().unwrap();
    let root = fixture(parent.path(), 1, 1);
    let held = OpenOptions::new().read(true).share_mode(3).open(root.join("stage-0")).unwrap();
    assert!(named(parent.path(), &root).is_err());
    assert!(root.join(MARKER).exists() && root.join("stage-0").exists());
    drop(held);
    assert_eq!(named(parent.path(), &root).unwrap().removed_roots, 1);
}
#[cfg(windows)]
#[test]
fn windows_junction_reparse_roots_are_not_followed() {
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("external-owned-fixture");
    std::fs::create_dir(&target).unwrap();
    file(&target.join("sentinel"), b"keep reparse target");
    struct Process(Option<std::process::Child>);
    impl Drop for Process {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let link = parent.path().join("nodecraft-assets-v1-123-1");
    let mut owned = Process(Some(
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = owned.0.as_mut().unwrap().try_wait().unwrap() {
            owned.0.take();
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "bounded junction fixture creation exceeded deadline");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(reparse(&std::fs::symlink_metadata(&link).unwrap()));
    assert_eq!(named(parent.path(), &link).unwrap().removed_roots, 0);
    assert_eq!(std::fs::read(target.join("sentinel")).unwrap(), b"keep reparse target");
    std::fs::remove_dir(link).unwrap();
}
