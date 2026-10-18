#![cfg(not(target_arch = "wasm32"))]
mod common;
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, LoadedProject, NativeStorage, ProjectStorage, Snapshot, StorageLocation, encode};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::{cell::RefCell, path::Path, rc::Rc};

struct State {
    storage: NativeStorage,
    fail_checkpoint: bool,
}
#[derive(Clone)]
struct Storage(Rc<RefCell<State>>);
impl ProjectStorage for Storage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().storage.load(location)
    }
    fn prepare_open(&mut self, location: &StorageLocation) -> Result<LoadedProject, Error> {
        self.0.borrow_mut().storage.prepare_open(location)
    }
    fn commit_open(&mut self) {
        self.0.borrow_mut().storage.commit_open();
    }
    fn cancel_open(&mut self) {
        self.0.borrow_mut().storage.cancel_open();
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().storage.save(location, bytes)
    }
    fn save_with_access(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<nodecraft_format::ProjectAccess, Error> {
        self.0.borrow_mut().storage.save_with_access(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        if state.fail_checkpoint {
            return Err(Error::new(ErrorKind::Conflict, "injected checkpoint failure"));
        }
        state.storage.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().storage.append(bytes)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().storage.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().storage.discard()
    }
}
fn snapshot() -> Snapshot {
    Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 }
}
fn params(path: &Path) -> serde_json::Value {
    json!({"location":path.to_str().unwrap(),"discard_dirty":true})
}
fn setup(dir: &Path) -> (Session, Storage) {
    let storage = Storage(Rc::new(RefCell::new(State { storage: NativeStorage::new(dir.join("recovery")).unwrap(), fail_checkpoint: false })));
    (Session::with_storage(snapshot(), Box::new(storage.clone())).unwrap(), storage)
}

#[test]
fn failed_load_decode_checkpoint_and_save_keep_old_document_location_recovery_and_guard() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.ncraft");
    let next = dir.path().join("next.ncraft");
    let malformed = dir.path().join("malformed.ncraft");
    let (mut session, storage) = setup(dir.path());
    session.dispatch("file.save_as", &json!({"location":old.to_str().unwrap()})).unwrap();
    let old_bytes = std::fs::read(&old).unwrap();
    let bytes = encode(&snapshot()).unwrap();
    std::fs::write(&next, &bytes).unwrap();
    std::fs::write(&malformed, b"not an archive").unwrap();
    let status = session.file_status();
    let document = session.document().clone();
    let revision = session.revision();
    let recovery = std::fs::read(dir.path().join("recovery")).unwrap();
    for candidate in [dir.path().join("missing.ncraft"), malformed.clone(), next.clone()] {
        storage.0.borrow_mut().fail_checkpoint = candidate == next;
        assert!(session.dispatch("file.open", &params(&candidate)).is_err());
        storage.0.borrow_mut().fail_checkpoint = false;
        assert_eq!(session.document(), &document);
        assert_eq!(session.file_status(), status);
        assert_eq!(session.revision(), revision);
        assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), recovery);
        let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
        assert_eq!(probe.load(&StorageLocation::Path(old.clone())).unwrap_err().kind, ErrorKind::Busy);
        if candidate.exists() {
            assert!(probe.load(&StorageLocation::Path(candidate)).is_ok(), "failed open must drop its candidate guard");
        }
    }
    let bad_save = dir.path().join("directory");
    std::fs::create_dir(&bad_save).unwrap();
    assert!(session.dispatch("file.save_as", &json!({"location":bad_save.to_str().unwrap()})).is_err());
    #[cfg(unix)]
    {
        let absent = dir.path().join("unpublished.ncraft");
        let alias = dir.path().join("dangling.ncraft");
        std::os::unix::fs::symlink(&absent, &alias).unwrap();
        assert!(session.dispatch("file.save_as", &json!({"location":alias.to_str().unwrap()})).is_err());
        assert!(!absent.exists());
    }
    assert_eq!(session.file_status(), status);
    assert_eq!(session.document(), &document);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), recovery);
    let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
    assert_eq!(probe.load(&StorageLocation::Path(old.clone())).unwrap_err().kind, ErrorKind::Busy);
    let result = session.dispatch("file.open", &params(&next)).unwrap();
    assert_eq!(result, json!({"opened":true,"read_only":false}));
    assert_eq!(probe.load(&StorageLocation::Path(old)).unwrap(), old_bytes);
    assert_eq!(probe.load(&StorageLocation::Path(next)).unwrap_err().kind, ErrorKind::Busy);
}

#[cfg(unix)]
#[test]
fn readonly_source_status_disables_save_but_local_edit_and_save_as_remain_available() {
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
    let source = readonly.join("example.ncraft");
    let bytes = encode(&snapshot()).unwrap();
    std::fs::write(&source, &bytes).unwrap();
    let restore = Restore(readonly.clone(), std::fs::metadata(&readonly).unwrap().permissions());
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (mut session, _storage) = setup(dir.path());
    assert_eq!(session.dispatch("file.open", &params(&source)).unwrap(), json!({"opened":true,"read_only":true}));
    assert!(session.file_status().read_only);
    assert!(!session.file_status().dirty);
    assert!(session.enabled("file.save", &json!({})).is_err());
    assert!(session.dispatch("file.save", &json!({})).is_err());
    session.dispatch("node.move", &json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":100,"y":50}})).unwrap();
    assert!(session.file_status().dirty);
    let alias = dir.path().join("alias");
    symlink(&source, &alias).unwrap();
    assert!(session.dispatch("file.save_as", &json!({"location":alias.to_str().unwrap()})).is_err());
    assert!(session.file_status().read_only);
    let destination = dir.path().join("copy.ncraft");
    session.enabled("file.save_as", &json!({"location":destination.to_str().unwrap()})).unwrap();
    session.dispatch("file.save_as", &json!({"location":destination.to_str().unwrap()})).unwrap();
    assert!(!session.file_status().read_only);
    assert!(!session.file_status().dirty);
    session.enabled("file.save", &json!({})).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&readonly).unwrap().count(), 1);
    let mut probe = NativeStorage::new(dir.path().join("probe-recovery")).unwrap();
    assert!(probe.load(&StorageLocation::Path(source)).is_ok());
    drop(restore);
}
