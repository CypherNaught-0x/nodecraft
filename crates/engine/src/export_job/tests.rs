// Test-only fixtures and deliberate runner-panic injection exercise the completion guard.
#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use nodecraft_types::AssetRef;
static SERIAL: Mutex<()> = Mutex::new(());
#[derive(Default)]
struct Held(Mutex<Option<SaveJob>>);
impl SaveRunner for Held {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}
impl Held {
    fn take(&self) -> SaveJob {
        self.0.lock().unwrap().take().unwrap()
    }
}
fn fixture() -> (Exports, Arc<Held>, AssetStore, Exported) {
    let mut assets = AssetStore::default();
    let hash = assets.insert(b"verified public export").unwrap();
    let what = Exported::Asset(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() });
    let runner = Arc::new(Held::default());
    let mut exports = Exports::default();
    exports.set_runner(runner.clone());
    (exports, runner, assets, what)
}

#[test]
fn a_queued_export_pins_its_bytes_and_retains_the_terminal_receipt() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.txt");
    let id = exports.start(&assets, what.clone(), path.clone(), false).unwrap();
    assert!(!path.exists());
    assert_eq!(exports.status().unwrap().phase, ExportPhase::Queued);
    assert_eq!(exports.start(&assets, what, path.clone(), false).unwrap_err().kind, ErrorKind::Busy);
    drop(assets);
    runner.take().run();
    assert_eq!(std::fs::read(&path).unwrap(), b"verified public export");
    let status = exports.status().unwrap();
    assert_eq!(status.id, id);
    assert_eq!(status.result, Some(ExportResult::Exported { bytes: 22 }));
    assert_eq!(exports.status(), Some(status));
}

#[test]
fn an_export_watch_survives_its_owner_until_cancelled_cleanup_finishes() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.txt");
    let id = exports.start(&assets, what, path.clone(), false).unwrap();
    let watch = exports.watch().unwrap();
    drop(exports);
    assert_eq!(watch.status().id, id);
    assert!(watch.status().result.is_none(), "dropping the owner only requests cancellation");
    runner.take().run();
    assert_eq!(watch.status().result, Some(ExportResult::Cancelled));
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn accepted_cancellation_preserves_an_existing_target_and_releases_admission() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.txt");
    std::fs::write(&path, b"keep this file").unwrap();
    let id = exports.start(&assets, what.clone(), path.clone(), true).unwrap();
    exports.cancel(id).unwrap();
    runner.take().run();
    assert_eq!(exports.status().unwrap().result, Some(ExportResult::Cancelled));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep this file");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no staging file survived");
    exports.start(&assets, what, path.clone(), false).unwrap();
    runner.take().run();
    assert!(matches!(exports.status().unwrap().result, Some(ExportResult::Failed { name_taken: true, .. })));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep this file");
}

#[test]
fn accepted_cancellation_wins_a_racing_name_conflict() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.txt");
    std::fs::write(&path, b"keep this file").unwrap();
    let id = exports.start(&assets, what.clone(), path.clone(), false).unwrap();
    let control = exports.current.as_ref().unwrap();
    control.phase(ExportPhase::Checking).unwrap();
    let conflict = crate::export::write(&assets, &what, &path, false).unwrap_err();
    assert!(crate::export::name_conflict(&conflict));
    exports.cancel(id).unwrap();
    control.finish(Err(conflict));
    assert_eq!(exports.status().unwrap().result, Some(ExportResult::Cancelled));
    drop(runner.take());
    assert_eq!(exports.status().unwrap().result, Some(ExportResult::Cancelled));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep this file");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn cancellation_and_finishing_have_one_publication_boundary() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.txt");
    let id = exports.start(&assets, what, path, false).unwrap();
    let control = exports.current.as_ref().unwrap();
    control.phase(ExportPhase::Finishing).unwrap();
    assert_eq!(exports.cancel(id).unwrap_err().kind, ErrorKind::Conflict);
    assert!(control.cancel.check().is_ok(), "a rejected late cancel cannot cancel publication");
    drop(runner.take());
    assert!(matches!(exports.status().unwrap().result, Some(ExportResult::Failed { .. })));
}

#[test]
fn a_missing_folder_and_a_dropped_job_are_terminal_path_free_failures() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private-folder").join("result.txt");
    exports.start(&assets, what.clone(), path.clone(), false).unwrap();
    runner.take().run();
    assert!(matches!(exports.status().unwrap().result, Some(ExportResult::Failed { .. })));
    assert!(!serde_json::to_string(&exports.status()).unwrap().contains("private-folder"));
    exports.start(&assets, what, path.clone(), false).unwrap();
    drop(runner.take());
    assert!(matches!(exports.status().unwrap().result, Some(ExportResult::Failed { .. })));
    assert!(!path.exists());
}

#[test]
fn a_dropped_job_releases_its_process_slot_before_publishing_completion() {
    let _serial = SERIAL.lock().unwrap();
    let (mut exports, runner, assets, what) = fixture();
    let dir = tempfile::tempdir().unwrap();
    exports.start(&assets, what, dir.path().join("result.txt"), false).unwrap();
    let control = exports.current.as_ref().unwrap().clone();
    let state = control.state.lock().unwrap();
    let job = runner.take();
    let worker = std::thread::spawn(move || drop(job));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let acquired = loop {
        if let Ok(lease) = Lease::acquire() {
            break Some(lease);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    drop(state);
    worker.join().unwrap();
    assert!(acquired.is_some(), "the lease must be released even while terminal publication is waiting");
    assert!(exports.status().unwrap().result.is_some());
}

struct Reject;
impl SaveRunner for Reject {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        Err(job)
    }
}
struct Panic;
impl SaveRunner for Panic {
    fn spawn(&self, _job: SaveJob) -> Result<(), SaveJob> {
        panic!("injected runner failure")
    }
}

#[test]
fn a_blocked_export_verifier_keeps_live_import_save_and_cancellation_available() {
    use nodecraft_format::{
        ProjectStorage,
        asset_source::{AssetRange, AssetSource, StreamMetrics},
    };
    use std::{
        io::{Cursor, Write},
        sync::mpsc,
    };
    let _serial = SERIAL.lock().unwrap();
    struct Blocked {
        bytes: Vec<u8>,
        armed: AtomicBool,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl AssetSource for Blocked {
        fn byte_len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
            if self.armed.swap(false, Ordering::AcqRel) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            let at = usize::try_from(at).unwrap();
            let data = self.bytes.get(at..).unwrap_or_default();
            let n = out.len().min(data.len());
            out[..n].copy_from_slice(&data[..n]);
            Ok(n)
        }
    }
    let payload = vec![b'x'; 512 * 1024];
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(&payload).as_bytes());
    let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([44; 16]), "Public export fixture").unwrap();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("project.json", options).unwrap();
    zip.write_all(&serde_json::to_vec(&serde_json::json!({"version":4,"epoch":0,"project":project})).unwrap()).unwrap();
    zip.start_file(format!("assets/{hash}"), options).unwrap();
    zip.write_all(&payload).unwrap();
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    let source = Arc::new(Blocked { bytes: zip.finish().unwrap().into_inner(), armed: AtomicBool::new(false), entered, release: Mutex::new(gate) });
    let range = AssetRange::new(source.clone(), 0, source.byte_len()).unwrap();
    let index = nodecraft_format::zip_index::ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap();
    let snapshot = nodecraft_format::decode_indexed(&index, storage.asset_context().unwrap()).unwrap();
    let mut session = crate::Session::with_storage(snapshot, Box::new(storage)).unwrap();
    let runner = Arc::new(Held::default());
    let mut exports = Exports::default();
    exports.set_runner(runner.clone());
    let path = dir.path().join("result.txt");
    source.armed.store(true, Ordering::Release);
    let id = exports
        .start(session.assets(), Exported::Asset(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }), path.clone(), false)
        .unwrap();
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, 0);
    let job = runner.take();
    let worker = std::thread::spawn(move || job.run());
    waiting.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(exports.status().unwrap().phase, ExportPhase::Checking);
    let scratch_bytes = payload.len() as u64;
    let checking = session.assets().context().storage_usage().unwrap();
    assert_eq!(checking.verified, scratch_bytes, "private export bytes share the session quota");
    assert_eq!(checking.staging, scratch_bytes, "the in-progress copy is charged before reading");
    // These previously returned Busy while the export held the shared context operation
    // and staging gates. The first export source read is still blocked throughout.
    let imported = session.import_asset(b"another accepted result").unwrap();
    assert!(session.assets().contains(&imported));
    session.dispatch("file.save_as", &serde_json::json!({"location":dir.path().join("live.ncraft")})).unwrap();
    session.dispatch("file.save", &serde_json::json!({})).unwrap();
    assert!(dir.path().join("live.ncraft").is_file());
    let retained = session.assets().context().storage_usage().unwrap().verified - scratch_bytes;
    exports.cancel(id).unwrap(); // Must return while the source read remains blocked.
    assert!(!path.exists());
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(exports.status().unwrap().result, Some(ExportResult::Cancelled));
    assert!(!path.exists());
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, retained, "cancel releases scratch charges without changing live storage");
}
#[test]
fn a_refused_or_panicking_runner_never_falls_back_to_inline_io() {
    let _serial = SERIAL.lock().unwrap();
    for runner in [Arc::new(Reject) as Arc<dyn SaveRunner>, Arc::new(Panic)] {
        let (mut exports, _, assets, what) = fixture();
        exports.set_runner(runner);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.txt");
        assert!(crate::session::guard(|| exports.start(&assets, what, path.clone(), false)).is_err());
        assert!(!path.exists());
        assert!(matches!(exports.status().unwrap().result, Some(ExportResult::Failed { .. })));
    }
}
