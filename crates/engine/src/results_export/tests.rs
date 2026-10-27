#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use crate::{
    Session,
    export_job::ExportResult,
    save_runner::{SaveJob, SaveRunner},
};
use nodecraft_graph::{Canvas, Change, JobInput, Port, Project};
use nodecraft_types::{Blake3, Modality, PortType, ProjectId};
use std::sync::{Arc, Mutex};
const C: CanvasId = CanvasId::from_bytes([66; 16]);
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
}
fn fixture() -> (Session, Arc<Held>) {
    let mut p = Project::new(ProjectId::from_bytes([66; 16]), "Public workflow").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let mut assets = Vec::new();
    for (i, mime, modality) in
        [(1, "image/png", Modality::Image), (2, "audio/wav", Modality::Audio), (3, "video/mp4", Modality::Video), (4, "model/gltf-binary", Modality::Mesh)]
    {
        let mut n = Node::new(NodeId::from_bytes([i; 16]), "test.media");
        n.title = format!("Take{i}");
        n.outputs = vec![Port { name: "out".into(), port_type: PortType { modality, many: false } }];
        for j in 1..=2 {
            let bytes = vec![i, j, 42];
            let hash = Blake3::from_bytes(*blake3::hash(&bytes).as_bytes());
            assets.push(bytes);
            let mut g = Generation::new(
                GenerationId::from_bytes([i + j * 10; 16]),
                vec![Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: Default::default() })],
            );
            g.settings = vec![JobSettings {
                provider: "mock".into(),
                instance: None,
                model: "original-model".into(),
                params: [("steps".into(), Value::Text("12".into())), ("api_key".into(), Value::Text("private-token".into()))].into(),
                inputs: vec![JobInput { role: "prompt".into(), value: Value::Text("original prompt".into()) }],
                item: None,
                billable_units: None,
                execution_ms: None,
            }];
            n.active = Some(g.id);
            n.history.push(g);
        }
        n.params.insert("prompt".into(), Value::Text("edited prompt must not appear".into()));
        Change::AddNode { canvas: C, node: n }.apply(&mut p).unwrap();
    }
    let mut s = Session::new(p).unwrap();
    for bytes in assets {
        s.import_asset(&bytes).unwrap();
    }
    let runner = Arc::new(Held::default());
    s.set_export_runner(runner.clone());
    (s, runner)
}
fn request(dir: &std::path::Path) -> Json {
    json!({"canvas":C,"destination":dir,"template":"{title}-{index}"})
}
fn run(s: &mut Session, r: &Held, host: &ExportHost, p: &Json) -> Report {
    crate::local_export::dispatch_with_host(s, host, "export.results_start", p).unwrap();
    assert!(s.export_status().unwrap().result.is_none());
    r.run();
    let Some(ExportResult::Results { report }) = s.export_status().unwrap().result else {
        panic!("{:?}", s.export_status());
    };
    report
}
#[test]
fn results966_mixed_history_atomic_names_provenance_and_scope() {
    let (mut s, r) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let p = request(dir.path());
    let revision = s.revision();
    assert!(s.dispatch("export.results_start", &p).is_err(), "JSON cannot grant authority");
    let mut empty = p.clone();
    empty["targets"] = json!([]);
    assert!(
        crate::local_export::dispatch_with_host(&mut s, &host, "export.results_plan", &empty).is_err(),
        "an empty explicit selector must never widen to all nodes"
    );
    let preview = crate::local_export::dispatch_with_host(&mut s, &host, "export.results_plan", &p).unwrap();
    assert_eq!(preview["total"], 8);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    std::fs::write(dir.path().join("Take1-1.png"), b"preserve").unwrap();
    std::fs::write(dir.path().join("workflow-results.json"), b"manifest sentinel").unwrap();
    let report = run(&mut s, &r, &host, &p);
    assert_eq!(report.written(), 8);
    assert_eq!(report.failed(), 0);
    assert_eq!(report.manifest.as_deref(), Some("workflow-results (2).json"));
    assert_eq!(std::fs::read(dir.path().join("Take1-1.png")).unwrap(), b"preserve");
    assert_eq!(s.revision(), revision);
    let bytes = std::fs::read(dir.path().join(report.manifest.unwrap())).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("original prompt"));
    assert!(text.contains("original-model"));
    assert!(!text.contains("edited prompt"));
    assert!(!text.contains("private-token"));
    assert!(!text.contains(&dir.path().display().to_string()));
    let manifest: Json = serde_json::from_str(&text).unwrap();
    for e in manifest["entries"].as_array().unwrap() {
        let name = e["result"]["file"].as_str().unwrap();
        assert!(!name.contains('/'));
        let data = std::fs::read(dir.path().join(name)).unwrap();
        assert_eq!(e["result"]["hash"], json!(Blake3::from_bytes(*blake3::hash(&data).as_bytes())));
    }
    assert!(s.dispatch("export.results_plan", &p).is_err(), "scoped host was restored");
    let mut selected = p.clone();
    selected["nodes"] = json!([NodeId::from_bytes([3; 16])]);
    selected["history"] = json!("active");
    let preview = crate::local_export::dispatch_with_host(&mut s, &host, "export.results_plan", &selected).unwrap();
    assert_eq!(preview["total"], 1);
}
#[test]
fn results966_failure_continues_without_staging_or_outside_writes() {
    let (mut s, r) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let p = request(dir.path());
    let mut unauthorized = p.clone();
    unauthorized["destination"] = json!(outside.path());
    assert!(crate::local_export::dispatch_with_host(&mut s, &host, "export.results_start", &unauthorized).is_err());
    // Hold every allowed numbered name for the first output: later media must still save.
    let base = ExportName { stem: "Take1-1".into(), extension: Some("png".into()) };
    for n in 1..=MAX_COLLISION_SUFFIX {
        let name = if n == 1 { base.file_name() } else { base.with_suffix(n) };
        std::os::unix::fs::symlink(outside.path().join("sentinel"), dir.path().join(name)).unwrap();
    }
    std::fs::write(outside.path().join("sentinel"), b"unchanged").unwrap();
    let report = run(&mut s, &r, &host, &p);
    assert_eq!(report.failed(), 1);
    assert_eq!(report.written(), 7);
    assert!(report.manifest.is_some());
    assert_eq!(std::fs::read(outside.path().join("sentinel")).unwrap(), b"unchanged");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".nodecraft")));
}
#[test]
fn results966_queued_cancel_drop_watch_and_exact_inactive_selector() {
    let (mut s, r) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let mut p = request(dir.path());
    p["targets"] = json!([{"node":NodeId::from_bytes([2;16]),"generation":GenerationId::from_bytes([12;16]),"port":"out"}]);
    assert_eq!(crate::local_export::dispatch_with_host(&mut s, &host, "export.results_plan", &p).unwrap()["total"], 1);
    let report = run(&mut s, &r, &host, &p);
    assert_eq!(report.written(), 1);
    assert_eq!(std::fs::read(dir.path().join(report.entries[0].file.as_ref().unwrap())).unwrap(), vec![2, 1, 42]);
    let id = crate::local_export::dispatch_with_host(&mut s, &host, "export.results_start", &p).unwrap()["pending"].as_u64().unwrap();
    let watch = s.export_watch().unwrap();
    s.dispatch("export.cancel", &json!({"id":id})).unwrap();
    drop(s);
    assert!(watch.status().result.is_none());
    r.run();
    let Some(ExportResult::Results { report }) = watch.status().result else {
        panic!("batch receipt");
    };
    assert!(report.cancelled);
    assert_eq!(report.written(), 0);
    assert!(report.manifest.is_some());
    assert_eq!(report.entries[0].state, "not_started");
}
#[test]
fn results966_staging_failure_removes_partial_and_swapped_grant_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("chosen");
    std::fs::create_dir(&folder).unwrap();
    let host = ExportHost::authorize(&folder).unwrap();
    let dest = host.pinned_destination(&folder).unwrap();
    let cancel = Default::default();
    let error = dest
        .publish_new("partial.png", &cancel, |out| {
            out.write_all(b"partial").unwrap();
            Err(invalid("injected failed read"))
        })
        .unwrap_err();
    assert_eq!(error.message, "injected failed read");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    let away = dir.path().join("moved");
    std::fs::rename(&folder, &away).unwrap();
    std::os::unix::fs::symlink(dir.path(), &folder).unwrap();
    assert!(dest.publish_new("escape.png", &cancel, |_| Ok(0)).is_err());
    assert!(!dir.path().join("escape.png").exists());
}

#[test]
fn results966_indexed_large_verifier_never_blocks_live_edits_or_quit_watch() {
    use nodecraft_format::{
        ProjectStorage,
        asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel},
    };
    use std::{
        io::Cursor,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
    };
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
            let part = self.bytes.get(usize::try_from(at).unwrap()..).unwrap_or_default();
            let n = part.len().min(out.len());
            out[..n].copy_from_slice(&part[..n]);
            Ok(n)
        }
    }
    let triangle = include_bytes!("../../../providers/src/fal/fixtures/triangle.glb");
    let json_len = u32::from_le_bytes(triangle[12..16].try_into().unwrap()) as usize;
    let mut model: Json = serde_json::from_slice(&triangle[20..20 + json_len]).unwrap();
    let mut bin = triangle[28 + json_len..].to_vec();
    bin.resize(16 * 1024 * 1024, 0);
    model["buffers"][0]["byteLength"] = json!(bin.len());
    let mut text = serde_json::to_vec(&model).unwrap();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let mut payload = b"glTF".to_vec();
    payload.extend_from_slice(&2u32.to_le_bytes());
    payload.extend_from_slice(&u32::try_from(28 + text.len() + bin.len()).unwrap().to_le_bytes());
    payload.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
    payload.extend_from_slice(b"JSON");
    payload.extend_from_slice(&text);
    payload.extend_from_slice(&u32::try_from(bin.len()).unwrap().to_le_bytes());
    payload.extend_from_slice(b"BIN\0");
    payload.extend_from_slice(&bin);
    nodecraft_media::decode_mesh(&payload, &Default::default()).unwrap();
    let hash = Blake3::from_bytes(*blake3::hash(&payload).as_bytes());
    let mut p = Project::new(ProjectId::from_bytes([96; 16]), "Public indexed workflow").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let mut n = Node::new(NodeId::from_bytes([96; 16]), "test.media");
    n.title = "Large mesh".into();
    n.outputs = vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Mesh, many: false } }];
    let g = Generation::new(
        GenerationId::from_bytes([96; 16]),
        vec![Value::Media(AssetRef { hash, mime: "model/gltf-binary".parse().unwrap(), meta: Default::default() })],
    );
    n.active = Some(g.id);
    n.history.push(g);
    // Keep this snapshot unverified: ordinary session load validates retained history.
    // The private plan below exercises the same worker with an indexed, uncached asset.
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("project.json", opts).unwrap();
    zip.write_all(&serde_json::to_vec(&json!({"version":4,"epoch":0,"project":p})).unwrap()).unwrap();
    zip.start_file(format!("assets/{hash}"), opts).unwrap();
    zip.write_all(&payload).unwrap();
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    let source = Arc::new(Blocked { bytes: zip.finish().unwrap().into_inner(), armed: AtomicBool::new(false), entered, release: Mutex::new(gate) });
    let range = AssetRange::new(source.clone(), 0, source.byte_len()).unwrap();
    let index = nodecraft_format::zip_index::ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let chosen = root.path().join("chosen");
    std::fs::create_dir(&chosen).unwrap();
    let storage = nodecraft_format::NativeStorage::new(root.path().join("recovery")).unwrap();
    let snapshot = nodecraft_format::decode_indexed(&index, storage.asset_context().unwrap()).unwrap();
    let mut s = Session::with_storage(snapshot, Box::new(storage)).unwrap();
    let runner = Arc::new(Held::default());
    s.set_export_runner(runner.clone());
    let host = ExportHost::authorize(&chosen).unwrap();
    let plan = Plan {
        host,
        destination: chosen.clone(),
        scope: History::All,
        entries: vec![Entry {
            name: Some(ExportName { stem: "Large mesh".into(), extension: Some("glb".into()) }),
            asset: Some(AssetRef { hash, mime: "model/gltf-binary".parse().unwrap(), meta: Default::default() }),
            source: Source { node: n.id, generation: n.active.unwrap(), port: "out".into(), item: None },
            provenance: json!({"origin":"not_recorded"}),
            error: None,
        }],
    };
    let mut exports = crate::export_job::Exports::default();
    exports.set_runner(runner.clone());
    assert_eq!(s.assets().context().storage_usage().unwrap().verified, 0, "preview never reads indexed bodies");
    source.armed.store(true, Ordering::Release);
    let id = exports.start_results(s.assets(), plan).unwrap();
    let watch = exports.watch().unwrap();
    let job = runner.0.lock().unwrap().take().unwrap();
    let worker = std::thread::spawn(move || job.run());
    waiting.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let usage = s.assets().context().storage_usage().unwrap();
    assert_eq!(usage.staging, payload.len() as u64, "verification has managed quota; no anonymous system-temp copy");
    assert_eq!(std::fs::read_dir(&chosen).unwrap().count(), 1, "only the private in-folder staging link exists while source is blocked");
    assert!(s.import_asset(b"live edits remain responsive").is_ok());
    exports.cancel(id).unwrap();
    assert!(watch.status().result.is_none(), "quit must wait for blocked read and cleanup");
    release.send(()).unwrap();
    worker.join().unwrap();
    let Some(ExportResult::Results { report }) = watch.status().result else {
        panic!("receipt");
    };
    assert!(report.cancelled);
    assert_eq!(report.written(), 0);
    assert!(report.manifest.is_some());
    assert_eq!(std::fs::read_dir(&chosen).unwrap().count(), 1, "no partial output or abandoned staging link");
    assert_eq!(s.assets().context().storage_usage().unwrap().staging, 0);
}
#[test]
fn results966_worker_drop_and_panic_cleanup_are_terminal() {
    let (mut s, r) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let p = request(dir.path());
    crate::local_export::dispatch_with_host(&mut s, &host, "export.results_start", &p).unwrap();
    let watch = s.export_watch().unwrap();
    drop(r.0.lock().unwrap().take());
    assert!(matches!(watch.status().result, Some(ExportResult::Failed { .. })));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let dest = host.pinned_destination(dir.path()).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dest.publish_new("panic.png", &Default::default(), |file| {
            file.write_all(b"partial").unwrap();
            panic!("injected source panic");
        })
    }));
    assert!(outcome.is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "unwinding cannot abandon private staging");
}
