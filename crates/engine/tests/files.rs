use crate::common;
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, MemoryStorage, ProjectStorage, Snapshot, StorageLocation, decode, journal};
use nodecraft_types::{AssetRef, Error, ErrorKind, MediaMeta, Value};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Faults {
    memory: MemoryStorage,
    save: bool,
    published_readonly: bool,
    checkpoint: bool,
    append: Option<usize>,
    flushes: usize,
    flush_error: bool,
    discard_error: bool,
}
#[derive(Clone)]
struct Storage(Rc<RefCell<Faults>>);
fn failure() -> Error {
    Error::new(ErrorKind::Conflict, "injected storage failure")
}
impl ProjectStorage for Storage {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().memory.load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        let mut f = self.0.borrow_mut();
        if f.save {
            return Err(failure());
        }
        f.memory.save(l, b)
    }
    fn save_with_access(&mut self, l: &StorageLocation, b: &[u8]) -> Result<nodecraft_format::ProjectAccess, Error> {
        self.save(l, b)?;
        Ok(if self.0.borrow().published_readonly { nodecraft_format::ProjectAccess::ReadOnly } else { nodecraft_format::ProjectAccess::ReadWrite })
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        let mut f = self.0.borrow_mut();
        if f.checkpoint {
            return Err(failure());
        }
        f.memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        let mut f = self.0.borrow_mut();
        if let Some(n) = f.append {
            f.memory.append(&b[..n.min(b.len())])?;
            return Err(failure());
        }
        f.memory.append(b)
    }
    fn flush(&mut self) -> Result<(), Error> {
        let mut faults = self.0.borrow_mut();
        faults.flushes += 1;
        if faults.flush_error { Err(failure()) } else { Ok(()) }
    }
    fn discard(&mut self) -> Result<(), Error> {
        let mut faults = self.0.borrow_mut();
        if faults.discard_error { Err(failure()) } else { faults.memory.discard() }
    }
}
fn setup() -> (Session, Storage) {
    let storage = Storage(Rc::new(RefCell::new(Faults::default())));
    let session = Session::with_storage(Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    (session, storage)
}
fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":x,"y":0},"gesture":"drag"})
}
fn saved(storage: &Storage, location: &str) -> Snapshot {
    let bytes = storage.0.borrow().memory.files.get(&StorageLocation::from_command(location.into()).unwrap()).unwrap().clone();
    decode(&bytes).unwrap()
}
fn replay(storage: &Storage) -> journal::Recovered {
    journal::recover(&storage.0.borrow().memory.recovery).unwrap()
}

#[test]
fn saves_track_undo_states_and_break_drag_coalescing_without_document_events() {
    let (mut s, storage) = setup();
    assert!(s.file_status().dirty);
    assert!(s.dispatch("file.save", &json!({})).is_err());
    for x in [1, 2] {
        s.dispatch("node.move", &movement(x)).unwrap();
    }
    assert_eq!(replay(&storage).applied, 2);
    assert_eq!(replay(&storage).snapshot.project, *s.document());
    let events = s.events_since(0).unwrap();
    let status = s.status_revision();
    assert_eq!(s.dispatch("file.save_as", &json!({"location":"first.ncraft"})).unwrap(), json!({"saved":true,"recovery_ready":true,"read_only":false}));
    assert!(!s.file_status().dirty);
    assert!(s.status_revision() > status);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(saved(&storage, "first.ncraft").project, *s.document());
    assert_eq!(replay(&storage).applied, 0);
    s.dispatch("node.move", &movement(3)).unwrap();
    assert!(s.file_status().dirty);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!s.file_status().dirty);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert!(s.file_status().dirty);
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.file_status().dirty);
    assert_eq!(s.document(), &common::project());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert!(!s.file_status().dirty);
    assert_eq!(replay(&storage).snapshot.project, *s.document());
    let old_epoch = saved(&storage, "first.ncraft").epoch;
    s.dispatch("file.save", &json!({})).unwrap();
    assert!(saved(&storage, "first.ncraft").epoch > old_epoch);
}

#[test]
fn failed_save_and_open_preserve_document_history_path_and_dirty_state() {
    let (mut s, storage) = setup();
    s.dispatch("file.save_as", &json!({"location":"first.ncraft"})).unwrap();
    s.dispatch("node.move", &movement(1)).unwrap();
    let before = s.document().clone();
    let status = s.file_status();
    let revision = s.revision();
    let recovery = storage.0.borrow().memory.recovery.clone();
    storage.0.borrow_mut().save = true;
    assert!(s.dispatch("file.save_as", &json!({"location":"second.ncraft"})).is_err());
    storage.0.borrow_mut().save = false;
    assert!(s.dispatch("file.open", &json!({"location":"first.ncraft"})).is_err());
    assert!(s.dispatch("file.open", &json!({"location":"missing.ncraft","discard_dirty":true})).is_err());
    storage.0.borrow_mut().checkpoint = true;
    assert!(s.dispatch("file.open", &json!({"location":"first.ncraft","discard_dirty":true})).is_err());
    storage.0.borrow_mut().checkpoint = false;
    assert_eq!(s.document(), &before);
    assert_eq!(s.file_status(), status);
    assert_eq!(s.revision(), revision);
    assert!(s.can_undo());
    assert_eq!(storage.0.borrow().memory.recovery, recovery);
    s.dispatch("file.open", &json!({"location":"first.ncraft","discard_dirty":true})).unwrap();
    assert_eq!(s.document(), &common::project());
    assert!(!s.can_undo());
    assert!(!s.can_redo());
    assert!(!s.file_status().dirty);
    assert_eq!(s.revision(), revision + 1);
    assert_eq!(s.events_since(revision).unwrap().len(), 1);
}

#[test]
fn append_failure_preserves_live_state_latches_availability_and_save_repairs_journal() {
    for prefix in [0, 1, 40, usize::MAX] {
        let (mut s, storage) = setup();
        let before = s.document().clone();
        storage.0.borrow_mut().append = Some(prefix);
        assert!(s.dispatch("node.move", &movement(1)).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), 0);
        assert!(!s.can_undo());
        assert!(s.file_status().recovery_required);
        assert!(s.enabled("node.move", &movement(2)).is_err());
        assert!(s.dispatch("node.move", &movement(2)).is_err());
        storage.0.borrow_mut().append = None;
        s.dispatch("file.save_as", &json!({"location":"repaired.ncraft"})).unwrap();
        assert!(!s.file_status().recovery_required);
        assert!(s.enabled("node.move", &movement(2)).is_ok());
        assert_eq!(replay(&storage).snapshot.project, before);
        assert_eq!(replay(&storage).applied, 0);
        s.dispatch("node.move", &movement(2)).unwrap();
        assert_eq!(replay(&storage).snapshot.project, *s.document());
    }
}

#[test]
fn published_save_with_failed_checkpoint_reports_success_and_recovery_required() {
    let (mut s, storage) = setup();
    s.dispatch("node.move", &movement(1)).unwrap();
    storage.0.borrow_mut().checkpoint = true;
    assert_eq!(s.dispatch("file.save_as", &json!({"location":"saved.ncraft"})).unwrap(), json!({"saved":true,"recovery_ready":false,"read_only":false}));
    assert_eq!(saved(&storage, "saved.ncraft").project, *s.document());
    assert!(!s.file_status().dirty);
    assert!(s.file_status().recovery_required);
    assert!(s.enabled("edit.undo", &json!({})).is_err());
    assert!(s.enabled("node.move", &movement(2)).is_err());
    storage.0.borrow_mut().checkpoint = false;
    s.dispatch("file.save", &json!({})).unwrap();
    assert!(!s.file_status().recovery_required);
    assert!(s.enabled("edit.undo", &json!({})).is_ok());
}

#[test]
fn recovery_includes_new_assets_and_survives_undo_redo() {
    let (mut s, storage) = setup();
    let hash = s.import_asset(b"new asset").unwrap();
    let mut node = common::node(8);
    node.params.insert("asset".into(), Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
    s.dispatch("node.add", &json!({"canvas":common::canvas(),"node":node})).unwrap();
    let recovered = replay(&storage);
    assert_eq!(recovered.snapshot.project, *s.document());
    assert_eq!(recovered.snapshot.assets.get(&hash), Some(b"new asset".as_slice()));
    for command in ["edit.undo", "edit.redo"] {
        s.dispatch(command, &json!({})).unwrap();
        assert_eq!(replay(&storage).snapshot.project, *s.document());
    }
    let recovered = Session::with_storage(replay(&storage).snapshot, Box::<MemoryStorage>::default()).unwrap();
    assert!(recovered.file_status().dirty);
    assert!(recovered.file_status().location.is_none());
    assert!(!recovered.can_undo());
}

#[test]
fn clean_close_discards_recovery_and_dirty_close_requires_explicit_discard() {
    let (mut session, storage) = setup();
    let checkpoint = storage.0.borrow().memory.recovery.clone();
    assert!(session.close_recovery(false).is_err());
    assert_eq!(storage.0.borrow().memory.recovery, checkpoint);
    session.dispatch("file.save_as", &json!({"location":"close.ncraft"})).unwrap();
    assert_eq!(storage.0.borrow().flushes, 1);
    session.close_recovery(false).unwrap();
    assert!(storage.0.borrow().memory.recovery.is_empty());
    assert_eq!(storage.0.borrow().flushes, 2);
    assert!(!session.file_status().available);
    assert!(session.dispatch("node.move", &movement(2)).is_err());
    let (mut dirty, storage) = setup();
    dirty.close_recovery(true).unwrap();
    assert!(storage.0.borrow().memory.recovery.is_empty());
}

#[test]
fn save_prunes_discarded_assets_but_preserves_redo_references() {
    let (mut session, storage) = setup();
    let hash = session.import_asset(b"redo asset").unwrap();
    let unused = session.import_asset(b"unused import").unwrap();
    let mut node = common::node(3);
    node.params.insert("asset".into(), Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
    session.dispatch("node.add", &json!({"canvas":common::canvas(),"node":node})).unwrap();
    session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    session.dispatch("file.save_as", &json!({"location":"gc.ncraft"})).unwrap();
    assert!(session.assets().get(&hash).is_some());
    assert!(session.assets().get(&unused).is_none());
    session.dispatch("edit.redo", &serde_json::Value::Null).unwrap();
    assert_eq!(replay(&storage).snapshot.assets.get(&hash), Some(b"redo asset".as_slice()));
    session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    session.dispatch("node.move", &movement(4)).unwrap();
    session.dispatch("file.save", &serde_json::Value::Null).unwrap();
    assert!(session.assets().get(&hash).is_none());
    assert!(!session.can_redo());
}

#[test]
fn close_failure_notifies_recovery_latch_without_document_events() {
    for flush_error in [true, false] {
        let (mut session, storage) = setup();
        session.dispatch("file.save_as", &json!({"location":"saved.ncraft"})).unwrap();
        let status = session.status_revision();
        let revision = session.revision();
        let events = session.events_since(0).unwrap();
        let recovery = storage.0.borrow().memory.recovery.clone();
        {
            let mut faults = storage.0.borrow_mut();
            faults.flush_error = flush_error;
            faults.discard_error = !flush_error;
        }
        assert!(session.close_recovery(false).is_err());
        assert_eq!(session.status_revision(), status + 1);
        assert!(session.file_status().recovery_required);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(storage.0.borrow().memory.recovery, recovery);
        assert!(session.enabled("node.move", &movement(1)).is_err());
    }
}

#[test]
fn published_readonly_save_adopts_location_savepoint_and_recovery_without_discarding_edits() {
    for failed_checkpoint in [false, true] {
        let (mut session, storage) = setup();
        session.dispatch("file.save_as", &json!({"location":"old.ncraft"})).unwrap();
        session.dispatch("node.move", &movement(12)).unwrap();
        let document = session.document().clone();
        let revision = session.revision();
        storage.0.borrow_mut().published_readonly = true;
        storage.0.borrow_mut().checkpoint = failed_checkpoint;
        assert_eq!(
            session.dispatch("file.save_as", &json!({"location":"published.ncraft"})).unwrap(),
            json!({"saved":true,"recovery_ready":!failed_checkpoint,"read_only":true})
        );
        assert_eq!(session.document(), &document);
        assert_eq!(session.revision(), revision);
        assert_eq!(saved(&storage, "published.ncraft").project, document);
        assert_eq!(session.file_status().location, Some(StorageLocation::from_command("published.ncraft".into()).unwrap()));
        assert!(!session.file_status().dirty);
        assert!(session.file_status().read_only);
        assert_eq!(session.file_status().recovery_required, failed_checkpoint);
        assert!(session.enabled("file.save", &json!({})).is_err());
        assert!(session.dispatch("file.save", &json!({})).is_err());
        if !failed_checkpoint {
            assert_eq!(replay(&storage).snapshot.project, document);
            session.dispatch("node.move", &movement(14)).unwrap();
        }
        storage.0.borrow_mut().published_readonly = false;
        storage.0.borrow_mut().checkpoint = false;
        session.dispatch("file.save_as", &json!({"location":"independent.ncraft"})).unwrap();
        assert!(!session.file_status().read_only);
        assert!(!session.file_status().recovery_required);
        session.enabled("file.save", &json!({})).unwrap();
    }
}

#[test]
fn persistent_layout_preserves_admission_replay_savepoint_and_failure_atomicity() {
    for mode in ["grid", "layered"] {
        let (mut session, storage) = setup();
        session.dispatch("file.save_as", &json!({"location":"layout.ncraft"})).unwrap();
        let original = session.document().clone();
        let request = json!({"mode":mode});
        session.swap_resource(Some(common::layout_context(&session)));
        let checkpoint = storage.0.borrow().memory.recovery.clone();
        session.enabled("canvas.auto_format", &request).unwrap();
        assert_eq!(storage.0.borrow().memory.recovery, checkpoint);
        assert!(!session.file_status().dirty);
        session.dispatch("canvas.auto_format", &request).unwrap();
        let formatted = session.document().clone();
        assert_ne!(formatted, original);
        assert!(session.file_status().dirty);
        assert_eq!(replay(&storage).applied, 1);
        assert_eq!(replay(&storage).snapshot.project, formatted);
        // Stale geometry and then exact no-op must not append, alter status or history.
        let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status(), session.status_revision());
        let journal = storage.0.borrow().memory.recovery.clone();
        assert!(session.dispatch("canvas.auto_format", &request).is_err());
        session.swap_resource(Some(common::layout_context(&session)));
        assert_eq!(session.dispatch("canvas.auto_format", &request).unwrap(), json!({"moved":0}));
        assert_eq!(
            (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status(), session.status_revision()),
            before
        );
        assert_eq!(storage.0.borrow().memory.recovery, journal);
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document(), &original);
        assert!(!session.file_status().dirty);
        assert_eq!(replay(&storage).snapshot.project, original);
        session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(session.document(), &formatted);
        assert_eq!(replay(&storage).snapshot.project, formatted);
        session.dispatch("edit.undo", &json!({})).unwrap();
        session.swap_resource(Some(common::layout_context(&session)));
        let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.can_undo(), session.can_redo());
        storage.0.borrow_mut().append = Some(0);
        assert!(session.dispatch("canvas.auto_format", &request).is_err());
        assert_eq!((session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.can_undo(), session.can_redo()), before);
        assert!(session.file_status().recovery_required);
        assert!(session.enabled("canvas.auto_format", &request).is_err());
        storage.0.borrow_mut().append = None;
        session.dispatch("file.save", &json!({})).unwrap();
        assert!(session.enabled("canvas.auto_format", &request).is_ok());
        session.dispatch("canvas.auto_format", &request).unwrap();
        assert_eq!(replay(&storage).snapshot.project, *session.document());
    }
}

#[test]
fn recovery_lineage_tracks_actual_saves_and_successful_replacements() {
    let (mut s, storage) = setup();
    let original = s.document_lineage();
    let snapshot = Snapshot { project: s.document().clone(), assets: s.assets().clone(), epoch: 0 };
    s.recover(snapshot.clone(), None).unwrap();
    let token = s.document_lineage();
    assert!(!s.is_current_document(&original));
    assert!(!s.has_saved_document(&token));
    s.dispatch("node.move", &movement(1)).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert!(s.is_current_document(&token));
    assert!(!s.has_saved_document(&token));
    storage.0.borrow_mut().save = true;
    assert!(s.dispatch("file.save_as", &json!({"location":"failed.ncraft"})).is_err());
    assert!(s.is_current_document(&token));
    assert!(!s.has_saved_document(&token));
    storage.0.borrow_mut().save = false;
    storage.0.borrow_mut().checkpoint = true;
    let result = s.dispatch("file.save_as", &json!({"location":"published.ncraft"})).unwrap();
    assert_eq!(result["recovery_ready"], false);
    assert!(s.has_saved_document(&token));
    assert_eq!(saved(&storage, "published.ncraft").project, *s.document());
    storage.0.borrow_mut().checkpoint = false;
    s.dispatch("file.open", &json!({"location":"published.ncraft","discard_dirty":true})).unwrap();
    assert!(!s.is_current_document(&token));
    assert!(!s.has_saved_document(&token));
    let opened = s.document_lineage();
    assert!(!s.has_saved_document(&opened));
    s.dispatch("file.save_as", &json!({"location":"renamed.ncraft"})).unwrap();
    assert!(s.has_saved_document(&opened));
    let (other, _) = setup();
    assert!(!other.is_current_document(&opened));
}
#[test]
fn failed_open_and_recover_do_not_rotate_or_fabricate_a_save_witness() {
    let (mut s, storage) = setup();
    let snapshot = Snapshot { project: s.document().clone(), assets: s.assets().clone(), epoch: 0 };
    s.recover(snapshot.clone(), None).unwrap();
    let token = s.document_lineage();
    assert!(s.dispatch("file.open", &json!({"location":"missing.ncraft","discard_dirty":true})).is_err());
    assert!(s.is_current_document(&token));
    assert!(!s.has_saved_document(&token));
    storage.0.borrow_mut().checkpoint = true;
    assert!(s.recover(snapshot.clone(), None).is_err());
    assert!(s.is_current_document(&token));
    assert!(!s.has_saved_document(&token));
    storage.0.borrow_mut().checkpoint = false;
    s.dispatch("file.save_as", &json!({"location":"saved.ncraft"})).unwrap();
    assert!(s.has_saved_document(&token));
    s.recover(snapshot, None).unwrap();
    assert!(!s.is_current_document(&token));
    assert!(!s.has_saved_document(&s.document_lineage()));
}

#[test]
fn undo_after_another_peers_edit_leaves_the_file_unsaved() {
    let (mut s, _storage) = setup();
    s.dispatch("file.save_as", &json!({"location":"shared.ncraft"})).unwrap();
    s.dispatch("node.move", &movement(5)).unwrap();
    let card = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([77; 16]), "text");
    s.apply_remote(nodecraft_graph::Changeset::new(vec![nodecraft_graph::Change::AddNode { canvas: common::canvas(), node: card }])).unwrap();
    // Undoing the move takes the file back to its saved positions, but the other card stays.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.file_status().dirty);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert!(s.file_status().dirty);
}
