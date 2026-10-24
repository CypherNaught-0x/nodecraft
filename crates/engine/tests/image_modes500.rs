//! Image card modes (#500): one Generate card runs text to image with nothing wired and edits
//! from its reference images once they are wired, sending them in their order, and in the new
//! order after `edge.reorder`. Mock provider only.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
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
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::JobInput;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const CARD: NodeId = NodeId::from_bytes([3; 16]);
const FIRST: NodeId = NodeId::from_bytes([4; 16]);
const SECOND: NodeId = NodeId::from_bytes([5; 16]);
const MODEL: &str = "fixture.painter";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-image"
label = "Fixture painter"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
[[model.modes]]
id = "edit"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "image", modality = "image", min = 1, max = 8 }}]
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
    /// Each picture's bytes and its shade.
    pictures: Vec<(Arc<[u8]>, u8)>,
}

/// One Generate card and two imported pictures, nothing wired yet.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([62; 16]), "Image modes").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("A lighthouse".into()))].into();
    let card = kinds.get(KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "image-modes".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(CARD, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut pictures = Vec::new();
    for (id, shade) in [(FIRST, 10u8), (SECOND, 240)] {
        let png = nodecraft_media::encode_png(4, 4, &[shade; 4 * 4 * 4], &Default::default()).unwrap();
        import_files(&mut session, C, Position { x: -300.0, y: f32::from(shade) }, 200.0, &[Incoming { name: "ref.png", bytes: &png }], &[id]).unwrap();
        pictures.push((Arc::from(png), shade));
    }
    Fixture { session, host, mock, pictures }
}

fn run(f: &mut Fixture) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], 10_000, "every mode is quoted before it runs: {q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":q["complete"] != true,"unpriced_jobs":0,"amounts":amounts});
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

/// The last request's reference images, by the shade of their bytes' source.
fn last_references(f: &Fixture) -> Vec<u8> {
    let requests = f.mock.requests();
    let job = requests.last().unwrap();
    job.inputs
        .iter()
        .filter_map(|input| match input {
            JobInput::Media { role, bytes, .. } if role == "image" => f.pictures.iter().find(|(b, _)| b == bytes).map(|(_, shade)| *shade),
            _ => None,
        })
        .collect()
}

#[test]
fn the_wired_references_choose_the_mode_and_reach_the_model_in_their_order() {
    let mut f = fixture();
    run(&mut f);
    assert_eq!(f.mock.requests().len(), 1);
    assert!(last_references(&f).is_empty(), "text to image: no reference is sent");

    let edges = [EdgeId::from_bytes([20; 16]), EdgeId::from_bytes([21; 16])];
    for (order, (edge, from)) in edges.iter().zip([FIRST, SECOND]).enumerate() {
        let edge = json!({"id": edge, "from": {"node": from, "port": "out"}, "to": {"node": CARD, "port": "image"}, "order": order});
        f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    run(&mut f);
    assert_eq!(last_references(&f), vec![10, 240], "edit: the references in wire order");

    let to = json!({"node": CARD, "port": "image"});
    f.session.dispatch("edge.reorder", &json!({"canvas": C, "to": to, "edges": [edges[1], edges[0]]})).unwrap();
    run(&mut f);
    assert_eq!(last_references(&f), vec![240, 10], "a reorder reaches the model");
    assert_eq!(f.mock.requests().len(), 3);
}
