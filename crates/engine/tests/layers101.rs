//! The Layer Editor (#101) end to end: an imported image wired into a Layer Editor, a shape
//! added and an image layer hidden through the layer commands, and each change run locally
//! through the run host into one composited frame. No provider is called.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    import::{Incoming, import_files},
    jobs::JobBudget,
    run_cmds::RunCatalog,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_media::image::RgbaImage;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Unsigned, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([71; 16]);
const IMAGE: NodeId = NodeId::from_bytes([1; 16]);
const EDITOR: NodeId = NodeId::from_bytes([2; 16]);
const COLOUR: [u8; 4] = [200, 90, 160, 255];

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
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

struct Fixture {
    session: Session,
    host: RunHost,
}

/// A uniform 40×30 image wired into an 200×100 Layer Editor with a white background.
fn fixture() -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::from_toml("").unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([72; 16]), "Layers").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "layers".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    host.install(&mut session);
    let pixels: Vec<u8> = (0..40 * 30).flat_map(|_| COLOUR).collect();
    let png = nodecraft_media::encode_png(40, 30, &pixels, &nodecraft_media::Limits::default()).unwrap();
    import_files(&mut session, C, Position { x: 0.0, y: 0.0 }, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[IMAGE]).unwrap();
    session.dispatch("node.create", &json!({"canvas": C, "id": EDITOR, "kind": "image.layers", "pos": {"x": 400, "y": 0}})).unwrap();
    for (name, value) in [("width", 200), ("height", 100)] {
        session.dispatch("node.param_set", &json!({"canvas": C, "id": EDITOR, "name": name, "value": Value::Unsigned(Unsigned(value))})).unwrap();
    }
    session.dispatch("node.param_set", &json!({"canvas": C, "id": EDITOR, "name": "transparent", "value": Value::Bool(false)})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": C, "id": EDITOR, "name": "background", "value": Value::Text("#ffffff".into())})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([10; 16]), "from": {"node": IMAGE, "port": "out"}, "to": {"node": EDITOR, "port": "layers"}, "order": 0});
    session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    Fixture { session, host }
}

/// Quotes and runs the editor, then returns the frame it made.
fn run(f: &mut Fixture) -> RgbaImage {
    let request = json!({"canvas": C, "mode": "selected", "nodes": [IMAGE, EDITOR]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != true, "unpriced_jobs": 0, "amounts": {}});
    f.session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&EDITOR).unwrap().clone();
    let result = node.history.iter().find(|g| Some(g.id) == node.active).unwrap();
    let Some(Value::Media(out)) = result.outputs.first() else { panic!("no image: {result:?}") };
    let bytes = f.session.assets().get(&out.hash).unwrap().to_vec();
    nodecraft_media::decode_image(&bytes, &nodecraft_media::Limits::default()).unwrap().1.to_rgba8()
}

fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
    image.get_pixel(x, y).0
}

#[test]
fn layer_edits_run_into_one_composited_frame() {
    let mut f = fixture();
    // The wired image is fitted into the middle of the frame, with the background either side.
    let frame = run(&mut f);
    assert_eq!((frame.width(), frame.height()), (200, 100));
    assert_eq!(pixel(&frame, 5, 50), [255, 255, 255, 255], "background beside the image");
    assert_eq!(pixel(&frame, 100, 50), COLOUR, "the image in the middle");

    // A shape added on top covers its square.
    let square = json!({"id": "dot", "name": "Dot", "x": 100, "y": 50, "width": 20, "height": 20,
        "content": {"kind": "shape", "shape": "rectangle", "fill": "#000000"}});
    f.session.dispatch("layers.add", &json!({"canvas": C, "id": EDITOR, "layer": square})).unwrap();
    let frame = run(&mut f);
    assert_eq!(pixel(&frame, 100, 50), [0, 0, 0, 255], "the shape over the image");
    assert_eq!(pixel(&frame, 100, 20), COLOUR, "the image around it");

    // Hiding the image layer leaves the shape on the background.
    let listed = f.session.dispatch("layers.list", &json!({"canvas": C, "id": EDITOR})).unwrap();
    let image = listed["layers"][0]["id"].clone();
    f.session.dispatch("layers.update", &json!({"canvas": C, "id": EDITOR, "layer": image, "set": {"visible": false}})).unwrap();
    let frame = run(&mut f);
    assert_eq!(pixel(&frame, 100, 20), [255, 255, 255, 255]);
    assert_eq!(pixel(&frame, 100, 50), [0, 0, 0, 255]);
}
