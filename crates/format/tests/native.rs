#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{AssetStore, NativeStorage, ProjectStorage, Snapshot, StorageLocation, decode, encode, journal};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;

#[test]
fn render_source_versions_survive_native_save_and_recovery() {
    use nodecraft_graph::{Canvas, Change, Generation, Node, SourceVersion};
    use nodecraft_types::{Blake3, CanvasId, GenerationId, NodeId};
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Timeline sources").unwrap();
    let canvas = CanvasId::from_bytes([2; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Timeline") }.apply(&mut project).unwrap();
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "video.timeline");
    let mut generation = Generation::new(GenerationId::from_bytes([4; 16]), vec![]);
    generation.source_versions.push(SourceVersion { role: "clip".into(), hash: Blake3::from_bytes([8; 32]), item: None });
    node.active = Some(generation.id);
    node.history.push(generation);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let snapshot = Snapshot { project, assets: AssetStore::default(), epoch: 0 };
    let dir = tempfile::tempdir().unwrap();
    let recovery = dir.path().join("recovery");
    let location = StorageLocation::Path(dir.path().join("timeline.ncraft"));
    let mut storage = NativeStorage::new(recovery.clone()).unwrap();
    storage.checkpoint(&journal::checkpoint(&snapshot).unwrap().bytes).unwrap();
    storage.save(&location, &encode(&snapshot).unwrap()).unwrap();
    assert_eq!(decode(&storage.load(&location).unwrap()).unwrap(), snapshot);
    drop(storage);
    assert_eq!(NativeStorage::new(recovery).unwrap().recover().unwrap().snapshot, snapshot);
}

#[test]
fn atomic_save_recovery_lock_and_failed_save_preserve_previous_data() {
    let dir = tempfile::tempdir().unwrap();
    let recovery = dir.path().join("recovery");
    let file = dir.path().join("project.ncraft");
    let location = StorageLocation::Path(file.clone());
    let snapshot = Snapshot { project: Project::new(ProjectId::from_bytes([1; 16]), "Fixture").unwrap(), assets: AssetStore::default(), epoch: 0 };
    let bytes = encode(&snapshot).unwrap();
    let mut storage = NativeStorage::new(recovery.clone()).unwrap();
    assert!(NativeStorage::new(recovery.clone()).is_err());
    storage.checkpoint(&journal::checkpoint(&snapshot).unwrap().bytes).unwrap();
    storage.save(&location, &bytes).unwrap();
    assert_eq!(decode(&storage.load(&location).unwrap()).unwrap(), snapshot);
    assert!(storage.save(&StorageLocation::Path(recovery.clone()), &bytes).is_err());
    assert!(storage.save(&StorageLocation::Path(dir.path().join("absent/file")), &bytes).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    drop(storage);
    // Persistent lock file remains; an OS lock releases automatically, including after crashes.
    let storage = NativeStorage::new(recovery).unwrap();
    assert_eq!(storage.recover().unwrap().snapshot, snapshot);
}

#[cfg(unix)]
#[test]
fn native_paths_preserve_non_utf8_and_reject_symlink_alias_of_recovery() {
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };
    let dir = tempfile::tempdir().unwrap();
    let recovery = dir.path().join("recovery");
    let mut storage = NativeStorage::new(recovery.clone()).unwrap();
    storage.checkpoint(b"checkpoint").unwrap();
    let alias = dir.path().join("alias");
    symlink(&recovery, &alias).unwrap();
    assert!(storage.save(&StorageLocation::Path(alias), b"new").is_err());
    let location = StorageLocation::Path(dir.path().join(OsString::from_vec(b"media-\xff.ncraft".to_vec())));
    // Some filesystems (including macOS volumes) reject non-UTF8 filenames. Either preserve
    // the exact spelling or return an error; never create the lossy replacement filename.
    let StorageLocation::Path(path) = &location else { panic!() };
    if std::fs::write(path, b"old").is_ok() {
        storage.save(&location, b"bytes").unwrap();
        assert_eq!(storage.load(&location).unwrap(), b"bytes");
    } else {
        assert!(storage.save(&location, b"bytes").is_err());
    }
    assert!(!dir.path().join("media-\u{fffd}.ncraft").exists());
}

#[test]
fn child_exits_without_releasing_storage_explicitly() {
    let Some(path) = std::env::var_os("NODECRAFT_TEST_CRASH_RECOVERY") else { return };
    let mut storage = NativeStorage::new(path.into()).unwrap();
    let snapshot = Snapshot { project: Project::new(ProjectId::from_bytes([1; 16]), "After crash").unwrap(), assets: AssetStore::default(), epoch: 0 };
    storage.checkpoint(&journal::checkpoint(&snapshot).unwrap().bytes).unwrap();
    std::process::exit(0); // Deliberately bypass Rust destructors, as a crashed process would.
}

#[test]
fn crash_releases_os_lock_and_keeps_recovery_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_exits_without_releasing_storage_explicitly"])
        .env("NODECRAFT_TEST_CRASH_RECOVERY", &path)
        .status()
        .unwrap();
    assert!(status.success());
    let storage = NativeStorage::new(path).unwrap();
    assert_eq!(storage.recover().unwrap().snapshot.project.title(), "After crash");
}

#[test]
fn project_writer_lock_prevents_two_sessions_overwriting_the_same_destination() {
    let dir = tempfile::tempdir().unwrap();
    let location = StorageLocation::Path(dir.path().join("shared.ncraft"));
    let mut first = NativeStorage::new(dir.path().join("recovery-one")).unwrap();
    let mut second = NativeStorage::new(dir.path().join("recovery-two")).unwrap();
    first.save(&location, b"first").unwrap();
    assert!(second.save(&location, b"second").is_err());
    assert_eq!(first.load(&location).unwrap(), b"first");
    drop(first);
    second.save(&location, b"second").unwrap();
    assert_eq!(second.load(&location).unwrap(), b"second");
}

#[test]
fn concurrent_distinct_writable_opens_do_not_collide_on_the_upstream_pid_probe() {
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Barrier::new(2));
    // Same-parent constructor admission is deliberately nonblocking. Prepare owners before
    // racing the actual Open/probe operation this control is intended to exercise.
    let fixtures: Vec<_> = (0..2)
        .map(|index| {
            let path = dir.path().join(format!("existing-{index}.ncraft"));
            std::fs::write(&path, b"public fixture").unwrap();
            (NativeStorage::new(dir.path().join(format!("recovery-{index}"))).unwrap(), path)
        })
        .collect();
    let handles: Vec<_> = fixtures
        .into_iter()
        .map(|(mut storage, path)| {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let mut modes = Vec::new();
                for _ in 0..100 {
                    gate.wait();
                    modes.push(storage.prepare_open(&StorageLocation::Path(path.clone())).map(|loaded| loaded.access));
                    storage.commit_open();
                    gate.wait();
                }
                modes
            })
        })
        .collect();
    for handle in handles {
        for mode in handle.join().unwrap() {
            assert_eq!(mode.unwrap(), nodecraft_format::ProjectAccess::ReadWrite);
        }
    }
}

#[test]
fn simultaneous_absent_aliases_use_the_published_canonical_guard() {
    use std::sync::{Arc, Barrier};
    for names in [["NewProject.ncraft", "newproject.ncraft"], ["café.ncraft", "cafe\u{301}.ncraft"], ["distinct-one.ncraft", "distinct-two.ncraft"]] {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("projects");
        std::fs::create_dir(&parent).unwrap();
        // Query this actual directory's alias semantics, then restore absence.
        let paths = names.map(|name| parent.join(name));
        std::fs::write(&paths[0], b"probe").unwrap();
        let aliases = paths[1].exists();
        std::fs::remove_file(&paths[0]).unwrap();
        let entered = Arc::new(Barrier::new(2));
        let done = Arc::new(Barrier::new(2));
        // Constructor contention is a separate tested contract; keep both admitted owners
        // alive while the original Save publication race and alias barriers run unchanged.
        let fixtures: Vec<_> =
            paths.iter().enumerate().map(|(index, path)| (NativeStorage::new(dir.path().join(format!("recovery-{index}"))).unwrap(), path.clone())).collect();
        let handles: Vec<_> = fixtures
            .into_iter()
            .map(|(mut storage, path)| {
                let entered = Arc::clone(&entered);
                let done = Arc::clone(&done);
                std::thread::spawn(move || {
                    entered.wait();
                    let outcome = storage.save_with_access(&StorageLocation::Path(path.clone()), b"public fixture");
                    done.wait(); // Keep both storages/guards alive through both attempts.
                    (storage, path, outcome)
                })
            })
            .collect();
        let owners: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
        assert_eq!(owners.iter().filter(|(_, _, outcome)| outcome.is_ok()).count(), if aliases { 1 } else { 2 });
        let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
        for (_, path, outcome) in &owners {
            match outcome {
                Ok(access) => {
                    assert_eq!(*access, nodecraft_format::ProjectAccess::ReadWrite);
                    let canonical = path.canonicalize().unwrap();
                    assert_eq!(probe.prepare_open(&StorageLocation::Path(canonical)).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
                }
                Err(error) => assert_eq!(error.kind, nodecraft_types::ErrorKind::Busy),
            }
        }
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), if aliases { 1 } else { 2 });
    }
}

#[cfg(unix)]
#[test]
fn dangling_alias_refuses_publication_retains_old_guard_and_existing_aliases_work() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let aliases = dir.path().join("aliases");
    let targets = dir.path().join("targets");
    std::fs::create_dir(&aliases).unwrap();
    std::fs::create_dir(&targets).unwrap();
    let target = targets.join("new.ncraft");
    let alias = aliases.join("alias.ncraft");
    symlink(&target, &alias).unwrap();
    let old = StorageLocation::Path(dir.path().join("old.ncraft"));
    let mut owner = NativeStorage::new(dir.path().join("owner-recovery")).unwrap();
    owner.save(&old, b"old source").unwrap();
    assert!(owner.save_with_access(&StorageLocation::Path(alias.clone()), b"must not publish").is_err());
    assert!(!target.exists());
    assert!(std::fs::symlink_metadata(&alias).unwrap().file_type().is_symlink());
    let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
    assert_eq!(probe.load(&old).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    let direct = StorageLocation::Path(target.clone());
    probe.save(&direct, b"target").unwrap();
    assert_eq!(owner.save(&StorageLocation::Path(alias.clone()), b"must not overwrite").unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    assert_eq!(std::fs::read(&target).unwrap(), b"target");
    drop(probe);
    // A now-resolvable chain retains the existing canonical-alias behavior.
    let chain = aliases.join("chain.ncraft");
    symlink("alias.ncraft", &chain).unwrap();
    assert_eq!(owner.save_with_access(&StorageLocation::Path(chain), b"existing alias").unwrap(), nodecraft_format::ProjectAccess::ReadWrite);
    assert_eq!(std::fs::read(&target).unwrap(), b"existing alias");
    let mut probe = NativeStorage::new(dir.path().join("probe-again")).unwrap();
    assert_eq!(probe.load(&direct).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    assert_eq!(probe.load(&old).unwrap(), b"old source");
}

#[cfg(unix)]
#[test]
fn concurrent_dangling_cross_directory_alias_never_publishes_twice() {
    use std::{
        os::unix::fs::symlink,
        sync::{Arc, Barrier},
    };
    let dir = tempfile::tempdir().unwrap();
    let aliases = dir.path().join("aliases");
    let targets = dir.path().join("targets");
    std::fs::create_dir(&aliases).unwrap();
    std::fs::create_dir(&targets).unwrap();
    let target = targets.join("new.ncraft");
    let alias = aliases.join("alias.ncraft");
    symlink(&target, &alias).unwrap();
    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [alias, target.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let recovery = dir.path().join(format!("recovery-{index}"));
            // Parent admission is nonblocking; create the fixture owners before
            // racing the actual saves under their original barriers.
            let mut storage = NativeStorage::new(recovery).unwrap();
            let start = Arc::clone(&start);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                start.wait();
                let outcome = storage.save_with_access(&StorageLocation::Path(path), &vec![index as u8; 1024 * 1024]);
                done.wait();
                (storage, outcome)
            })
        })
        .collect();
    let owners: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert!(owners[0].1.is_err(), "alias must reject before creation or find the direct owner's held canonical guard");
    assert_eq!(*owners[1].1.as_ref().unwrap(), nodecraft_format::ProjectAccess::ReadWrite);
    assert_eq!(std::fs::read(&target).unwrap(), vec![1; 1024 * 1024]);
}

#[test]
fn damaged_recovery_preserves_original_bytes_and_refuses_append_until_a_new_checkpoint() {
    use nodecraft_graph::{Change, Changeset};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged-recovery");
    let snapshot = Snapshot { project: Project::new(ProjectId::from_bytes([1; 16]), "Checkpoint").unwrap(), assets: AssetStore::default(), epoch: 0 };
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    let mut bytes = checkpoint.bytes.clone();
    let change = Changeset::new(vec![Change::SetProjectTitle { title: "Validated prefix".into() }]);
    bytes.extend(journal::frame(checkpoint.identity, 1, &change, &snapshot.assets, &Default::default()).unwrap());
    let damaged_at = bytes.len();
    bytes.extend_from_slice(&[0; 64]);
    bytes.extend(journal::frame(checkpoint.identity, 3, &change, &snapshot.assets, &Default::default()).unwrap());
    std::fs::write(&path, &bytes).unwrap();
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.applied, 1);
    assert_eq!(recovered.damage.unwrap().offset, damaged_at as u64);
    assert!(storage.append(b"must not append after damage").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let checkpoint = journal::checkpoint(&recovered.snapshot).unwrap();
    storage.checkpoint(&checkpoint.bytes).unwrap();
    storage.append(&journal::frame(checkpoint.identity, 1, &change, &recovered.snapshot.assets, &Default::default()).unwrap()).unwrap();
    assert!(storage.recover().unwrap().damage.is_none());
}

#[test]
fn destination_transitions_release_old_locks_without_a_historical_limit_or_project_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    std::fs::create_dir(&projects).unwrap();
    let mut owner = NativeStorage::new(dir.path().join("owner-recovery")).unwrap();
    let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
    let mut previous = None;
    for n in 0..70 {
        let location = StorageLocation::Path(projects.join(format!("project-{n}.ncraft")));
        owner.save(&location, b"public fixture").unwrap();
        assert_eq!(probe.load(&location).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
        if let Some(previous) = previous {
            assert_eq!(probe.load(&previous).unwrap(), b"public fixture");
        }
        previous = Some(location);
    }
    assert_eq!(std::fs::read_dir(&projects).unwrap().count(), 70, "no sidecars or leaked probes in project folders");
}

#[test]
fn staged_load_and_failed_save_retain_the_original_guard_until_publication() {
    let dir = tempfile::tempdir().unwrap();
    let old = StorageLocation::Path(dir.path().join("old.ncraft"));
    let new = StorageLocation::Path(dir.path().join("new.ncraft"));
    let mut owner = NativeStorage::new(dir.path().join("owner-recovery")).unwrap();
    let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
    owner.save(&old, b"old").unwrap();
    std::fs::write(dir.path().join("new.ncraft"), b"new").unwrap();
    owner.prepare_open(&new).unwrap();
    assert_eq!(probe.load(&old).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    assert_eq!(probe.load(&new).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    owner.cancel_open();
    assert_eq!(probe.load(&new).unwrap(), b"new");
    assert_eq!(probe.load(&old).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    drop(probe);
    let directory = dir.path().join("not-a-file");
    std::fs::create_dir(&directory).unwrap();
    assert!(owner.save(&StorageLocation::Path(directory.clone()), b"must fail").is_err());
    let mut probe = NativeStorage::new(dir.path().join("probe-again")).unwrap();
    assert_eq!(probe.load(&old).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    assert_ne!(probe.load(&StorageLocation::Path(directory)).unwrap_err().kind, nodecraft_types::ErrorKind::Busy, "failed save releases candidate");
    owner.prepare_open(&new).unwrap();
    owner.commit_open();
    assert_eq!(probe.load(&old).unwrap(), b"old");
    assert_eq!(probe.load(&new).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
}

#[cfg(unix)]
#[test]
fn readonly_directory_opens_explicitly_readonly_and_save_as_preserves_the_source() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    struct Restore(std::path::PathBuf, std::fs::Permissions);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    let file = readonly.join("example.ncraft");
    std::fs::write(&file, b"public example").unwrap();
    let restore = Restore(readonly.clone(), std::fs::metadata(&readonly).unwrap().permissions());
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).unwrap();
    let location = StorageLocation::Path(file.clone());
    let mut storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let loaded = storage.prepare_open(&location).unwrap();
    assert_eq!(loaded.access, nodecraft_format::ProjectAccess::ReadOnly);
    assert_eq!(loaded.bytes, b"public example");
    storage.commit_open();
    assert!(storage.save(&location, b"must not overwrite").is_err());
    let alias = dir.path().join("example-alias");
    symlink(&file, &alias).unwrap();
    assert!(storage.save(&StorageLocation::Path(alias.clone()), b"alias cannot bypass read-only").is_err());
    let mut other = NativeStorage::new(dir.path().join("other-recovery")).unwrap();
    assert_eq!(other.prepare_open(&StorageLocation::Path(alias)).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
    storage.save(&StorageLocation::Path(dir.path().join("saved-copy.ncraft")), b"editable copy").unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"public example");
    assert_eq!(std::fs::read_dir(&readonly).unwrap().count(), 1);
    drop(restore);
}
