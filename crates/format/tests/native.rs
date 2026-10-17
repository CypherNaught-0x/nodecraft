#![cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{AssetStore, NativeStorage, ProjectStorage, Snapshot, StorageLocation, decode, encode, journal};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;

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
