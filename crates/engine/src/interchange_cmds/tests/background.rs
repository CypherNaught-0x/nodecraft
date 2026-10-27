use super::*;
use crate::export_job::ExportResult;
use crate::save_runner::{SaveJob, SaveRunner};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Held(Mutex<Option<SaveJob>>);
impl SaveRunner for Held {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}
impl Held {
    fn run(&self) {
        self.0.lock().unwrap().take().unwrap().run();
    }
    fn take(&self) -> SaveJob {
        self.0.lock().unwrap().take().unwrap()
    }
}
fn params(path: &std::path::Path) -> Json {
    json!({"canvas": CANVAS, "id": EDITOR, "path": path, "background": true})
}

#[test]
fn background_interchange_captures_metadata_and_bytes_without_mutating_the_document() {
    let mut s = session();
    let before = s.document().clone();
    let runner = Arc::new(Held::default());
    s.set_export_runner(runner.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cut.zip");
    let reply = s.dispatch("timeline.export_interchange", &params(&path)).unwrap();
    let id = reply["pending"].as_u64().unwrap();
    assert!(reply.get("bytes").is_none(), "no fabricated early byte count");
    assert_eq!(s.document(), &before);
    assert!(!path.exists());
    // Source/title/clip edits after admission cannot change the captured export recipe.
    s.dispatch("node.title", &json!({"canvas": CANVAS, "id": EDITOR, "title": "Changed later"})).unwrap();
    runner.run();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(s.export_status().unwrap().id, id);
    assert_eq!(s.export_status().unwrap().result, Some(ExportResult::Exported { bytes: bytes.len() as u64 }));
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut xml = String::new();
    zip.by_name("Rough-cut.fcpxml").unwrap().read_to_string(&mut xml).unwrap();
    assert!(!xml.contains("Changed later"));
    let mut media = Vec::new();
    zip.by_name("media/01-Füchse-Eulen.mp4").unwrap().read_to_end(&mut media).unwrap();
    assert_eq!(media, MP4);
}

#[test]
fn background_cancel_preserves_existing_destination_and_watch_survives_session_drop() {
    for drop_session in [false, true] {
        let mut s = session();
        let runner = Arc::new(Held::default());
        s.set_export_runner(runner.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep.zip");
        std::fs::write(&path, b"existing original").unwrap();
        let mut p = params(&path);
        p["overwrite"] = json!(true);
        let id = s.dispatch("timeline.export_interchange", &p).unwrap()["pending"].as_u64().unwrap();
        let watch = s.export_watch().unwrap();
        if drop_session {
            drop(s);
        } else {
            s.dispatch("export.cancel", &json!({"id": id})).unwrap();
        }
        runner.run();
        assert_eq!(watch.status().result, Some(ExportResult::Cancelled));
        assert_eq!(std::fs::read(&path).unwrap(), b"existing original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn a_missing_or_rejected_runner_never_exports_inline() {
    struct Reject;
    impl SaveRunner for Reject {
        fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
            Err(job)
        }
    }
    let mut s = session();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cut.zip");
    assert_eq!(s.dispatch("timeline.export_interchange", &params(&path)).unwrap_err().kind, ErrorKind::Unsupported);
    assert!(!path.exists());
    s.set_export_runner(Arc::new(Reject));
    assert!(s.dispatch("timeline.export_interchange", &params(&path)).is_err());
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn enabled_and_queued_dispatch_never_read_an_indexed_media_body() {
    use nodecraft_format::{
        ProjectStorage,
        asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    struct Counted {
        bytes: Vec<u8>,
        armed: AtomicBool,
        reads: AtomicUsize,
        block: AtomicBool,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl AssetSource for Counted {
        fn byte_len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
            if self.armed.load(Ordering::Relaxed) {
                self.reads.fetch_add(1, Ordering::Relaxed);
            }
            if self.block.swap(false, Ordering::AcqRel) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            let data = self.bytes.get(usize::try_from(at).unwrap()..).unwrap_or_default();
            let n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
            Ok(n)
        }
    }
    let s = session();
    // Version 4 retains indexed bodies unverified. The classic encoder deliberately emits
    // version 1 for this small fixture, whose reader eagerly validates every body.
    use std::io::Write as _;
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("project.json", options).unwrap();
    archive.write_all(&serde_json::to_vec(&json!({"version":4,"epoch":0,"project":s.document()})).unwrap()).unwrap();
    for hash in s.assets().hashes() {
        archive.start_file(format!("assets/{hash}"), options).unwrap();
        archive.write_all(s.assets().get(hash).unwrap()).unwrap();
    }
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    let source = Arc::new(Counted {
        bytes: archive.finish().unwrap().into_inner(),
        armed: AtomicBool::new(false),
        reads: AtomicUsize::new(0),
        block: AtomicBool::new(false),
        entered,
        release: Mutex::new(gate),
    });
    let range = AssetRange::new(source.clone(), 0, source.byte_len()).unwrap();
    let index = nodecraft_format::zip_index::ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap();
    let mut snapshot = nodecraft_format::decode_indexed(&index, storage.asset_context().unwrap()).unwrap();
    assert_eq!(snapshot.assets.context().storage_usage().unwrap().verified, 0, "v4 fixture must start unverified");
    let document = snapshot.project.clone();
    // A native session's initial recovery checkpoint authenticates its referenced assets.
    // Keep these indexed sources unreferenced in the live checkpoint and supply the saved
    // timeline through the same command Context boundary, so the regression truly exercises
    // uncached indexed I/O rather than accidentally testing an already verified lease.
    snapshot.project = nodecraft_graph::Project::new(ProjectId::from_bytes([9; 16]), "Live work").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "Canvas") }.apply(&mut snapshot.project).unwrap();
    let mut s = Session::with_storage(snapshot, Box::new(storage)).unwrap();
    let runner = Arc::new(Held::default());
    let mut exports = crate::export_job::Exports::default();
    exports.set_runner(runner.clone());
    source.armed.store(true, Ordering::Release);
    let path = dir.path().join("cut.zip");
    let capture = |path: &std::path::Path| {
        let mut context = s.context(crate::commands::Phase::Dispatch);
        context.document = &document;
        let spec = context.commands.get("timeline.export_interchange").unwrap();
        (spec.enabled)(&context, &params(path)).unwrap();
        (spec.run)(&context, &params(path)).unwrap()
    };
    let Effect::Export { what, path: target, overwrite, background: true } = capture(&path) else { panic!("background export effect") };
    exports.start(s.assets(), what, target, overwrite).unwrap();
    assert_eq!(source.reads.load(Ordering::Relaxed), 0, "admission authenticates no media body");
    runner.run();
    assert!(source.reads.load(Ordering::Relaxed) > 0, "the worker authenticates the original source");
    assert!(matches!(exports.status().unwrap().result, Some(ExportResult::Exported { .. })));
    assert_eq!(s.assets().context().storage_usage().unwrap().verified, 0, "private verifier doesn't populate the live cache");
    // Start another copy and block its first original-source read. The private verifier
    // must leave live graph edits/import/checkpoint and cancellation usable throughout.
    let path = dir.path().join("cancelled.zip");
    let Effect::Export { what, path: target, overwrite, background: true } = capture(&path) else { panic!("background export effect") };
    let id = exports.start(s.assets(), what, target, overwrite).unwrap();
    source.block.store(true, Ordering::Release);
    let job = runner.take();
    let worker = std::thread::spawn(move || job.run());
    waiting.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    s.import_asset(b"new result while exporting").unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": EDITOR, "kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    s.dispatch("file.save_as", &json!({"location": dir.path().join("live.ncraft")})).unwrap();
    exports.cancel(id).unwrap();
    assert!(!path.exists());
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(exports.status().unwrap().result, Some(ExportResult::Cancelled));
    assert!(!path.exists());
}
