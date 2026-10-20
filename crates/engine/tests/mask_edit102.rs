//! Inpainting and outpainting (#102) end to end: an imported image, `mask.create`, strokes or
//! margins, `mask.edit`, then a quoted run through the run host. The Expand and Mask actions run
//! locally and the mock provider receives the image and its mask at the same size. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    import::{Incoming, import_files},
    jobs::JobBudget,
    provider_routing::ProviderRouting,
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
use nodecraft_providers::JobInput;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const IMAGE: NodeId = NodeId::from_bytes([1; 16]);
const EXPAND: NodeId = NodeId::from_bytes([2; 16]);
const MASK: NodeId = NodeId::from_bytes([3; 16]);
const EDIT: NodeId = NodeId::from_bytes([4; 16]);
const MODEL: &str = "fixture.inpainter";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-image"
label = "Fixture inpainter"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.02 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
[[model.modes]]
id = "inpaint"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "image", modality = "image", min = 1, max = 1 }}, {{ role = "mask", modality = "image", min = 1, max = 1 }}]
"#
    ))
    .unwrap()
}

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
    mock: MockProvider,
}

/// An imported 40×30 image with an Expand canvas and a Mask behind it.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([62; 16]), "Inpaint").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "mask-edit".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(EDIT, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let pixels: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 200) as u8, 90, 160, 255]).collect();
    let png = nodecraft_media::encode_png(40, 30, &pixels, &nodecraft_media::Limits::default()).unwrap();
    import_files(&mut session, C, Position { x: 0.0, y: 0.0 }, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[IMAGE]).unwrap();
    let create = json!({"canvas": C, "from": {"node": IMAGE, "port": "out"}, "expand": EXPAND, "mask": MASK,
        "edges": [EdgeId::from_bytes([10; 16]), EdgeId::from_bytes([11; 16])], "pos": {"x": 400, "y": 0}});
    session.dispatch("mask.create", &create).unwrap();
    Fixture { session, host, mock }
}

/// Adds the edit card, quotes it (the quoted price is the model's), and runs everything until idle.
fn edit_and_run(f: &mut Fixture, prompt: &str) {
    let edit = json!({"canvas": C, "mask": MASK, "id": EDIT, "edges": [EdgeId::from_bytes([12; 16]), EdgeId::from_bytes([13; 16])],
        "pos": {"x": 1200, "y": 0}, "prompt": prompt, "model": MODEL});
    f.session.dispatch("mask.edit", &edit).unwrap();
    let request = json!({"canvas": C, "mode": "selected", "nodes": [IMAGE, EXPAND, MASK, EDIT]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], 20_000, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != true, "unpriced_jobs": 0, "amounts": amounts});
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
}

/// The decoded image and mask the one provider call was sent.
fn sent(f: &Fixture) -> (RgbaImage, RgbaImage) {
    let requests = f.mock.requests();
    let [job] = requests.as_slice() else { panic!("{} calls", requests.len()) };
    let media = |want: &str| {
        let bytes = job.inputs.iter().find_map(|i| match i {
            JobInput::Media { role, bytes, .. } if role == want => Some(bytes.clone()),
            _ => None,
        });
        nodecraft_media::decode_image(&bytes.unwrap_or_else(|| panic!("no {want}")), &nodecraft_media::Limits::default()).unwrap().1.to_rgba8()
    };
    assert!(job.inputs.iter().any(|i| matches!(i, JobInput::Text { role, .. } if role == "prompt")));
    (media("image"), media("mask"))
}

fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
    image.get_pixel(x, y).0
}

#[test]
fn a_painted_mask_is_quoted_and_sent_with_its_image() {
    let mut f = fixture();
    let strokes = json!([{"size": 0.3, "hardness": 1.0, "points": [[0.25, 0.5]]}]);
    f.session.dispatch("node.mask_set", &json!({"canvas": C, "id": MASK, "strokes": strokes})).unwrap();
    edit_and_run(&mut f, "a red door");
    let (image, mask) = sent(&f);
    assert_eq!((image.width(), image.height(), mask.width(), mask.height()), (40, 30, 40, 30));
    assert_eq!(pixel(&mask, 10, 15), [255, 255, 255, 0], "the painted spot is to change");
    assert_eq!(pixel(&mask, 35, 15), [0, 0, 0, 255], "the rest stays");
}

#[test]
fn margins_alone_outpaint_the_expanded_canvas() {
    let mut f = fixture();
    f.session.dispatch("mask.margins_set", &json!({"canvas": C, "id": EXPAND, "margins": {"left": 0, "top": 0, "right": 20, "bottom": 0}})).unwrap();
    edit_and_run(&mut f, "more beach");
    let (image, mask) = sent(&f);
    assert_eq!((image.width(), image.height(), mask.width(), mask.height()), (60, 30, 60, 30));
    assert_eq!(pixel(&image, 50, 15)[3], 0, "the new margin is transparent");
    assert_eq!(pixel(&image, 10, 15)[3], 255, "the original is placed at the left");
    assert_eq!(pixel(&mask, 50, 15), [255, 255, 255, 0], "the margin is to fill");
    assert_eq!(pixel(&mask, 10, 15), [0, 0, 0, 255], "the original stays");
}
