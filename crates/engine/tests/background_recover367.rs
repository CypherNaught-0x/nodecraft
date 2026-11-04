//! Recovering a kept journal's work on the save runner (#367): the media copies and the
//! checkpoint run off the calling thread under one owner, every other change is refused as busy
//! meanwhile, and a cancel or failure before the checkpoint publishes keeps the current project
//! and its journal byte for byte. Also the cancel bounds the issue asks for: a cancel lands at
//! the next chunk boundary, never inside a blocking read, and a partial journal append latches
//! until a checkpoint repairs it.
use crate::common;
use nodecraft_engine::Session;
use nodecraft_engine::file_cmds::{FileOperation, FileOperationKind, FilePhase};
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_format::{
    AssetContext, AssetStore, MemoryStorage, NativeAssetWorkspace, NativeStorage, ProjectStorage, Snapshot, StorageLocation,
    asset_source::{AssetRange, AssetSource, MemorySource, StreamMetrics, TransferCancel},
    disk_quota::DiskQuota,
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, Error, ErrorKind, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

/// Holds each job until the test releases it, as a slow disk would.
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

/// Memory storage whose checkpoint or append can fail, or whose append writes only part of its
/// frame before failing, as a full or failing recovery drive would.
#[derive(Default)]
struct Faults {
    memory: MemoryStorage,
    fail_checkpoint: bool,
    partial_append: bool,
    checkpoints: usize,
}
#[derive(Clone, Default)]
struct Flaky(Arc<Mutex<Faults>>);
impl ProjectStorage for Flaky {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.lock().unwrap().memory.load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        let mut faults = self.0.lock().unwrap();
        if faults.fail_checkpoint {
            return Err(Error::new(ErrorKind::Internal, "injected recovery write failure"));
        }
        faults.checkpoints += 1;
        faults.memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        let mut faults = self.0.lock().unwrap();
        if faults.partial_append {
            // Half the frame reaches the journal, then the drive fails.
            let half = b.get(..b.len() / 2).unwrap_or_default().to_vec();
            faults.memory.recovery.extend_from_slice(&half);
            return Err(Error::new(ErrorKind::Internal, "injected partial append"));
        }
        faults.memory.append(b)
    }
}
impl Flaky {
    fn journal(&self) -> Vec<u8> {
        self.0.lock().unwrap().memory.recovery.clone()
    }
}

fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":x,"y":0}})
}
fn memory_session(runner: Arc<dyn SaveRunner>) -> (Session, Flaky) {
    let storage = Flaky::default();
    let snapshot = Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(storage.clone())).unwrap();
    session.set_save_runner(runner);
    (session, storage)
}
/// Work an earlier session left in its journal: the test project, retitled and moved.
fn recovered() -> Snapshot {
    let mut project = common::project();
    Change::SetProjectTitle { title: "Recovered".into() }.apply(&mut project).unwrap();
    Snapshot { project, assets: AssetStore::default(), epoch: 3 }
}
fn operation(session: &Session) -> Option<FileOperation> {
    session.file_status().operation
}
fn busy(error: &Error) -> bool {
    error.kind == ErrorKind::Busy && error.message.contains("file.cancel")
}

#[test]
fn a_background_recovery_runs_on_the_runner_and_refuses_changes_until_it_publishes() {
    let gate = Arc::new(Gate::default());
    let (mut session, storage) = memory_session(gate.clone());
    let current = session.document().clone();
    let journal = storage.journal();
    let revision = session.status_revision();
    let location = StorageLocation::from_command("/projects/recovered.ncraft".into()).unwrap();
    let started = session.recover_in_background(recovered(), Some(location.clone())).unwrap();
    assert_eq!(started, json!({"recovered": false, "recovering": true}));
    assert!(session.status_revision() > revision, "hosts see it start");
    assert_eq!(operation(&session), Some(FileOperation { kind: FileOperationKind::Recover, phase: FilePhase::Preparing, cancel_requested: false }));
    // Nothing is replaced yet, and nothing may change the project it is about to replace.
    assert_eq!(session.document(), &current);
    assert_eq!(storage.journal(), journal, "the journal still holds the current project");
    for (id, params) in [
        ("node.move", movement(30)),
        ("file.save_as", json!({"location": "/projects/other.ncraft"})),
        ("file.open", json!({"location": "/projects/other.ncraft", "discard_dirty": true})),
    ] {
        let error = session.dispatch(id, &params).unwrap_err();
        assert!(busy(&error), "{id}: {error:?}");
    }
    let again = session.recover_in_background(recovered(), None).unwrap_err();
    assert!(busy(&again), "one operation owns the storage: {again:?}");
    assert!(session.import_asset(b"late").is_err_and(|error| busy(&error)));
    // Reads still answer.
    session.dispatch("perf.report", &json!({})).unwrap();
    assert!(session.poll_file().is_none(), "still running");

    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap(), json!({"recovered": true, "read_only": false}));
    assert_eq!(session.document(), &recovered().project);
    assert!(!session.can_undo() && !session.can_redo(), "history starts fresh");
    let status = session.file_status();
    assert!(status.dirty && !status.recovery_required && status.operation.is_none(), "{status:?}");
    assert_eq!(status.location, Some(location));
    assert_eq!(storage.0.lock().unwrap().checkpoints, 2, "the session's first checkpoint, then the recovery's");
    let replayed = nodecraft_format::journal::recover(&storage.journal()).unwrap();
    assert_eq!(replayed.snapshot.project, recovered().project, "the journal now holds the recovered work");
    // The recovered work edits and journals as any project does.
    session.dispatch("node.move", &movement(40)).unwrap();
    assert_eq!(nodecraft_format::journal::recover(&storage.journal()).unwrap().snapshot.project, *session.document());
}

#[test]
fn a_recovery_cancelled_or_failing_before_its_checkpoint_keeps_the_project_and_its_journal() {
    for fail in [false, true] {
        let gate = Arc::new(Gate::default());
        let (mut session, storage) = memory_session(gate.clone());
        let current = session.document().clone();
        let lineage = session.document_lineage();
        let journal = storage.journal();
        session.recover_in_background(recovered(), None).unwrap();
        if fail {
            storage.0.lock().unwrap().fail_checkpoint = true;
        } else {
            assert_eq!(session.dispatch("file.cancel", &json!({})).unwrap()["cancelling"], true);
            assert!(operation(&session).unwrap().cancel_requested);
        }
        gate.release();
        let error = session.poll_file().unwrap().unwrap_err();
        if fail {
            assert_eq!(error.message, "injected recovery write failure", "a failure keeps its own error");
        } else {
            assert_eq!(error.kind, ErrorKind::Conflict);
            assert!(error.message.starts_with("recovering was cancelled") && error.message.contains("unchanged"), "{}", error.message);
        }
        assert_eq!(session.document(), &current);
        assert!(session.is_current_document(&lineage), "the current project is still the one adopted");
        assert_eq!(storage.journal(), journal, "byte for byte");
        let status = session.file_status();
        assert!(!status.recovery_required && status.operation.is_none() && status.location.is_none(), "{status:?}");
        // The storage came back: the current project edits and journals on, and the same work can
        // be recovered again once the drive works.
        storage.0.lock().unwrap().fail_checkpoint = false;
        session.dispatch("node.move", &movement(50)).unwrap();
        assert_eq!(nodecraft_format::journal::recover(&storage.journal()).unwrap().snapshot.project, *session.document());
    }
}

#[test]
fn a_recovery_with_no_runner_or_a_refusing_one_runs_inline_through_the_same_path() {
    struct Refuses;
    impl SaveRunner for Refuses {
        fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
            Err(job)
        }
    }
    let (mut session, storage) = memory_session(Arc::new(Refuses));
    assert_eq!(session.recover_in_background(recovered(), None).unwrap(), json!({"recovered": true, "read_only": false}));
    assert_eq!(session.document(), &recovered().project);
    assert!(session.poll_file().is_none(), "the inline outcome was the answer");
    assert_eq!(nodecraft_format::journal::recover(&storage.journal()).unwrap().snapshot.project, recovered().project);
    // `recover` keeps its inline contract, and its refusals.
    let (mut plain, _) = memory_session(Arc::new(Gate::default()));
    plain.dispatch("node.move", &movement(10)).unwrap();
    assert_eq!(plain.recover(recovered(), None).unwrap_err().kind, ErrorKind::Conflict, "its own edits would be lost");
}

#[test]
fn a_cancel_that_arrives_after_the_checkpoint_published_reports_the_recovery() {
    let gate = Arc::new(Gate::default());
    let (mut session, _storage) = memory_session(gate.clone());
    session.recover_in_background(recovered(), None).unwrap();
    gate.release();
    // The job has finished (the checkpoint is the journal now); the host hasn't polled yet.
    assert_eq!(session.dispatch("file.cancel", &json!({})).unwrap()["cancelling"], true);
    assert_eq!(session.poll_file().unwrap().unwrap()["recovered"], true, "it can't be rolled back, and isn't claimed to be");
    assert_eq!(session.document(), &recovered().project);
}

#[test]
fn a_recovery_whose_job_is_dropped_unrun_says_so_and_never_journals_nowhere() {
    struct Drops;
    impl SaveRunner for Drops {
        fn spawn(&self, _: SaveJob) -> Result<(), SaveJob> {
            Ok(())
        }
    }
    let (mut session, _storage) = memory_session(Arc::new(Drops));
    let current = session.document().clone();
    session.recover_in_background(recovered(), None).unwrap();
    let error = session.poll_file().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.starts_with("recovering the work stopped"), "{}", error.message);
    assert_eq!(session.document(), &current);
    assert!(session.file_status().recovery_required);
    assert!(session.dispatch("node.move", &movement(10)).is_err(), "nothing is journalled without the storage");
}

#[test]
fn a_partial_append_latches_until_a_checkpoint_repairs_the_journal() {
    let (mut session, storage) = memory_session(Arc::new(Gate::default()));
    session.dispatch("node.move", &movement(10)).unwrap();
    let journalled = session.document().clone();
    storage.0.lock().unwrap().partial_append = true;
    assert!(session.dispatch("node.move", &movement(20)).is_err());
    assert_eq!(session.document(), &journalled, "the edit the journal didn't take isn't applied");
    assert!(session.file_status().recovery_required, "a partial append can't be trusted to continue");
    // The torn frame stays at the journal's end: replay stops before it and keeps what was whole.
    let replayed = nodecraft_format::journal::recover(&storage.journal()).unwrap();
    assert_eq!(replayed.snapshot.project, journalled);
    assert!(replayed.truncated_tail || replayed.damage.is_some(), "the torn tail is reported");
    // Every further edit is refused until a checkpoint, even once the drive works again.
    storage.0.lock().unwrap().partial_append = false;
    let refused = session.dispatch("node.move", &movement(30)).unwrap_err();
    assert!(refused.message.contains("checkpoint"), "{}", refused.message);
    // A save writes a new checkpoint over the torn journal; editing journals again.
    session.dispatch("file.save_as", &json!({"location": "/projects/repaired.ncraft"})).unwrap();
    assert!(!session.file_status().recovery_required);
    session.dispatch("node.move", &movement(40)).unwrap();
    let replayed = nodecraft_format::journal::recover(&storage.journal()).unwrap();
    assert!(replayed.damage.is_none() && !replayed.truncated_tail);
    assert_eq!(replayed.snapshot.project, *session.document());
}

const C: CanvasId = CanvasId::from_bytes([6; 16]);
fn media(hash: Blake3) -> Node {
    let mut node = Node::new(NodeId::from_bytes([9; 16]), "source.text");
    node.params.insert("content".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    node
}
fn native_session(journal: &std::path::Path) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([6; 16]), "Native").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let storage = NativeStorage::new(journal.to_path_buf()).unwrap();
    Session::with_sendable_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap()
}

#[test]
fn native_work_recovered_in_the_background_holds_its_media_after_the_kept_copy_goes() {
    let dir = tempfile::tempdir().unwrap();
    let kept = dir.path().join("kept.journal");
    let hash = {
        let mut earlier = native_session(&kept);
        let hash = earlier.import_asset(&vec![7u8; 300 * 1024]).unwrap();
        earlier.dispatch("node.add", &json!({"canvas": C, "node": media(hash)})).unwrap();
        hash
        // A crash: the session ends without closing its journal.
    };
    let found = NativeStorage::new(kept.clone()).unwrap().recover().unwrap();
    let expected = found.snapshot.project.clone();
    let journal = dir.path().join("session.journal");
    let mut session = native_session(&journal);
    session.set_save_runner(Arc::new(Threads));
    let file = StorageLocation::Path(dir.path().join("a.ncraft"));
    assert_eq!(session.recover_in_background(found.snapshot, Some(file.clone())).unwrap()["recovering"], true);
    assert_eq!(session.finish_file().unwrap().unwrap()["recovered"], true);
    assert_eq!(session.document(), &expected);
    // The kept copy goes (the host removes it once the work is saved; here, before): the
    // session's own workspace and journal hold the media.
    std::fs::remove_file(&kept).unwrap();
    let lease = session.assets().verified_content(&hash, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(lease.byte_len(), 300 * 1024);
    // The live journal is locked by this session (and leases on its media); replay it once they
    // have gone.
    drop(lease);
    drop(session);
    let replayed = NativeStorage::new(journal).unwrap().recover().unwrap();
    assert_eq!(replayed.snapshot.project, expected);
    assert_eq!(replayed.snapshot.assets.verified_content(&hash, &TransferCancel::default(), &StreamMetrics::default()).unwrap().byte_len(), 300 * 1024);
}

/// A source whose reads can be held mid-stream, as a slow or stalled drive's would, and which
/// counts them and remembers the largest one asked for.
struct Slow {
    bytes: Arc<[u8]>,
    reads: AtomicUsize,
    largest: AtomicUsize,
    hold: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}
impl Slow {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self { bytes: Arc::from(bytes), reads: AtomicUsize::new(0), largest: AtomicUsize::new(0), hold: Mutex::new(None) })
    }
}
impl AssetSource for Slow {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.largest.fetch_max(buffer.len(), Ordering::SeqCst);
        let hold = self.hold.lock().unwrap().take();
        if let Some((entered, go)) = hold {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        MemorySource::new(Arc::clone(&self.bytes)).read_at(offset, buffer)
    }
}

/// The streaming copy every file operation's body passes go through (`import_stream`: a
/// recovery's media copies into this workspace, an import, a classic open's verified copies)
/// reads in bounded chunks and checks the cancel between them. So `file.cancel` stops it at the
/// next chunk, never inside a blocking read: one already under way finishes first.
#[test]
fn a_cancel_lands_at_the_next_chunk_and_never_inside_a_blocking_read() {
    let dir = tempfile::tempdir().unwrap();
    let length = 4 * 1024 * 1024;
    let body: Vec<u8> = (0..length as u32).map(|n| (n % 251) as u8).collect();
    let workspace = |name: &str| {
        let folder = dir.path().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        AssetContext::native(NativeAssetWorkspace::new(&folder, DiskQuota::default()).unwrap())
    };
    // Uncancelled, the copy reads the whole body in many bounded chunks.
    let whole = Slow::new(body.clone());
    let mut store = AssetStore::with_context(workspace("whole"));
    store.import_stream(&AssetRange::new(whole.clone(), 0, length as u64).unwrap(), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let (reads, largest) = (whole.reads.load(Ordering::SeqCst), whole.largest.load(Ordering::SeqCst));
    assert!(reads >= 4 && largest <= 1024 * 1024, "{reads} reads, the largest {largest} bytes");

    // Stalled in its first read, then cancelled.
    let slow = Slow::new(body);
    let (entered, held) = mpsc::channel();
    let (go, wait) = mpsc::channel();
    *slow.hold.lock().unwrap() = Some((entered, wait));
    let cancel = TransferCancel::default();
    let copy = {
        let (slow, cancel) = (slow.clone(), cancel.clone());
        let context = workspace("stalled");
        std::thread::spawn(move || {
            let mut store = AssetStore::with_context(context);
            let result = store.import_stream(&AssetRange::new(slow, 0, length as u64).unwrap(), &cancel, &StreamMetrics::default());
            (result, store.len())
        })
    };
    held.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    cancel.cancel();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!copy.is_finished(), "the blocking read isn't interrupted, and isn't claimed to be");
    let at_cancel = slow.reads.load(Ordering::SeqCst);
    go.send(()).unwrap();
    let (result, stored) = copy.join().unwrap();
    let error = result.unwrap_err();
    assert!(TransferCancel::is_cancellation(&error), "{error:?}");
    assert_eq!(slow.reads.load(Ordering::SeqCst), at_cancel, "the next chunk boundary stopped it: no further read");
    assert_eq!(stored, 0, "nothing half-copied is adopted");
}

/// A recovery is checked against the recovery drive before anything is written, titled or not
/// (#367): the media it copies into this workspace and its checkpoint.
#[test]
fn a_recovery_that_the_recovery_drive_cannot_hold_is_refused_before_it_writes() {
    let dir = tempfile::tempdir().unwrap();
    let kept = dir.path().join("kept.journal");
    {
        let mut earlier = native_session(&kept);
        let hash = earlier.import_asset(&vec![5u8; 300 * 1024]).unwrap();
        earlier.dispatch("node.add", &json!({"canvas": C, "node": media(hash)})).unwrap();
    }
    let free = Arc::new(std::sync::atomic::AtomicU64::new(64 * 1024));
    let report: nodecraft_format::SpaceReport = {
        let free = Arc::clone(&free);
        Arc::new(move |_: &StorageLocation| nodecraft_format::space::SpaceProbe { recovery_free: Some(free.load(Ordering::SeqCst)), ..Default::default() })
    };
    let journal = dir.path().join("session.journal");
    let mut storage = NativeStorage::new(journal.clone()).unwrap();
    storage.set_space_probe(report);
    let mut project = Project::new(ProjectId::from_bytes([6; 16]), "Short").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::with_sendable_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    let gate = Arc::new(Gate::default());
    session.set_save_runner(gate.clone());
    let current = session.document().clone();
    for location in [None, Some(StorageLocation::Path(dir.path().join("a.ncraft")))] {
        let found = NativeStorage::new(kept.clone()).unwrap().recover().unwrap();
        let error = session.recover_in_background(found.snapshot, location).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy);
        assert!(error.message.contains("recover this work") && error.message.contains("unchanged"), "{}", error.message);
        assert!(!error.message.contains(dir.path().to_str().unwrap()), "sizes, never paths");
        assert!(gate.0.lock().unwrap().is_empty(), "nothing was started");
        assert_eq!(session.document(), &current);
        assert!(session.file_status().operation.is_none());
    }
    // With room, the same work recovers.
    free.store(1 << 40, Ordering::SeqCst);
    let found = NativeStorage::new(kept).unwrap().recover().unwrap();
    session.recover_in_background(found.snapshot, None).unwrap();
    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap()["recovered"], true);
}
