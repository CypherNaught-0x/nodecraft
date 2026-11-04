//! Background persistence under faults (#367): a save whose file is published but whose recovery
//! checkpoint then fails, a cancel that arrives after publication, cleanup after a cancelled
//! save on native storage, recovery after the opened file is deleted, and the disk-space
//! inventory's housekeeping extents measured against a real save.
use crate::common;
use nodecraft_engine::Session;
use nodecraft_engine::file_cmds::{FileOperationKind, FilePhase};
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, CheckpointPlan, FramePlan, LoadedProject, MemoryStorage, NativeStorage, PreparedSnapshot, ProjectAccess,
    ProjectStorage, Snapshot, StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
    space::{SpaceInventory, SpaceProbe},
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, Error, ErrorKind, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex, mpsc};

/// Holds each job until the test releases it.
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

/// Memory storage whose checkpoint can fail, or be held while the test acts, as a full or slow
/// recovery drive would.
#[derive(Default)]
struct Faults {
    memory: MemoryStorage,
    fail_checkpoint: bool,
    hold_checkpoint: Option<Hold>,
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
        let hold = self.0.lock().unwrap().hold_checkpoint.take();
        if let Some((entered, go)) = hold {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        let mut faults = self.0.lock().unwrap();
        if faults.fail_checkpoint {
            return Err(Error::new(ErrorKind::Internal, "injected recovery write failure"));
        }
        faults.checkpoints += 1;
        faults.memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().memory.append(b)
    }
}
impl Flaky {
    fn file(&self, location: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().memory.files.get(&StorageLocation::from_command(location.into()).unwrap()).cloned()
    }
}

const FILE: &str = "/projects/faults.ncraft";
fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":x,"y":0}})
}
fn memory_session(runner: Arc<dyn SaveRunner>) -> (Session, Flaky) {
    let storage = Flaky::default();
    let snapshot = Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(storage.clone())).unwrap();
    session.set_save_runner(runner);
    session.dispatch("node.move", &movement(10)).unwrap();
    session.dispatch("file.save_as", &json!({"location": FILE})).unwrap();
    session.dispatch("node.move", &movement(20)).unwrap();
    (session, storage)
}

#[test]
fn a_background_save_that_publishes_and_then_fails_its_checkpoint_says_so_and_holds_the_latch() {
    let gate = Arc::new(Gate::default());
    let (mut session, storage) = memory_session(gate.clone());
    let edited = session.document().clone();
    let lineage = session.document_lineage();
    storage.0.lock().unwrap().fail_checkpoint = true;
    assert_eq!(session.dispatch("file.save", &json!({"background": true})).unwrap()["saving"], true);
    gate.release();
    let outcome = session.poll_file().unwrap().unwrap();
    // The file is the new one: the outcome says saved, and that recovery isn't ready.
    assert_eq!(outcome["saved"], true);
    assert_eq!(outcome["recovery_ready"], false);
    assert_eq!(nodecraft_format::decode(&storage.file(FILE).unwrap()).unwrap().project, edited);
    let status = session.file_status();
    assert!(!status.dirty, "the destination holds this state");
    assert!(status.recovery_required, "the recovery latch is held");
    assert_eq!(status.operation, None);
    assert!(session.has_saved_document(&lineage), "a failed checkpoint doesn't undo the save");
    // Edits can't journal against the stale checkpoint: refused, and nothing changes.
    let error = session.dispatch("node.move", &movement(30)).unwrap_err();
    assert!(error.message.contains("checkpoint"), "{}", error.message);
    assert_eq!(session.document(), &edited);
    // A later save with a working drive repairs recovery.
    storage.0.lock().unwrap().fail_checkpoint = false;
    session.dispatch("file.save", &json!({"background": true})).unwrap();
    gate.release();
    assert_eq!(session.poll_file().unwrap().unwrap()["recovery_ready"], true);
    assert!(!session.file_status().recovery_required);
    session.dispatch("node.move", &movement(30)).unwrap();
}

#[test]
fn a_cancel_after_the_file_is_published_cannot_report_a_rollback() {
    let (mut session, storage) = memory_session(Arc::new(Threads));
    let edited = session.document().clone();
    let (entered, held) = mpsc::channel();
    let (go, wait) = mpsc::channel();
    storage.0.lock().unwrap().hold_checkpoint = Some((entered, wait));
    session.dispatch("file.save", &json!({"background": true})).unwrap();
    held.recv().unwrap();
    // The destination is already replaced; the operation is checkpointing.
    let operation = session.file_status().operation.unwrap();
    assert_eq!((operation.kind, operation.phase), (FileOperationKind::Save, FilePhase::Checkpointing));
    assert_eq!(nodecraft_format::decode(&storage.file(FILE).unwrap()).unwrap().project, edited);
    session.dispatch("file.cancel", &json!({})).unwrap();
    assert!(session.file_status().operation.unwrap().cancel_requested);
    go.send(()).unwrap();
    let outcome = session.finish_file().unwrap().unwrap();
    assert_eq!(outcome["saved"], true, "the save happened; a late cancel can't claim the file is unchanged");
    assert_eq!(outcome["recovery_ready"], true, "the checkpoint wasn't cancelled either");
    assert!(!session.file_status().dirty);
    assert_eq!(storage.0.lock().unwrap().checkpoints, 3);
}

// --- Native storage -----------------------------------------------------------------------

const C: CanvasId = CanvasId::from_bytes([6; 16]);
fn media(n: usize, hash: Blake3) -> Node {
    let mut id = [9; 16];
    id[..8].copy_from_slice(&(n as u64).to_le_bytes());
    let mut node = Node::new(NodeId::from_bytes(id), "source.text");
    node.params.insert("content".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    node
}
fn native_session(journal: &std::path::Path) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([6; 16]), "Faults").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let storage = NativeStorage::new(journal.to_path_buf()).unwrap();
    Session::with_sendable_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap()
}
/// Imports `count` distinct assets of `bytes` each and places a node naming each one.
fn add_media(session: &mut Session, count: usize, bytes: usize, seed: u8) -> Vec<Blake3> {
    (0..count)
        .map(|n| {
            let mut body = vec![seed; bytes];
            body[..8].copy_from_slice(&(n as u64).to_le_bytes());
            let hash = session.import_asset(&body).unwrap();
            session.dispatch("node.add", &json!({"canvas": C, "node": media(n + usize::from(seed) * 1000, hash)})).unwrap();
            hash
        })
        .collect()
}
/// The project in the file at `path`, read by a session of its own.
fn read_back(dir: &std::path::Path, path: &std::path::Path) -> Project {
    let mut reader = native_session(&dir.join("read-back"));
    reader.dispatch("file.open", &json!({"location": path.to_str().unwrap(), "discard_dirty": true})).unwrap();
    let project = reader.document().clone();
    reader.close_recovery(true).unwrap();
    project
}
fn names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Signals the test that a write was reached, then waits for its go-ahead.
type Hold = (mpsc::Sender<()>, mpsc::Receiver<()>);
/// Native storage whose save is held at the start of writing the destination, after the archive
/// was prepared in the private workspace.
struct HeldNative {
    native: NativeStorage,
    hold: Arc<Mutex<Option<Hold>>>,
}
impl ProjectStorage for HeldNative {
    fn project_identity_scope(&self, l: &StorageLocation) -> Result<Blake3, Error> {
        self.native.project_identity_scope(l)
    }
    fn capability(&self) -> StorageCapability {
        self.native.capability()
    }
    fn asset_context(&self) -> Option<AssetContext> {
        self.native.asset_context()
    }
    fn prepare_open_snapshot(&mut self, l: &StorageLocation, c: &TransferCancel, m: &StreamMetrics) -> Result<PreparedSnapshot, Error> {
        self.native.prepare_open_snapshot(l, c, m)
    }
    fn save_plan(&mut self, l: &StorageLocation, p: &ArchivePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<ProjectAccess, Error> {
        let hold = self.hold.lock().unwrap().take();
        if let Some((entered, go)) = hold {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        self.native.save_plan(l, p, c, m)
    }
    fn checkpoint_plan(&mut self, p: &CheckpointPlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        self.native.checkpoint_plan(p, c, m)
    }
    fn append_plan(&mut self, p: &FramePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        self.native.append_plan(p, c, m)
    }
    fn space_probe(&self, l: &StorageLocation) -> SpaceProbe {
        self.native.space_probe(l)
    }
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.native.load(l)
    }
    fn prepare_open(&mut self, l: &StorageLocation) -> Result<LoadedProject, Error> {
        self.native.prepare_open(l)
    }
    fn commit_open(&mut self) {
        self.native.commit_open();
    }
    fn cancel_open(&mut self) {
        self.native.cancel_open();
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.native.save(l, b)
    }
    fn save_with_access(&mut self, l: &StorageLocation, b: &[u8]) -> Result<ProjectAccess, Error> {
        self.native.save_with_access(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.native.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.native.append(b)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.native.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.native.discard()
    }
}

#[test]
fn a_save_cancelled_after_its_archive_was_prepared_leaves_no_cleanup_debt_or_stray_files() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    std::fs::create_dir(&projects).unwrap();
    let path = projects.join("cancel.ncraft");
    let hold = Arc::new(Mutex::new(None));
    let mut project = Project::new(ProjectId::from_bytes([7; 16]), "Cancel").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let storage = HeldNative { native: NativeStorage::new(dir.path().join("journal")).unwrap(), hold: hold.clone() };
    let mut session = Session::with_sendable_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    session.set_save_runner(Arc::new(Threads));
    add_media(&mut session, 4, 256 * 1024, 1);
    session.dispatch("file.save_as", &json!({"location": path.to_str().unwrap(), "background": true})).unwrap();
    session.finish_file().unwrap().unwrap();
    add_media(&mut session, 2, 256 * 1024, 2);
    let edited = session.document().clone();
    let on_disk = std::fs::read(&path).unwrap();
    let before = (names(&projects), session.assets().context().storage_usage().unwrap(), session.assets().context().cleanup_debt().unwrap());

    let (entered, held) = mpsc::channel();
    let (go, wait) = mpsc::channel();
    *hold.lock().unwrap() = Some((entered, wait));
    session.dispatch("file.save", &json!({"background": true})).unwrap();
    held.recv().unwrap();
    // The prepared archive is in the workspace now: the charge grew.
    let during = session.assets().context().storage_usage().unwrap().total().unwrap();
    assert!(during > before.1.total().unwrap(), "the prepared archive is charged while it exists");
    session.dispatch("file.cancel", &json!({})).unwrap();
    go.send(()).unwrap();
    let error = session.finish_file().unwrap().unwrap_err();
    assert_eq!(error.message, "the save was cancelled; the file is unchanged");
    assert_eq!(std::fs::read(&path).unwrap(), on_disk, "the file is byte for byte as it was");
    assert_eq!(names(&projects), before.0, "no staged file is left beside it");
    let after = (session.assets().context().storage_usage().unwrap(), session.assets().context().cleanup_debt().unwrap());
    assert_eq!(after, (before.1, before.2), "the prepared archive was released and nothing is owed");
    assert_eq!(after.1, 0);
    assert_eq!(session.document(), &edited);
    assert!(session.file_status().dirty && !session.file_status().recovery_required);
    // And a save afterwards goes through.
    session.dispatch("file.save", &json!({})).unwrap();
    // One session holds a project at a time: close this one before reading it back.
    session.close_recovery(false).unwrap();
    drop(session);
    assert_eq!(read_back(dir.path(), &path), edited);
}

#[test]
fn work_on_a_background_opened_project_recovers_after_its_file_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.ncraft");
    let hashes = {
        let mut seed = native_session(&dir.path().join("seed"));
        let hashes = add_media(&mut seed, 3, 64 * 1024, 3);
        seed.dispatch("file.save_as", &json!({"location": path.to_str().unwrap()})).unwrap();
        seed.close_recovery(false).unwrap();
        hashes
    };
    let journal = dir.path().join("journal");
    {
        let mut session = native_session(&journal);
        session.set_save_runner(Arc::new(Threads));
        assert_eq!(
            session.dispatch("file.open", &json!({"location": path.to_str().unwrap(), "discard_dirty": true, "background": true})).unwrap()["opening"],
            true
        );
        assert_eq!(session.finish_file().unwrap().unwrap()["opened"], true);
        // The source goes away (moved, deleted, a drive unplugged) while work continues.
        std::fs::remove_file(&path).unwrap();
        add_media(&mut session, 1, 32 * 1024, 4);
        // A crash: the session ends without closing its journal.
    }
    let recovered = NativeStorage::new(journal).unwrap().recover().unwrap();
    assert!(recovered.damage.is_none() && recovered.applied > 0);
    for hash in &hashes {
        let lease = recovered.snapshot.assets.verified_content(hash, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert_eq!(lease.byte_len(), 64 * 1024, "the recovery copy holds the deleted file's media itself");
    }
    // The recovered work saves to a new file without the original.
    let mut session = native_session(&dir.path().join("after"));
    let expected = recovered.snapshot.project.clone();
    session.recover(recovered.snapshot, Some(StorageLocation::Path(path.clone()))).unwrap();
    let elsewhere = dir.path().join("recovered.ncraft");
    session.dispatch("file.save_as", &json!({"location": elsewhere.to_str().unwrap()})).unwrap();
    session.close_recovery(false).unwrap();
    drop(session);
    assert_eq!(read_back(dir.path(), &elsewhere), expected);
}

/// Every file under `root` with its size.
fn tree(root: &std::path::Path, into: &mut std::collections::BTreeMap<std::path::PathBuf, u64>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.filter_map(Result::ok) {
        match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => tree(&entry.path(), into),
            Ok(metadata) => {
                into.insert(entry.path(), metadata.len());
            }
            Err(_) => {}
        }
    }
}
fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, u64> {
    let mut files = std::collections::BTreeMap::new();
    tree(root, &mut files);
    files
}
/// The bytes of every file under `root`, once two readings 20 ms apart agree, so writes still
/// settling from earlier work (a journal's last records, say) aren't charged to what follows.
fn settled(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, u64> {
    let mut last = snapshot(root);
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let now = snapshot(root);
        if now == last {
            break;
        }
        last = now;
    }
    last
}
/// Peak growth, and which files made it, as the calibration's evidence.
struct Growth {
    bytes: u64,
    files: String,
}
/// Samples every file under `root` while `work` runs: the peak growth over the settled total
/// before it started, and the files that differed from that baseline at the peak.
fn peak_growth(root: &std::path::Path, work: impl FnOnce()) -> Growth {
    let start = settled(root);
    let start_total: u64 = start.values().sum();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let (root, done) = (root.to_path_buf(), done.clone());
        std::thread::spawn(move || {
            let mut peak = (0, std::collections::BTreeMap::new());
            loop {
                let finished = done.load(std::sync::atomic::Ordering::Acquire);
                let now = snapshot(&root);
                let total: u64 = now.values().sum();
                if total > peak.0 {
                    peak = (total, now);
                }
                if finished {
                    return peak;
                }
            }
        })
    };
    work();
    done.store(true, std::sync::atomic::Ordering::Release);
    let (total, at_peak) = sampler.join().unwrap();
    let mut files = String::new();
    for (path, bytes) in &at_peak {
        let before = start.get(path).copied().unwrap_or(0);
        if *bytes != before {
            let name = path.strip_prefix(root).unwrap_or(path).display();
            files.push_str(&format!("\n  {name}: {before} -> {bytes}"));
        }
    }
    for (path, before) in &start {
        if !at_peak.contains_key(path) {
            files.push_str(&format!("\n  {}: {before} -> gone", path.strip_prefix(root).unwrap_or(path).display()));
        }
    }
    eprintln!("calibration367 files at the peak:{files}");
    Growth { bytes: total.saturating_sub(start_total), files }
}

/// What production checks before saving `session` to `target`: its own inventory's one-volume
/// need, with the parts reported for the calibration.
fn estimate(session: &mut Session, target: &std::path::Path) -> (u64, nodecraft_format::OutputSpace, u64) {
    let output = session.assets().output_space(session.document(), &std::collections::BTreeSet::new()).unwrap().unwrap();
    let archive = ArchivePlan::estimate_bytes(session.document(), session.assets(), 1, StorageCapability::StreamingV2).unwrap();
    let (inventory, _) = session.save_space(Some(StorageLocation::Path(target.to_path_buf()))).unwrap().unwrap();
    (inventory.needed_on_one_volume().unwrap(), output, archive)
}

/// Modest real calibration (#367): a 6 MiB project in 30 assets on native storage. It is saved
/// from the session that imported it, then opened in a new session and saved again. The estimate never falls short of the measured
/// growth of every file under the test's folder, and doesn't overshoot it by more than the
/// parts sampling can't see (per-extent slack, a transient pack, the replaced journal).
#[test]
fn the_inventory_covers_measured_saves() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (dir.path().join("first"), dir.path().join("second"));
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let source = first.join("calibration.ncraft");
    let mut report = Vec::new();
    {
        let mut seed = native_session(&dir.path().join("seed"));
        add_media(&mut seed, 30, 200 * 1024, 5);
        let (needed, output, archive) = estimate(&mut seed, &source);
        let growth = peak_growth(dir.path(), || {
            seed.dispatch("file.save_as", &json!({"location": source.to_str().unwrap()})).unwrap();
        });
        let measured = growth.bytes;
        let written = std::fs::metadata(&source).unwrap().len();
        report.push(format!(
            "imported session: archive estimate {archive} B, file written {written} B, verified copies {} B, compaction {:?}, estimate {needed} B, measured peak growth {measured} B",
            output.verified_copies, output.compaction
        ));
        assert!(archive >= written && archive - written <= 16, "archive estimate {archive} vs written {written}");
        assert!(needed >= measured, "the estimate {needed} understates the measured peak {measured}; at the peak:{}", growth.files);
        assert!(needed <= measured.saturating_mul(2), "the estimate {needed} more than doubles the measured peak {measured}");
        seed.close_recovery(false).unwrap();
    }
    let mut session = native_session(&dir.path().join("journal"));
    // Opening a classic-format file: its private copy, a verified copy of each asset (checked
    // once the copy is read), and its checkpoint.
    let file_bytes = std::fs::metadata(&source).unwrap().len();
    let open_needed =
        SpaceInventory::open(file_bytes).unwrap().with_verified_copies(30 * 200 * 1024).needed(nodecraft_format::space::Volume::Recovery).unwrap();
    let open_growth = peak_growth(dir.path(), || {
        session.dispatch("file.open", &json!({"location": source.to_str().unwrap(), "discard_dirty": true})).unwrap();
    });
    let open_measured = open_growth.bytes;
    report.push(format!("open: file {file_bytes} B, estimate {open_needed} B, measured peak growth {open_measured} B"));
    assert!(open_needed >= open_measured, "the open estimate {open_needed} understates the measured peak {open_measured}; at the peak:{}", open_growth.files);
    assert!(open_needed <= open_measured.saturating_mul(2), "the open estimate {open_needed} more than doubles {open_measured}");
    let target = second.join("calibration.ncraft");
    let (needed, output, archive) = estimate(&mut session, &target);
    let growth = peak_growth(dir.path(), || {
        session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap()})).unwrap();
    });
    let measured = growth.bytes;
    let written = std::fs::metadata(&target).unwrap().len();
    report.push(format!(
        "opened session: archive estimate {archive} B, file written {written} B, verified copies {} B, compaction {:?}, estimate {needed} B, measured peak growth {measured} B",
        output.verified_copies, output.compaction
    ));
    eprintln!("calibration367:\n{}", report.join("\n"));
    assert!(needed >= measured, "the estimate {needed} understates the measured peak {measured}; at the peak:{}", growth.files);
    assert!(needed <= measured.saturating_mul(2), "the estimate {needed} more than doubles the measured peak {measured}");
}
