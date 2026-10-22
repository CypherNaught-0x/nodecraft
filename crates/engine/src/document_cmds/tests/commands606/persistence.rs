use super::*;
use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation, journal};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct State {
    memory: MemoryStorage,
    refuse_append: bool,
    appends: usize,
}

#[derive(Clone, Default)]
struct Storage(Rc<RefCell<State>>);

impl ProjectStorage for Storage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().memory.load(location)
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().memory.save(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().memory.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut state = self.0.borrow_mut();
        state.appends += 1;
        if state.refuse_append {
            return Err(Error::new(ErrorKind::Conflict, "injected extraction append refusal"));
        }
        state.memory.append(bytes)
    }
}

#[test]
fn extraction_publishes_media_and_edit_only_after_one_durable_append() {
    for command in ["document.extract_page", "document.extract_text"] {
        for refuse in [false, true] {
            let original = session();
            let storage = Storage::default();
            let snapshot = Snapshot { project: original.document().clone(), assets: original.assets().clone(), epoch: 0 };
            let mut s = Session::with_storage(snapshot.clone(), Box::new(storage.clone())).unwrap();
            storage.0.borrow_mut().refuse_append = refuse;
            let result = s.dispatch(command, &params(2));
            assert_eq!(storage.0.borrow().appends, 1);
            let recovered = journal::recover(&storage.0.borrow().memory.recovery).unwrap();
            if refuse {
                assert!(result.is_err());
                assert_eq!(s.document(), &snapshot.project);
                assert_eq!(s.assets().len(), snapshot.assets.len());
                assert_eq!(s.revision(), 0);
                assert!(!s.can_undo());
                assert!(s.events_since(0).unwrap().is_empty());
                assert_eq!(recovered.applied, 0);
                assert_eq!(recovered.snapshot.project, snapshot.project);
                assert_eq!(recovered.snapshot.assets.len(), snapshot.assets.len());
            } else {
                assert_eq!(result.unwrap(), json!({"id":NEW}));
                assert_eq!(recovered.applied, 1);
                assert_eq!(recovered.snapshot.project, *s.document());
                assert_eq!(recovered.snapshot.assets.len(), s.assets().len());
                assert!(node(&s, NEW).is_some());
                assert_eq!(s.events_since(0).unwrap().first().unwrap().command, command);
                s.dispatch("edit.undo", &json!({})).unwrap();
                assert_eq!(s.document(), &snapshot.project);
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn extracted_page_survives_native_storage_recovery_with_its_verified_asset() {
    let original = session();
    let dir = tempfile::tempdir().unwrap();
    let recovery = dir.path().join("recovery");
    let snapshot = Snapshot { project: original.document().clone(), assets: original.assets().clone(), epoch: 0 };
    let mut s = Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(recovery.clone()).unwrap())).unwrap();
    s.dispatch("document.extract_page", &params(2)).unwrap();
    let Some(Value::Media(asset)) = node(&s, NEW).unwrap().params.get("asset") else { panic!("extracted PDF") };
    let asset = asset.clone();
    let expected = s.document().clone();
    drop(s);
    let storage = nodecraft_format::NativeStorage::new(recovery).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.applied, 1);
    assert_eq!(recovered.snapshot.project, expected);
    let bytes = recovered
        .snapshot
        .assets
        .open_verified(&asset, &TransferCancel::default(), &StreamMetrics::default())
        .unwrap()
        .read_small(MAX_ASSET_BYTES, &StreamMetrics::default())
        .unwrap();
    assert_eq!(Document::open(&bytes, &DocumentLimits::default()).unwrap().text(0).unwrap(), "Top left\nBottom right");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn oversized_native_pdf_is_refused_from_its_descriptor_before_opening_it() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource};
    use std::sync::Arc;
    let original = session();
    let dir = tempfile::tempdir().unwrap();
    let snapshot = Snapshot { project: original.document().clone(), assets: original.assets().clone(), epoch: 0 };
    let mut s = Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap())).unwrap();
    let mut bytes = pdf(&["BT /F1 12 Tf 20 150 Td (Large public PDF fixture) Tj ET"]);
    bytes.resize(MAX_ASSET_BYTES + 1, b' ');
    let length = bytes.len() as u64;
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, length).unwrap();
    let hash = s.import_asset_from(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let Some(Value::Media(mut asset)) = node(&s, SOURCE_ID).unwrap().params.get("asset").cloned() else { panic!("PDF fixture") };
    asset.hash = hash;
    asset.meta.page_count = Some(1);
    s.dispatch("node.param_set", &json!({"canvas":C,"id":SOURCE_ID,"name":"asset","value":Value::Media(asset)})).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let assets = s.assets().len();
    for command in ["document.extract_page", "document.extract_text"] {
        let args = params(1);
        assert!(s.enabled(command, &args).unwrap_err().message.contains("16 MiB"));
        assert!(s.impact(command, &args).is_err());
        assert!(s.dispatch(command, &args).unwrap_err().message.contains("16 MiB"));
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert_eq!(s.assets().len(), assets);
        assert!(s.events_since(revision).unwrap().is_empty());
    }
}
