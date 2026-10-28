//! Real free-action run, native history, original bytes and project reopen for binary STL.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::Services,
    generation_catalog::GenerationCatalog,
    import::{Incoming, import_files},
    jobs::JobBudget,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::driver::{Runtime, TaskFuture};
use nodecraft_format::{NativeStorage, Snapshot, asset_source::StreamMetrics};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};

const CANVAS: CanvasId = CanvasId::from_bytes([56; 16]);
const SOURCE: NodeId = NodeId::from_bytes([57; 16]);
const CONVERT: NodeId = NodeId::from_bytes([58; 16]);
const OBJ: &[u8] = b"v -1 0 0 1 0 0\nv 1 0 0 0 1 0\nv 1 1 0 0 0 1\nv -1 1 0 1 1 0\nf 1 2 3 4\n";

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
fn empty_project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([56; 16]), "Original STL example").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Geometry") }.apply(&mut project).unwrap();
    project
}
fn read(session: &Session, asset: &AssetRef) -> Vec<u8> {
    let metrics = StreamMetrics::default();
    let lease = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap();
    let mut bytes = Vec::new();
    lease.reader(metrics).take(1024 * 1024).read_to_end(&mut bytes).unwrap();
    bytes
}
#[test]
fn stl569_actual_run_keeps_source_and_reopens_verified_geometry_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let kinds = GenerationCatalog::builtin().unwrap().registry().unwrap();
    let mut project = empty_project();
    let action =
        kinds.get("mesh.convert").unwrap().instantiate_instance(CONVERT, Default::default(), &[("format".into(), Value::Text("stl".into()))].into()).unwrap();
    Change::AddNode { canvas: CANVAS, node: action }.apply(&mut project).unwrap();
    let mut session =
        Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(NativeStorage::new(dir.path().join("recovery")).unwrap()))
            .unwrap();
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "stl569".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone());
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    session = session.with_services(services);
    host.install(&mut session);
    import_files(&mut session, CANVAS, Position::default(), 300.0, &[Incoming { name: "original.obj", bytes: OBJ }], &[SOURCE]).unwrap();
    session.dispatch("edge.connect", &json!({"canvas":CANVAS,"edge":{"id":EdgeId::from_bytes([56;16]),"from":{"node":SOURCE,"port":"out"},"to":{"node":CONVERT,"port":"model"},"order":0}})).unwrap();
    let original = session.document().canvases()[&CANVAS].nodes()[&SOURCE].params["asset"].clone();
    let mut request = json!({"canvas":CANVAS,"mode":"selected","nodes":[CONVERT]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":false,"unpriced_jobs":0});
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(notices.contains(&RunNotice::Recorded { canvas: CANVAS, node: CONVERT }), "{notices:?}");
    assert!(budget.log.lock().unwrap().entries().all(|entry| entry.cost.micros == 0), "a deterministic conversion has no paid cost");
    let node = &session.document().canvases()[&CANVAS].nodes()[&CONVERT];
    let [Value::Media(asset)] = node.history.last().unwrap().outputs.as_slice() else { panic!("one STL result") };
    let asset = asset.clone();
    assert_eq!((asset.mime.essence(), asset.meta.vertices, asset.meta.triangles), ("model/stl", Some(6), Some(2)));
    let bytes = read(&session, &asset);
    assert_eq!(bytes.len(), 184);
    let mesh = nodecraft_media::decode_mesh(&bytes, &Default::default()).unwrap();
    assert_eq!((mesh.vertex_count(), mesh.triangle_count()), (6, 2));
    assert!(mesh.colors.iter().all(|c| c == &[1.0; 4]));
    assert_eq!(session.document().canvases()[&CANVAS].nodes()[&SOURCE].params["asset"], original);
    let Value::Media(source) = &original else { panic!("original model") };
    assert_eq!(read(&session, source), OBJ);
    let exported = dir.path().join(format!("geometry.{}", nodecraft_format::export::extension_for(asset.mime.essence()).unwrap()));
    std::fs::write(&exported, &bytes).unwrap();
    assert_eq!(nodecraft_media::decode_mesh(&std::fs::read(exported).unwrap(), &Default::default()).unwrap(), mesh);
    let path = dir.path().join("geometry.ncraft");
    session.dispatch("file.save_as", &json!({"location":path})).unwrap();
    let expected = session.document().clone();
    host.shut_down();
    drop(host);
    drop(session);
    let mut reopened = Session::with_storage(
        Snapshot { project: empty_project(), assets: Default::default(), epoch: 0 },
        Box::new(NativeStorage::new(dir.path().join("reopen")).unwrap()),
    )
    .unwrap();
    reopened.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
    assert_eq!(reopened.document(), &expected);
    assert_eq!(read(&reopened, &asset), bytes);
    assert_eq!(read(&reopened, source), OBJ);
    let imported = NodeId::from_bytes([59; 16]);
    import_files(&mut reopened, CANVAS, Position { x: -300.0, y: 0.0 }, 300.0, &[Incoming { name: "geometry.stl", bytes: &bytes }], &[imported]).unwrap();
    let imported_node = &reopened.document().canvases()[&CANVAS].nodes()[&imported];
    assert_eq!(imported_node.kind, "source.mesh");
    assert_eq!(imported_node.params["asset"], Value::Media(asset));
    reopened.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(reopened.document(), &expected);
}
