//! The disk-space checks are wired into the real Session paths (#367): each test makes the
//! recovery drive short at exactly one check (an open's first check, the check once the file is
//! read, the identity rewrite, the save's history roots) on native storage, so removing that
//! check makes the operation go ahead and the test fail. Also: a save's history-root copies run
//! on the save's own thread, under its cancel, and a modest streaming (v4) project calibrates
//! the copies term.
use nodecraft_engine::Session;
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_format::{
    ArchivePlan, AssetStore, NativeStorage, Snapshot, SpaceReport, StorageCapability, StorageLocation,
    space::{SpaceInventory, SpaceProbe, Volume},
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const C: CanvasId = CanvasId::from_bytes([8; 16]);
const PLENTY: u64 = 1 << 50;
const SHORT: u64 = 4096;

/// A free-space report that is plentiful for the first `calls` checks and short after, with the
/// real size of any file at the location; and how many checks asked.
fn report(plentiful: usize) -> (SpaceReport, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let report: SpaceReport = Arc::new(move |location: &StorageLocation| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        let free = if n < plentiful { PLENTY } else { SHORT };
        let existing_bytes = match location {
            StorageLocation::Path(path) => std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file).map(|m| m.len()),
            _ => None,
        };
        SpaceProbe { destination_free: Some(free), recovery_free: Some(free), shared: true, existing_bytes }
    });
    (report, asked)
}
fn project(id: [u8; 16]) -> Project {
    let mut project = Project::new(ProjectId::from_bytes(id), "Wiring").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    project
}
fn session(journal: &Path, space: Option<SpaceReport>) -> Session {
    let mut storage = NativeStorage::new(journal.to_path_buf()).unwrap();
    if let Some(space) = space {
        storage.set_space_probe(space);
    }
    Session::with_sendable_storage(Snapshot { project: project([8; 16]), assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap()
}
fn media_node(n: usize, hash: Blake3) -> Node {
    let mut id = [7; 16];
    id[..8].copy_from_slice(&(n as u64).to_le_bytes());
    let mut node = Node::new(NodeId::from_bytes(id), "source.text");
    node.params.insert("content".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    node
}
fn add_media(session: &mut Session, count: usize, bytes: usize, seed: u8) {
    for n in 0..count {
        let mut body = vec![seed; bytes];
        body[..8].copy_from_slice(&(n as u64).to_le_bytes());
        let hash = session.import_asset(&body).unwrap();
        session.dispatch("node.add", &json!({"canvas": C, "node": media_node(n, hash)})).unwrap();
    }
}
/// A classic-format project file with three small assets.
fn classic_file(dir: &Path) -> PathBuf {
    let path = dir.join("classic.ncraft");
    let mut seed = session(&dir.join("seed-classic"), None);
    add_media(&mut seed, 3, 64 * 1024, 1);
    seed.dispatch("file.save_as", &json!({"location": path.to_str().unwrap()})).unwrap();
    seed.close_recovery(false).unwrap();
    path
}
/// A streaming-format (v4) project file: four 13 MiB assets, 52 MiB in all, past the classic
/// wire's 48 MiB of assets.
fn streaming_file(dir: &Path) -> PathBuf {
    let path = dir.join("streaming.ncraft");
    let mut seed = session(&dir.join("seed-streaming"), None);
    add_media(&mut seed, 4, 13 * 1024 * 1024, 2);
    seed.dispatch("file.save_as", &json!({"location": path.to_str().unwrap()})).unwrap();
    seed.close_recovery(false).unwrap();
    path
}
fn open(session: &mut Session, path: &Path) -> Result<serde_json::Value, nodecraft_types::Error> {
    session.dispatch("file.open", &json!({"location": path.to_str().unwrap(), "discard_dirty": true}))
}
/// The open was refused for space inside the open: nothing replaced, no checkpoint published.
fn refused_open(dir: &Path, path: &Path, plentiful: usize) {
    let journal = dir.join(format!("journal-{plentiful}"));
    let (space, asked) = report(plentiful);
    let mut session = session(&journal, Some(space));
    let before = (session.document().clone(), std::fs::read(&journal).unwrap());
    let error = open(&mut session, path).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy, "{}", error.message);
    assert!(error.message.starts_with("Not enough free disk space to open this project"), "{}", error.message);
    assert!(error.message.contains("current project is unchanged"), "{}", error.message);
    assert!(asked.load(Ordering::SeqCst) > plentiful, "refused by the check after the first {plentiful}");
    assert_eq!(session.document(), &before.0, "the current project stays");
    assert!(session.file_status().location.is_none());
    assert_eq!(std::fs::read(&journal).unwrap(), before.1, "no checkpoint was published");
    // With room, the same open goes through.
    let mut roomy = self::session(&dir.join(format!("journal-roomy-{plentiful}")), Some(report(usize::MAX).0));
    open(&mut roomy, path).unwrap();
}

#[test]
fn a_classic_open_is_refused_when_its_asset_copies_dont_fit_after_the_private_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = classic_file(dir.path());
    // First check (before anything is written) passes; the one once the file is read doesn't.
    refused_open(dir.path(), &path, 1);
}

#[test]
fn a_legacy_identity_open_is_refused_when_its_rewrite_doesnt_fit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.ncraft");
    // A project under the pooled legacy identity, as older builds wrote.
    let snapshot = Snapshot { project: project([2; 16]), assets: AssetStore::default(), epoch: 0 };
    std::fs::write(&path, nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    // The open's first check and the check once the file is read pass; the identity rewrite's doesn't.
    refused_open(dir.path(), &path, 2);
}

#[test]
fn an_older_format_open_is_refused_when_its_rewrite_doesnt_fit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comments-v1.ncraft");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../format/tests/fixtures/v1/comments-v1.ncraft"), &path).unwrap();
    refused_open(dir.path(), &path, 2);
}

/// A session that opened the streaming file (its media indexed, not yet verified here) and then
/// deleted the first node, so its 13 MiB asset is a history-only root.
fn opened_with_history_root(dir: &Path, journal: &str, space: Option<SpaceReport>) -> Session {
    let path = streaming_file(dir);
    let mut session = session(&dir.join(journal), space);
    open(&mut session, &path).unwrap();
    let first = media_node(0, Blake3::from_bytes([0; 32])).id;
    session.dispatch("node.remove", &json!({"canvas": C, "id": first})).unwrap();
    session
}

#[test]
fn a_save_is_refused_when_its_history_roots_alone_dont_fit() {
    let dir = tempfile::tempdir().unwrap();
    let free = Arc::new(Mutex::new(PLENTY));
    let shared = Arc::clone(&free);
    let space: SpaceReport = Arc::new(move |location: &StorageLocation| {
        let existing_bytes = match location {
            StorageLocation::Path(path) => std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file).map(|m| m.len()),
            _ => None,
        };
        let free = *shared.lock().unwrap();
        SpaceProbe { destination_free: Some(free), recovery_free: Some(free), shared: true, existing_bytes }
    });
    let mut session = opened_with_history_root(dir.path(), "journal", Some(space));
    let output = session.assets().output_space(session.document(), &std::collections::BTreeSet::new()).unwrap().unwrap();
    let archive = ArchivePlan::estimate_bytes(session.document(), session.assets(), 2, StorageCapability::StreamingV2).unwrap();
    let document_only = SpaceInventory::save(archive, None).unwrap().with_verified_copies(output.verified_copies).needed_on_one_volume().unwrap();
    assert_eq!(output.verified_copies, 3 * 13 * 1024 * 1024, "the document's own indexed media");
    // Room for the document's media, but not for the 13 MiB history root on top.
    *free.lock().unwrap() = document_only + 1024 * 1024;
    let target = dir.path().join("history.ncraft");
    let error = session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap()})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy, "{}", error.message);
    assert!(error.message.starts_with("Not enough free disk space to save"), "{}", error.message);
    assert!(!target.exists());
    assert!(session.file_status().dirty);
    *free.lock().unwrap() = PLENTY;
    session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap()})).unwrap();
}

#[derive(Default)]
struct Gate(Mutex<Vec<SaveJob>>);
impl SaveRunner for Gate {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}

#[test]
fn a_background_saves_history_copies_run_on_its_job_and_stop_when_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = opened_with_history_root(dir.path(), "journal", None);
    let gate = Arc::new(Gate::default());
    session.set_save_runner(gate.clone());
    let before = session_usage(&session);
    let target = dir.path().join("copied.ncraft");
    assert_eq!(session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap(), "background": true})).unwrap()["saving"], true);
    assert_eq!(session_usage(&session), before, "the calling thread copied nothing: the job does it");
    session.dispatch("file.cancel", &json!({})).unwrap();
    std::mem::take(&mut *gate.0.lock().unwrap()).into_iter().for_each(SaveJob::run);
    let error = session.poll_file().unwrap().unwrap_err();
    assert_eq!(error.message, "the save was cancelled; the file is unchanged");
    assert_eq!(session_usage(&session), before, "the cancel stopped the copies too");
    assert!(!target.exists());
    // Saved for real, the job copies the history root (and the document's media) itself.
    session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap(), "background": true})).unwrap();
    assert_eq!(session_usage(&session), before);
    std::mem::take(&mut *gate.0.lock().unwrap()).into_iter().for_each(SaveJob::run);
    assert_eq!(session.poll_file().unwrap().unwrap()["saved"], true);
}
fn session_usage(session: &Session) -> u64 {
    session.assets().context().storage_usage().unwrap().verified
}

/// Sums the sizes of every file under `root`.
fn tree_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => tree_bytes(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}
/// The peak growth of every file under `root` while `work` runs, sampled on another thread.
fn peak_growth(root: &Path, work: impl FnOnce()) -> u64 {
    let start = tree_bytes(root);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let (root, done) = (root.to_path_buf(), done.clone());
        std::thread::spawn(move || {
            let mut peak = 0;
            while !done.load(Ordering::Acquire) {
                peak = peak.max(tree_bytes(&root));
            }
            peak.max(tree_bytes(&root))
        })
    };
    work();
    done.store(true, Ordering::Release);
    sampler.join().unwrap().saturating_sub(start)
}

/// Modest streaming calibration (#367): a 52 MiB v4 project opened in a new session (its media
/// stay indexed, not copied) and saved again, which copies all of them first. The estimates are
/// the ones production computes, in the order it checks them.
#[test]
fn a_streaming_project_calibrates_the_copies_term() {
    let dir = tempfile::tempdir().unwrap();
    let path = streaming_file(dir.path());
    let file_bytes = std::fs::metadata(&path).unwrap().len();
    let mut session = session(&dir.path().join("journal"), None);
    // An open's first check; a streaming file's second (classic copies) adds nothing.
    let open_needed = SpaceInventory::open(file_bytes)
        .unwrap()
        .with_journal_replaced(std::fs::metadata(dir.path().join("journal")).unwrap().len())
        .needed(Volume::Recovery)
        .unwrap();
    let open_measured = peak_growth(dir.path(), || {
        open(&mut session, &path).unwrap();
    });
    let output = session.assets().output_space(session.document(), &std::collections::BTreeSet::new()).unwrap().unwrap();
    let archive = ArchivePlan::estimate_bytes(session.document(), session.assets(), 2, StorageCapability::StreamingV2).unwrap();
    let target = dir.path().join("resaved.ncraft");
    let save_needed = session.save_space(Some(StorageLocation::Path(target.clone()))).unwrap().unwrap().0.needed_on_one_volume().unwrap();
    let save_measured = peak_growth(dir.path(), || {
        session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap()})).unwrap();
    });
    let written = std::fs::metadata(&target).unwrap().len();
    eprintln!(
        "calibration367 streaming: file {file_bytes} B; open estimate {open_needed} B, measured {open_measured} B; save: archive estimate {archive} B, written {written} B, verified copies {} B, compaction {:?}, estimate {save_needed} B, measured {save_measured} B",
        output.verified_copies, output.compaction
    );
    assert_eq!(output.verified_copies, 4 * 13 * 1024 * 1024, "a streaming open leaves every asset to copy before a save");
    assert!(archive >= written && archive - written <= 16);
    for (needed, measured) in [(open_needed, open_measured), (save_needed, save_measured)] {
        assert!(needed >= measured, "the estimate {needed} understates the measured peak {measured}");
        assert!(needed <= measured.saturating_mul(2), "the estimate {needed} more than doubles the measured peak {measured}");
    }
}
