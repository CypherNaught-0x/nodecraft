//! Background saves (#461): the file work runs on the host's save runner, and every later
//! change waits for it, so the file holds exactly the state the save snapshotted.
mod common;
use nodecraft_engine::Session;
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_format::{AssetStore, MemoryStorage, ProjectStorage, Snapshot, StorageLocation, decode};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::sync::{Arc, Mutex, mpsc};

#[derive(Default)]
struct Faults {
    memory: MemoryStorage,
    save: bool,
    panic: bool,
    saves: usize,
}
#[derive(Clone, Default)]
struct Storage(Arc<Mutex<Faults>>);
impl ProjectStorage for Storage {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.lock().unwrap().memory.load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        let mut f = self.0.lock().unwrap();
        if f.panic {
            drop(f);
            panic!("injected storage panic");
        }
        if f.save {
            return Err(Error::new(ErrorKind::Conflict, "injected storage failure"));
        }
        f.saves += 1;
        f.memory.save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.append(b)
    }
}

/// Holds each job until the test runs it, as a slow disk would.
#[derive(Default)]
struct Gate(Mutex<Vec<SaveJob>>);
impl SaveRunner for Gate {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}
impl Gate {
    fn release(&self) {
        let jobs = std::mem::take(&mut *self.0.lock().unwrap());
        jobs.into_iter().for_each(SaveJob::run);
    }
}
/// Runs each job on a thread once the test says so, so a waiting edit really waits.
struct Thread(Mutex<Option<mpsc::Receiver<()>>>);
impl SaveRunner for Thread {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        let go = self.0.lock().unwrap().take().unwrap();
        std::thread::spawn(move || {
            go.recv().unwrap();
            job.run();
        });
        Ok(())
    }
}
struct Refuses;
impl SaveRunner for Refuses {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        Err(job)
    }
}
struct Drops;
impl SaveRunner for Drops {
    fn spawn(&self, _: SaveJob) -> Result<(), SaveJob> {
        Ok(())
    }
}

const FILE: &str = "/projects/a.ncraft";
fn setup(runner: Arc<dyn SaveRunner>) -> (Session, Storage) {
    let storage = Storage::default();
    let snapshot = Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(storage.clone())).unwrap();
    session.set_save_runner(runner);
    (session, storage)
}
fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":x,"y":0}})
}
fn saved(storage: &Storage) -> Option<Snapshot> {
    let bytes = storage.0.lock().unwrap().memory.files.get(&StorageLocation::from_command(FILE.into()).unwrap()).cloned()?;
    Some(decode(&bytes).unwrap())
}
fn background_save_as(session: &mut Session) -> serde_json::Value {
    session.dispatch("file.save_as", &json!({"location": FILE, "background": true})).unwrap()
}

#[test]
fn a_background_save_returns_at_once_and_publishes_when_its_job_finishes() {
    let gate = Arc::new(Gate::default());
    let (mut session, storage) = setup(gate.clone());
    session.dispatch("node.move", &movement(10)).unwrap();
    let revision = session.status_revision();
    assert_eq!(background_save_as(&mut session), json!({"saved": false, "saving": true, "recovery_ready": false, "read_only": false}));
    let status = session.file_status();
    assert!(status.saving && status.dirty && status.location.is_none(), "nothing is published yet: {status:?}");
    assert!(session.status_revision() > revision, "hosts see the save start");
    assert!(saved(&storage).is_none());
    assert!(session.poll_save().is_none(), "still writing");

    gate.release();
    let revision = session.status_revision();
    assert_eq!(session.poll_save().unwrap().unwrap(), json!({"saved": true, "recovery_ready": true, "read_only": false}));
    let status = session.file_status();
    assert!(!status.saving && !status.dirty && status.location.is_some(), "{status:?}");
    assert!(session.status_revision() > revision, "hosts see the save end");
    assert_eq!(&saved(&storage).unwrap().project, session.document());
    assert!(session.poll_save().is_none(), "reported once");
    // The session keeps its storage: the next save and edits work as before.
    session.dispatch("node.move", &movement(20)).unwrap();
    assert_eq!(session.dispatch("file.save", &json!({})).unwrap()["saved"], true);
    assert_eq!(&saved(&storage).unwrap().project, session.document());
}

#[test]
fn an_edit_waits_for_the_save_which_holds_the_state_before_the_edit() {
    let (go, wait) = mpsc::channel();
    let (mut session, storage) = setup(Arc::new(Thread(Mutex::new(Some(wait)))));
    session.dispatch("node.move", &movement(10)).unwrap();
    let before = session.document().clone();
    background_save_as(&mut session);
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        go.send(()).unwrap();
    });
    // Blocks until the save thread is released and has published.
    session.dispatch("node.move", &movement(30)).unwrap();
    release.join().unwrap();
    assert_eq!(saved(&storage).unwrap().project, before, "the file holds the snapshot, not the later edit");
    let status = session.file_status();
    assert!(!status.saving && status.dirty && status.location.is_some(), "saved, then edited: {status:?}");
    assert_eq!(session.poll_save().unwrap().unwrap()["saved"], true, "the outcome the edit waited for is kept for the host");
    assert!(session.can_undo());
}

#[test]
fn reads_do_not_wait_but_saves_and_quitting_do() {
    let gate = Arc::new(Gate::default());
    let (mut session, storage) = setup(gate.clone());
    session.dispatch("node.move", &movement(10)).unwrap();
    background_save_as(&mut session);
    // A read-only command answers while the save is still writing.
    session.dispatch("perf.report", &json!({})).unwrap();
    assert!(session.file_status().saving);
    gate.release();
    // A second save first finishes the one in flight.
    session.dispatch("node.move", &movement(20)).unwrap();
    background_save_as(&mut session);
    gate.release();
    // Waits for the second save even though the first one's outcome is still unreported.
    assert_eq!(session.finish_save().unwrap().unwrap()["saved"], true);
    assert!(!session.file_status().saving);
    assert_eq!(storage.0.lock().unwrap().saves, 2);
    assert!(session.finish_save().is_none());
}

#[test]
fn a_failed_or_panicking_background_save_reports_and_keeps_the_storage() {
    for panic in [false, true] {
        let gate = Arc::new(Gate::default());
        let (mut session, storage) = setup(gate.clone());
        session.dispatch("node.move", &movement(10)).unwrap();
        {
            let mut faults = storage.0.lock().unwrap();
            faults.save = !panic;
            faults.panic = panic;
        }
        background_save_as(&mut session);
        gate.release();
        let error = session.poll_save().unwrap().unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Conflict | ErrorKind::Internal), "{error:?}");
        let status = session.file_status();
        assert!(!status.saving && status.dirty && status.location.is_none(), "nothing was published: {status:?}");
        // The storage came back: once the disk recovers, editing and saving work.
        {
            let mut faults = storage.0.lock().unwrap();
            faults.save = false;
            faults.panic = false;
        }
        session.dispatch("node.move", &movement(20)).unwrap();
        background_save_as(&mut session);
        gate.release();
        assert_eq!(session.poll_save().unwrap().unwrap()["saved"], true);
        assert_eq!(&saved(&storage).unwrap().project, session.document());
    }
}

#[test]
fn saves_run_inline_without_a_thread_or_a_sendable_storage() {
    // A runner that can't start a thread hands the job back.
    let (mut session, storage) = setup(Arc::new(Refuses));
    session.dispatch("node.move", &movement(10)).unwrap();
    assert_eq!(background_save_as(&mut session)["saved"], true);
    assert!(!session.file_status().saving && !session.file_status().dirty);
    assert!(session.poll_save().is_none());
    assert_eq!(&saved(&storage).unwrap().project, session.document());

    // Without `background`, the save is inline even with a runner.
    let gate = Arc::new(Gate::default());
    let (mut session, _) = setup(gate.clone());
    assert_eq!(session.dispatch("file.save_as", &json!({"location": FILE})).unwrap()["saved"], true);
    assert!(gate.0.lock().unwrap().is_empty());

    // A storage that isn't `Send` always saves inline.
    let snapshot = Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_storage(snapshot, Box::new(MemoryStorage::default())).unwrap();
    session.set_save_runner(gate.clone());
    assert_eq!(background_save_as(&mut session)["saved"], true);
    assert!(gate.0.lock().unwrap().is_empty());
}

#[test]
fn a_job_dropped_unrun_is_reported_and_refuses_further_edits() {
    let (mut session, _) = setup(Arc::new(Drops));
    session.dispatch("node.move", &movement(10)).unwrap();
    background_save_as(&mut session);
    let error = session.poll_save().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(session.file_status().recovery_required);
    assert!(session.dispatch("node.move", &movement(20)).is_err(), "no edit can be journalled any more");
    assert!(session.dispatch("file.save_as", &json!({"location": FILE})).is_err());
}

#[test]
fn background_is_a_typed_save_parameter() {
    let (mut session, _) = setup(Arc::new(Refuses));
    assert_eq!(session.dispatch("file.save_as", &json!({"location": FILE, "background": "yes"})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(session.dispatch("file.save", &json!({"background": true, "other": 1})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(session.dispatch("file.save_as", &json!({"location": FILE})).unwrap()["saved"], true);
    assert_eq!(session.dispatch("file.save", &json!({"background": true})).unwrap()["saved"], true);
}

/// Starts a background save whose job runs 30 ms later on another thread.
fn slow_save() -> (Session, std::thread::JoinHandle<()>) {
    let (go, wait) = mpsc::channel();
    let (mut session, _) = setup(Arc::new(Thread(Mutex::new(Some(wait)))));
    session.dispatch("node.move", &movement(10)).unwrap();
    background_save_as(&mut session);
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        go.send(()).unwrap();
    });
    (session, release)
}

#[test]
fn a_drop_during_a_save_waits_for_it_instead_of_failing() {
    // A drop prepared while the save is writing must not be rejected when the save publishes.
    let (mut session, release) = slow_save();
    let image = nodecraft_media::encode_png(2, 2, &[127; 16], &Default::default()).unwrap();
    let files = [nodecraft_engine::import::Incoming { name: "image.png", bytes: &image }];
    let at = nodecraft_graph::Position { x: 0.0, y: 0.0 };
    let imported = nodecraft_engine::import::import_files(&mut session, common::canvas(), at, 100.0, &files, &[common::node_id(9)]).unwrap();
    release.join().unwrap();
    assert_eq!(imported.nodes, vec![common::node_id(9)]);
    assert_eq!(session.poll_save().unwrap().unwrap()["saved"], true);
}
