//! Background opens, cancellation and progress (#367): an open reads on the host's runner while
//! reads keep working and edits are refused, and `file.cancel` stops a save or open before it
//! publishes anything, or reports truthfully that it was too late.
mod common;
use nodecraft_engine::Session;
use nodecraft_engine::file_cmds::{FileOperation, FileOperationKind, FilePhase};
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_format::{AssetStore, MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::sync::{Arc, Mutex, mpsc};

/// Memory storage whose saves can be held mid-write, as a slow disk would.
#[derive(Default)]
struct Inner {
    memory: MemoryStorage,
    hold: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}
#[derive(Clone, Default)]
struct Storage(Arc<Mutex<Inner>>);
impl ProjectStorage for Storage {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.lock().unwrap().memory.load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        let hold = self.0.lock().unwrap().hold.take();
        if let Some((entered, go)) = hold {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        self.0.lock().unwrap().memory.save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.append(b)
    }
}
impl Storage {
    fn has(&self, location: &str) -> bool {
        self.0.lock().unwrap().memory.files.contains_key(&StorageLocation::from_command(location.into()).unwrap())
    }
}

/// Holds each job until the test runs it.
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
struct Threads;
impl SaveRunner for Threads {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        std::thread::spawn(move || job.run());
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
const OTHER: &str = "/projects/b.ncraft";
fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":x,"y":0}})
}
fn x(session: &Session) -> f32 {
    session.document().canvases()[&common::canvas()].nodes()[&common::node_id(1)].pos.x
}
/// A session whose project is saved at `FILE` with the node at x = 10, then moved to x = 20
/// and left unsaved.
fn setup(runner: Arc<dyn SaveRunner>) -> (Session, Storage) {
    let storage = Storage::default();
    let snapshot = Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(storage.clone())).unwrap();
    session.set_save_runner(runner);
    session.dispatch("node.move", &movement(10)).unwrap();
    session.dispatch("file.save_as", &json!({"location": FILE})).unwrap();
    session.dispatch("node.move", &movement(20)).unwrap();
    (session, storage)
}
fn background_open(session: &mut Session) -> serde_json::Value {
    session.dispatch("file.open", &json!({"location": FILE, "discard_dirty": true, "background": true})).unwrap()
}
fn operation(session: &Session) -> Option<FileOperation> {
    session.file_status().operation
}

#[test]
fn a_background_open_returns_at_once_keeps_reads_and_refuses_edits_until_it_finishes() {
    let gate = Arc::new(Gate::default());
    let (mut session, _) = setup(gate.clone());
    assert_eq!(background_open(&mut session), json!({"opened": false, "opening": true}));
    assert_eq!(operation(&session), Some(FileOperation { kind: FileOperationKind::Open, phase: FilePhase::Reading, cancel_requested: false }));
    assert!(!session.file_status().saving, "an open is not a save");
    assert_eq!(x(&session), 20.0, "the current project stays until the open publishes");
    assert!(session.dispatch("perf.report", &json!({})).is_ok(), "reads go on");
    for (id, params) in
        [("node.move", movement(30)), ("edit.undo", json!({})), ("file.save", json!({})), ("file.open", json!({"location": FILE, "discard_dirty": true}))]
    {
        let error = session.dispatch(id, &params).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy, "{id}");
        assert!(error.message.contains("file.cancel"), "{id}: {}", error.message);
    }
    assert_eq!(session.import_asset(b"bytes").unwrap_err().kind, ErrorKind::Busy, "imports would land on the project being replaced");
    assert!(session.poll_file().is_none(), "still reading");

    let revision = session.status_revision();
    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap(), json!({"opened": true, "read_only": false}));
    assert!(session.poll_file().is_none(), "reported once");
    assert_eq!(x(&session), 10.0, "the file's project replaced the unsaved one");
    assert!(!session.can_undo(), "history starts fresh");
    let status = session.file_status();
    assert_eq!(status.operation, None);
    assert!(!status.dirty);
    assert_eq!(status.location, Some(StorageLocation::from_command(FILE.into()).unwrap()));
    assert!(session.status_revision() > revision, "hosts see the status change");
    session.dispatch("node.move", &movement(40)).unwrap();
}

#[test]
fn cancelling_an_open_before_it_publishes_keeps_the_current_project() {
    let gate = Arc::new(Gate::default());
    let (mut session, _) = setup(gate.clone());
    background_open(&mut session);
    assert_eq!(session.dispatch("file.cancel", &json!({})).unwrap(), json!({"cancelling": true}));
    assert!(operation(&session).unwrap().cancel_requested);
    gate.release();
    let error = session.poll_file().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(error.message, "opening was cancelled; the current project is unchanged");
    assert_eq!(x(&session), 20.0);
    assert!(session.can_undo(), "the current project keeps its history");
    let status = session.file_status();
    assert!(status.dirty && !status.recovery_required && status.operation.is_none());
    // It still journals and saves.
    session.dispatch("node.move", &movement(30)).unwrap();
    assert_eq!(session.dispatch("file.save", &json!({})).unwrap()["saved"], true);
}

#[test]
fn cancelling_a_save_before_it_writes_leaves_the_file_unchanged() {
    let gate = Arc::new(Gate::default());
    let (mut session, storage) = setup(gate.clone());
    session.dispatch("file.save_as", &json!({"location": OTHER, "background": true})).unwrap();
    assert_eq!(operation(&session).unwrap().kind, FileOperationKind::Save);
    assert_eq!(session.dispatch("file.cancel", &json!(null)).unwrap(), json!({"cancelling": true}));
    gate.release();
    let error = session.poll_file().unwrap().unwrap_err();
    assert_eq!(error.message, "the save was cancelled; the file is unchanged");
    assert!(!storage.has(OTHER), "nothing was written");
    let status = session.file_status();
    assert!(status.dirty && status.operation.is_none());
    assert_eq!(status.location, Some(StorageLocation::from_command(FILE.into()).unwrap()), "still the old file");
    assert_eq!(session.dispatch("file.save_as", &json!({"location": OTHER})).unwrap()["saved"], true);
}

#[test]
fn a_cancel_after_the_save_published_reports_the_save() {
    let gate = Arc::new(Gate::default());
    let (mut session, storage) = setup(gate.clone());
    session.dispatch("file.save_as", &json!({"location": OTHER, "background": true})).unwrap();
    gate.release();
    // The job has finished but the session hasn't settled it: too late to cancel.
    assert_eq!(session.dispatch("file.cancel", &json!({})).unwrap(), json!({"cancelling": true}));
    assert_eq!(session.poll_file().unwrap().unwrap()["saved"], true);
    assert!(storage.has(OTHER));
    assert!(!session.file_status().dirty);
    assert_eq!(session.dispatch("file.cancel", &json!({})).unwrap(), json!({"cancelling": false}), "nothing left to cancel");
}

#[test]
fn a_held_save_reports_its_writing_phase_and_a_cancel_too_late_to_stop_it_reports_the_save() {
    let (mut session, storage) = setup(Arc::new(Threads));
    let (entered, wait) = mpsc::channel();
    let (go, held) = mpsc::channel();
    storage.0.lock().unwrap().hold = Some((entered, held));
    session.dispatch("file.save_as", &json!({"location": OTHER, "background": true})).unwrap();
    wait.recv().unwrap();
    assert_eq!(operation(&session).unwrap().phase, FilePhase::Writing);
    assert!(session.dispatch("perf.report", &json!({})).is_ok(), "the UI keeps working while the disk is slow");
    session.dispatch("file.cancel", &json!({})).unwrap();
    go.send(()).unwrap();
    // A legacy storage's write can't be interrupted once inside it, so this one publishes,
    // and the outcome says so rather than claiming a cancellation it couldn't do.
    assert_eq!(session.finish_file().unwrap().unwrap()["saved"], true);
    assert!(storage.has(OTHER));
}

#[test]
fn a_failed_background_open_keeps_the_current_project_and_its_journal() {
    let gate = Arc::new(Gate::default());
    let (mut session, _) = setup(gate.clone());
    session.dispatch("file.open", &json!({"location": "/projects/missing.ncraft", "discard_dirty": true, "background": true})).unwrap();
    gate.release();
    let error = session.poll_file().unwrap().unwrap_err();
    assert_ne!(error.kind, ErrorKind::Internal, "{}", error.message);
    assert_eq!(x(&session), 20.0);
    assert!(!session.file_status().recovery_required);
    session.dispatch("node.move", &movement(30)).unwrap();
}

#[test]
fn an_open_the_runner_refuses_runs_inline_and_one_it_drops_reports_the_loss() {
    let (mut session, _) = setup(Arc::new(Refuses));
    assert_eq!(background_open(&mut session), json!({"opened": true, "read_only": false}));
    assert_eq!(x(&session), 10.0);

    let (mut session, _) = setup(Arc::new(Drops));
    background_open(&mut session);
    let error = session.poll_file().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.starts_with("opening the project stopped"), "{}", error.message);
    assert!(session.file_status().recovery_required);
    assert!(session.dispatch("node.move", &movement(30)).is_err(), "nothing can journal without the storage");
}

#[test]
fn finishing_waits_for_a_background_open() {
    let (mut session, _) = setup(Arc::new(Threads));
    background_open(&mut session);
    assert_eq!(session.finish_file().unwrap().unwrap()["opened"], true);
    assert_eq!(x(&session), 10.0);
}

#[test]
fn cancel_and_background_open_are_typed_parameters() {
    let (mut session, _) = setup(Arc::new(Refuses));
    assert_eq!(session.dispatch("file.cancel", &json!({"now": true})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(session.dispatch("file.open", &json!({"location": FILE, "background": 1})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(session.dispatch("file.cancel", &json!({})).unwrap(), json!({"cancelling": false}));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn on_native_storage_a_cancelled_save_or_open_leaves_the_files_and_the_journal_as_they_were() {
    let dir = tempfile::tempdir().unwrap();
    let (old, next, recovery) = (dir.path().join("old.ncraft"), dir.path().join("next.ncraft"), dir.path().join("recovery"));
    let storage = nodecraft_format::NativeStorage::new(recovery.clone()).unwrap();
    let snapshot = Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(storage)).unwrap();
    let gate = Arc::new(Gate::default());
    session.set_save_runner(gate.clone());
    session.dispatch("node.move", &movement(10)).unwrap();
    session.dispatch("file.save_as", &json!({"location": old.to_str().unwrap()})).unwrap();
    session.dispatch("node.move", &movement(20)).unwrap();
    let journal = std::fs::read(&recovery).unwrap();
    let saved = std::fs::read(&old).unwrap();

    session.dispatch("file.save_as", &json!({"location": next.to_str().unwrap(), "background": true})).unwrap();
    session.dispatch("file.cancel", &json!({})).unwrap();
    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap_err().message, "the save was cancelled; the file is unchanged");
    assert!(!next.exists());
    assert_eq!(std::fs::read(&recovery).unwrap(), journal, "the journal still holds the unsaved edit");

    session.dispatch("file.open", &json!({"location": old.to_str().unwrap(), "discard_dirty": true, "background": true})).unwrap();
    session.dispatch("file.cancel", &json!({})).unwrap();
    gate.release();
    assert!(session.poll_file().unwrap().is_err());
    assert_eq!(std::fs::read(&recovery).unwrap(), journal);
    assert_eq!(x(&session), 20.0);

    // Uncancelled, both go through; the open replaces the journal with the file's checkpoint.
    session.dispatch("file.open", &json!({"location": old.to_str().unwrap(), "discard_dirty": true, "background": true})).unwrap();
    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap()["opened"], true);
    assert_eq!(x(&session), 10.0);
    assert_ne!(std::fs::read(&recovery).unwrap(), journal);
    assert_eq!(std::fs::read(&old).unwrap(), saved);
    session.dispatch("file.save_as", &json!({"location": next.to_str().unwrap(), "background": true})).unwrap();
    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap()["saved"], true);
    assert!(next.exists());
}
