//! A batch collecting another batch (#92): its items are flattened into the collecting batch's,
//! every one is quoted before the run, and a Generate card runs once per item, or once per
//! combination with a third batch. Mock provider only.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
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
use nodecraft_graph::{Canvas, Change, Generation, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::JobInput;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const ANIMALS: NodeId = NodeId::from_bytes([1; 16]);
const SUBJECTS: NodeId = NodeId::from_bytes([2; 16]);
const TIMES: NodeId = NodeId::from_bytes([3; 16]);
const PAINTER: NodeId = NodeId::from_bytes([4; 16]);
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
}

/// Animals (a fox, an owl) → subjects (collects the animals, adds a cat) → a painter's prompt,
/// with a times-of-day batch beside them, not yet wired.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([62; 16]), "Nested batches").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(String::new()))].into();
    let painter = kinds.get(KIND).unwrap().instantiate_instance(PAINTER, Default::default(), &values).unwrap();
    let batch = |id: NodeId, text: &str| {
        let mut batch = kinds.get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
        batch.params.insert("text".into(), Value::Text(text.into()));
        batch
    };
    for node in [batch(ANIMALS, "a fox\nan owl"), batch(SUBJECTS, "a cat"), batch(TIMES, "at dawn\nat night"), painter] {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "nested-batches".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(PAINTER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut f = Fixture { session, host, mock };
    connect(&mut f, 10, (ANIMALS, "item", SUBJECTS, "in"), 0);
    connect(&mut f, 11, (SUBJECTS, "item", PAINTER, "prompt"), 0);
    f
}

fn connect(f: &mut Fixture, e: u8, (from, port, to, to_port): (NodeId, &str, NodeId, &str), order: u32) {
    let edge = json!({"id": EdgeId::from_bytes([e; 16]), "from": {"node": from, "port": port}, "to": {"node": to, "port": to_port}, "order": order});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
}

fn quote(f: &mut Fixture) -> serde_json::Value {
    f.session.dispatch("run.quote", &json!({"canvas":C,"mode":"selected","nodes":[PAINTER]})).unwrap()
}

/// Quotes, checks the quoted price, accepts it, and drives the run until the host is idle.
fn run(f: &mut Fixture, micros: u64) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[PAINTER]});
    let q = quote(f);
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["complete"], true, "every item is known before the run: {q}");
    assert_eq!(q["totals"]["USD"]["micros"], micros, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":amounts});
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

fn shown(f: &Fixture, id: NodeId) -> Generation {
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap();
    node.history.iter().find(|g| Some(g.id) == node.active).cloned().unwrap()
}

/// The prompt of every request sent, in the order sent, sorted (jobs run in parallel).
fn prompts(f: &Fixture) -> Vec<String> {
    let mut prompts: Vec<String> = f
        .mock
        .requests()
        .iter()
        .map(|job| {
            job.inputs.iter().filter_map(|i| if let JobInput::Text { text, .. } = i { Some(text.clone()) } else { None }).collect::<Vec<_>>().join(" | ")
        })
        .collect();
    prompts.sort();
    prompts
}

#[test]
fn a_batch_collecting_a_batch_paints_every_item_and_quotes_each() {
    let mut f = fixture();
    // Three items, each one call at 0.01 USD, quoted before anything is sent.
    run(&mut f, 30_000);
    assert_eq!(prompts(&f), ["a cat", "a fox", "an owl"]);
    let painted = shown(&f, PAINTER);
    assert_eq!(painted.items.len(), 3);
    assert!(painted.items.iter().all(|tag| tag.len() == 1 && tag.first().unwrap().batch == SUBJECTS), "the collecting batch names the items");

    // Editing the collected batch makes the painter stale and changes what the next run sends.
    let stale = |f: &mut Fixture| f.session.dispatch("canvas.stale", &json!({"canvas": C})).unwrap()["nodes"].clone();
    assert_eq!(stale(&mut f), json!([]), "fresh after its run");
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":ANIMALS,"name":"text","value":{"type":"text","value":"a fox\nan owl\na bee"}})).unwrap();
    assert_eq!(stale(&mut f), json!([PAINTER]), "an inner batch is part of the painter's recipe");
    let q = quote(&mut f);
    assert_eq!(q["totals"]["USD"]["micros"], 40_000, "{q}");

    // A third batch pairs with the collecting one by Cross: every combination, all quoted.
    connect(&mut f, 12, (TIMES, "item", PAINTER, "prompt"), 1);
    let q = quote(&mut f);
    assert_eq!(q["totals"]["USD"]["micros"], 80_000, "4 subjects × 2 times: {q}");
}

#[test]
fn a_nested_batch_over_the_limit_is_refused_before_anything_is_sent() {
    let mut f = fixture();
    let many: String = (0..nodecraft_nodes::batch::MAX_ITEMS).map(|n| format!("animal {n}\n")).collect();
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":ANIMALS,"name":"text","value":{"type":"text","value":many}})).unwrap();
    let q = quote(&mut f);
    assert_eq!(q["allowed"], false, "{q}");
    assert!(q.to_string().contains("at most 1000 items"), "{q}");
    assert!(f.mock.requests().is_empty());
}
