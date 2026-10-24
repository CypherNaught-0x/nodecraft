//! Local legacy identities are migrated before durable adoption, without guessing old charges.
use crate::common;
use nodecraft_engine::Session;
use nodecraft_exec::cost::{Charge, UsageEntry, UsageLog};
use nodecraft_format::{AssetStore, LoadedProject, MemoryStorage, ProjectAccess, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::Change;
use nodecraft_types::{Currency, Error, ErrorKind, Money, ProjectId};
use serde_json::json;
use std::sync::{Arc, Mutex};

const LEGACY: ProjectId = ProjectId::from_bytes([2; 16]);

#[derive(Default)]
struct State {
    memory: MemoryStorage,
    fail_checkpoint: bool,
    read_only: bool,
}
#[derive(Clone, Default)]
struct Store(Arc<Mutex<State>>);
impl ProjectStorage for Store {
    fn prepare_open(&mut self, location: &StorageLocation) -> Result<LoadedProject, Error> {
        let bytes = self.load(location)?;
        let access = if self.0.lock().unwrap().read_only { ProjectAccess::ReadOnly } else { ProjectAccess::ReadWrite };
        Ok(LoadedProject { bytes, access })
    }
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.lock().unwrap().memory.load(location)
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.save(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut state = self.0.lock().unwrap();
        if state.fail_checkpoint {
            return Err(Error::new(ErrorKind::Internal, "injected checkpoint failure"));
        }
        state.memory.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.append(bytes)
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.0.lock().unwrap().memory.discard()
    }
}
fn snapshot(legacy: bool) -> Snapshot {
    let mut project = common::project();
    if legacy {
        Change::SetProjectId { id: LEGACY }.apply(&mut project).unwrap();
    }
    Snapshot { project, assets: AssetStore::default(), epoch: 0 }
}
fn setup() -> (Session, Store) {
    setup_named("old.ncraft")
}
fn setup_named(name: &str) -> (Session, Store) {
    let mut storage = Store::default();
    storage.save(&StorageLocation::from_command(name.into()).unwrap(), &nodecraft_format::encode(&snapshot(true)).unwrap()).unwrap();
    let session = Session::with_storage(snapshot(false), Box::new(storage.clone())).unwrap();
    (session, storage)
}
fn open(session: &mut Session) -> serde_json::Value {
    session.dispatch("file.open", &json!({"location":"old.ncraft", "discard_dirty":true})).unwrap()
}
fn recovered(storage: &Store) -> Snapshot {
    nodecraft_format::journal::recover(&storage.0.lock().unwrap().memory.recovery).unwrap().snapshot
}
fn saved(storage: &Store, name: &str) -> Snapshot {
    nodecraft_format::decode(storage.0.lock().unwrap().memory.files.get(&StorageLocation::from_command(name.into()).unwrap()).unwrap()).unwrap()
}

#[test]
fn legacy_files_open_with_separate_project_totals_and_one_replayable_undo_step() {
    let (mut a, store_a) = setup();
    let (mut b, _) = setup_named("other.ncraft");
    let receipt = open(&mut a);
    b.dispatch("file.open", &json!({"location":"other.ncraft", "discard_dirty":true})).unwrap();
    let (a_id, b_id) = (a.document().id(), b.document().id());
    assert_ne!(a_id, LEGACY);
    assert_ne!(a_id, b_id);
    assert_eq!(receipt["identity_migrated"], json!({"from":LEGACY,"to":a_id}));
    assert!(a.file_status().dirty, "save persists the migrated identity into the source file");
    assert_eq!(saved(&store_a, "old.ncraft").project.id(), LEGACY, "opening does not overwrite the source");
    assert_eq!(recovered(&store_a).project.id(), a_id, "crash recovery already owns the new identity");

    let mut log = UsageLog::default();
    for (id, key, micros) in [(LEGACY, "older", 50), (a_id, "a", 10), (b_id, "b", 20)] {
        log.record(UsageEntry {
            at_ms: 0,
            session: "now".into(),
            node: common::node_id(1),
            provider: "mock".into(),
            instance: None,
            project: Some(id),
            model: "fixture".into(),
            cost: Money { currency: Currency::USD, micros },
            idempotency_key: key.into(),
            status: Charge::Actual,
        })
        .unwrap();
    }
    assert_eq!(log.spent_in("now", Some(a_id)).this_project.get(&Currency::USD).unwrap().micros, 10);
    assert_eq!(log.spent_in("now", Some(b_id)).this_project.get(&Currency::USD).unwrap().micros, 20);
    assert_eq!(log.spent_in("now", Some(a_id)).project.get(&Currency::USD).unwrap().micros, 80);
    assert_eq!(log.spent_in("now", Some(LEGACY)).this_project.get(&Currency::USD).unwrap().micros, 50, "old charges are not guessed or lost");

    a.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(a.document().id(), LEGACY);
    assert!(!a.can_undo(), "migration is exactly one undo step");
    assert_eq!(recovered(&store_a).project.id(), LEGACY);
    a.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(a.document().id(), a_id);
    assert_eq!(recovered(&store_a).project.id(), a_id);
    a.dispatch("file.save", &json!({})).unwrap();
    assert_eq!(saved(&store_a, "old.ncraft").project.id(), a_id);
    let reopened = open(&mut a);
    assert!(reopened.get("identity_migrated").is_none());
    assert_eq!(a.document().id(), a_id);
    assert!(!a.can_undo());
}

#[test]
fn migration_checkpoint_failure_preserves_the_previous_document_and_recovery() {
    let (mut session, store) = setup();
    let before = session.document().clone();
    let recovery = store.0.lock().unwrap().memory.recovery.clone();
    let status = session.file_status();
    store.0.lock().unwrap().fail_checkpoint = true;
    assert!(session.dispatch("file.open", &json!({"location":"old.ncraft","discard_dirty":true})).is_err());
    assert_eq!(session.document(), &before);
    assert_eq!(session.file_status(), status);
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
    assert_eq!(store.0.lock().unwrap().memory.recovery, recovery);
}

#[test]
fn recovered_legacy_projects_migrate_but_joining_a_live_project_keeps_its_identity() {
    let (mut session, store) = setup();
    session.recover(snapshot(true), Some(StorageLocation::from_command("old.ncraft".into()).unwrap())).unwrap();
    let id = session.document().id();
    assert_ne!(id, LEGACY);
    assert!(session.can_undo());
    assert_eq!(recovered(&store).project.id(), id);
    let (mut joined, _) = setup();
    joined.adopt_joined(snapshot(true).project).unwrap();
    assert_eq!(joined.document().id(), LEGACY, "a peer cannot rename the shared room");
    assert!(!joined.can_undo());
}

#[test]
fn save_as_keeps_identity_and_storage_constructors_migrate_local_snapshots() {
    let storage = Store::default();
    let mut session =
        Session::with_storage_origin(snapshot(true), Box::new(storage.clone()), StorageLocation::from_command("source.ncraft".into()).unwrap()).unwrap();
    let id = session.document().id();
    assert_ne!(id, LEGACY);
    assert!(session.can_undo());
    for name in ["first.ncraft", "copy.ncraft"] {
        session.dispatch("file.save_as", &json!({"location":name})).unwrap();
        assert_eq!(session.document().id(), id);
        assert_eq!(saved(&storage, name).project.id(), id);
    }
    let reopened = Session::with_storage(recovered(&storage), Box::<MemoryStorage>::default()).unwrap();
    assert_eq!(reopened.document().id(), id);
    assert!(!reopened.can_undo());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn background_open_publishes_the_migration_only_after_its_checkpoint_succeeds() {
    #[derive(Default)]
    struct Held(Mutex<Option<nodecraft_engine::save_runner::SaveJob>>);
    impl nodecraft_engine::save_runner::SaveRunner for Held {
        fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
            *self.0.lock().unwrap() = Some(job);
            Ok(())
        }
    }
    for fail in [false, true] {
        let (_, store) = setup();
        let mut session = Session::with_sendable_storage(snapshot(false), Box::new(store.clone())).unwrap();
        let runner = Arc::new(Held::default());
        session.set_save_runner(runner.clone());
        let original = session.document().clone();
        let recovery = store.0.lock().unwrap().memory.recovery.clone();
        let result = session.dispatch("file.open", &json!({"location":"old.ncraft","discard_dirty":true,"background":true})).unwrap();
        assert_eq!(result, json!({"opened":false,"opening":true}));
        assert_eq!(session.document(), &original);
        assert!(!session.can_undo());
        store.0.lock().unwrap().fail_checkpoint = fail;
        runner.0.lock().unwrap().take().unwrap().run();
        let outcome = session.poll_file().unwrap();
        if fail {
            assert!(outcome.is_err());
            assert_eq!(session.document(), &original);
            assert_eq!(store.0.lock().unwrap().memory.recovery, recovery);
            assert!(!session.can_undo());
        } else {
            assert_eq!(outcome.unwrap()["identity_migrated"]["to"], json!(session.document().id()));
            assert_ne!(session.document().id(), LEGACY);
            assert!(session.can_undo());
            assert!(session.file_status().dirty);
            assert_eq!(recovered(&store).project, *session.document());
        }
    }
}

#[test]
fn legacy_reopen_discard_and_read_only_keep_the_project_cap() {
    use nodecraft_exec::cost::{self, Block, Budgets, Quote, Scope};
    for read_only in [false, true] {
        let (mut session, store) = setup();
        store.0.lock().unwrap().read_only = read_only;
        open(&mut session);
        let id = session.document().id();
        let mut log = UsageLog::default();
        log.record(UsageEntry {
            at_ms: 0,
            session: "previous".into(),
            node: common::node_id(1),
            provider: "mock".into(),
            instance: None,
            project: Some(id),
            model: "fixture".into(),
            cost: Money::new(Currency::USD, 100),
            idempotency_key: "spent".into(),
            status: Charge::Actual,
        })
        .unwrap();
        let budgets = Budgets { this_project: Some(Money::new(Currency::USD, 100)), ..Default::default() };
        let quote = Quote { steps: vec![], totals: [(Currency::USD, Money::new(Currency::USD, 1))].into() };
        for _ in 0..3 {
            open(&mut session);
            assert_eq!(session.document().id(), id);
            assert_eq!(session.file_status().read_only, read_only);
            let admission = cost::check(&quote, &budgets, &log.spent_in("next", Some(session.document().id()))).unwrap();
            assert!(matches!(admission.blocks.as_slice(), [Block::Over { scope: Scope::ThisProject, .. }]));
        }
        let mut second = Session::with_storage(snapshot(false), Box::new(store.clone())).unwrap();
        open(&mut second);
        assert_eq!(second.document().id(), id, "another window or process uses the same scope");
        assert_eq!(saved(&store, "old.ncraft").project.id(), LEGACY, "no source save required");
    }
}

#[test]
fn recovery_and_located_constructor_agree_and_unlocated_snapshots_stay_conservative() {
    let location = StorageLocation::from_command("old.ncraft".into()).unwrap();
    let (mut opened, _) = setup();
    open(&mut opened);
    let (mut recovered, _) = setup();
    recovered.recover(snapshot(true), Some(location.clone())).unwrap();
    let constructed = Session::with_storage_origin(snapshot(true), Box::<MemoryStorage>::default(), location).unwrap();
    assert_eq!(opened.document().id(), recovered.document().id());
    assert_eq!(opened.document().id(), constructed.document().id());
    for _ in 0..2 {
        let unlocated = Session::with_storage(snapshot(true), Box::<MemoryStorage>::default()).unwrap();
        assert_eq!(unlocated.document().id(), LEGACY, "unknown origins must not reset spending");
        assert!(!unlocated.can_undo());
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_legacy_aliases_read_only_and_distinct_files_have_stable_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.ncraft");
    let other = dir.path().join("other.ncraft");
    let bytes = nodecraft_format::encode(&snapshot(true)).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(&other, &bytes).unwrap();
    let original = std::fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&path, readonly).unwrap();
    let storage = nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap();
    let mut session = Session::with_storage(snapshot(false), Box::new(storage)).unwrap();
    session.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
    let id = session.document().id();
    assert!(session.file_status().read_only);
    let alias = dir.path().join(".").join("legacy.ncraft");
    session.dispatch("file.open", &json!({"location":alias,"discard_dirty":true})).unwrap();
    assert_eq!(session.document().id(), id);
    let located = Session::with_storage_origin(snapshot(true), Box::<MemoryStorage>::default(), StorageLocation::Path(path.canonicalize().unwrap())).unwrap();
    assert_eq!(located.document().id(), id, "native and canonical headless origins agree");
    session.dispatch("file.open", &json!({"location":other,"discard_dirty":true})).unwrap();
    assert_ne!(session.document().id(), id, "identical legacy bytes at separate locations do not pool");
    drop(session);
    std::fs::set_permissions(&path, original).unwrap();
}
