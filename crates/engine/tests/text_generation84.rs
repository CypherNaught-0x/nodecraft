//! Text generation cards (#84) through quote, admission, the Driver, the mock provider and
//! run.record. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generate,
    generation_catalog::GenerationCatalog,
    jobs::{CompletionDelivery, JobBudget, ProviderRunner},
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    run::RunOptions,
    scheduler::{JobId, JobState, Scheduler},
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::{KIND, TEXT_KIND};
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, Modality, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const C: CanvasId = CanvasId::from_bytes([84; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const IMAGE: NodeId = NodeId::from_bytes([4; 16]);
const MODEL: &str = "fixture.writer";

/// A text model with a text-only mode and an image-reading mode, served by the mock, plus a
/// second text model and an image model to swap between.
fn catalog() -> Catalog {
    let mut catalog = Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-text"
label = "Fixture writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
[[model.modes]]
id = "image-to-text"
output = "text"
inputs = [
  {{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }},
  {{ role = "image", modality = "image", min = 1, max = 4 }},
]
[[model.params.params]]
name = "tone"
label = "Tone"
control = {{ type = "text", multiline = false, max_bytes = 64 }}
default = {{ type = "text", value = "plain" }}
"#
    ))
    .unwrap();
    for (id, output) in [("fixture.editor", "text"), ("fixture.painter", "image")] {
        let other = Catalog::from_toml(&format!(
            r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "mock-{output}"
label = "Fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "free" }}
[[model.modes]]
id = "text-to-{output}"
output = "{output}"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
        ))
        .unwrap();
        catalog.merge(other).unwrap();
    }
    catalog
}

#[derive(Default)]
struct Immediate(Arc<AtomicU64>);
impl Runtime for Immediate {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let w = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_ready());
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let t = self.0.clone();
        Box::pin(async move {
            t.fetch_add(ms, Ordering::SeqCst);
        })
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    session: Session,
    captured: Arc<Mutex<Vec<PreparedRun>>>,
    mock: MockProvider,
}

fn wire(project: &mut Project, id: u8, from: NodeId, to: NodeId, port: &str) {
    let edge =
        Edge { id: EdgeId::from_bytes([id; 16]), from: Endpoint { node: from, port: "out".into() }, to: Endpoint { node: to, port: port.into() }, order: 0 };
    Change::Connect { canvas: C, edge }.apply(project).unwrap();
}

/// A writer card with a local prompt; `connected` wires a text source into its prompt and
/// `image` an image source into its image role.
fn fixture(connected: Option<&str>, image: bool) -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([85; 16]), "Text generation").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let node = kinds
        .get(TEXT_KIND)
        .unwrap()
        .instantiate_instance(
            WRITER,
            Default::default(),
            &[("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Write a haiku".into()))].into(),
        )
        .unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    if let Some(text) = connected {
        let source =
            kinds.get("source.text").unwrap().instantiate_instance(SOURCE, Default::default(), &[("text".into(), Value::Text(text.into()))].into()).unwrap();
        Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
        wire(&mut project, 3, SOURCE, WRITER, "prompt");
    }
    if image {
        let source = kinds.get("source.image").unwrap().instantiate_instance(IMAGE, Default::default(), &Default::default()).unwrap();
        Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
        wire(&mut project, 5, IMAGE, WRITER, "image");
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    let captured: Arc<Mutex<Vec<PreparedRun>>> = Arc::default();
    let sink = captured.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |p| {
            sink.lock().unwrap().push(p);
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    let mock = MockProvider::standard();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    Fixture { session, captured, mock }
}

fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[WRITER]})
}

fn writer(s: &Session) -> nodecraft_graph::Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&WRITER).unwrap().clone()
}

/// Quote, accept, drive the one job through the mock and record it; returns the answer.
fn run(f: &mut Fixture) -> Value {
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], 10_000);
    let mut start = request();
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    f.session.dispatch("run.start", &start).unwrap();
    let prepared = f.captured.lock().unwrap().pop().unwrap();
    let (mut run, progress) =
        f.session.authorize_prepared_run(prepared, RunOptions { id: [86; 16], first_job: 1, limits: Default::default(), retry: Default::default() }).unwrap();
    assert_eq!(progress.jobs.len(), 1);
    let spec = &progress.jobs[0];
    let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
    let deliveries: Arc<Mutex<BTreeMap<JobId, CompletionDelivery>>> = Arc::default();
    let out = deliveries.clone();
    let runtime = Arc::new(Immediate::default());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "text-fixture".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let runner = Arc::new(
        ProviderRunner::new(
            runtime.clone(),
            Default::default(),
            Arc::new(move |id, d| {
                assert!(out.lock().unwrap().insert(id, d).is_none());
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(budget.clone()),
    );
    runner.register(spec.id, queued);
    let mut scheduler = Scheduler::new(Default::default());
    scheduler.add(progress.jobs.clone()).unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let end = futures::executor::block_on(driver.run(runner, runtime, |_| {}));
    assert_eq!(end.state(spec.id), Some(&JobState::Done));
    let delivery = deliveries.lock().unwrap().remove(&spec.id).unwrap();
    let nodecraft_nodes::JobSpec::Generate { outputs: routes, .. } = run.work(spec.id).unwrap() else { panic!("not a generation job") };
    assert_eq!(routes[0].modality, Modality::Text);
    let converted = generate::produced(routes, &delivery.completed).unwrap();
    assert!(converted.assets.is_empty(), "text answers are values, not assets");
    let done = run.job_done(f.session.kinds(), spec.id, converted.outputs);
    assert!(done.failed.is_empty());
    let record = json!({"canvas":C,"results":[{"node":WRITER,"generation":done.finished[0].generation}]});
    f.session.dispatch("run.record", &record).unwrap();
    assert_eq!(budget.log.lock().unwrap().entries().count(), 1, "the paid call is accounted");
    writer(&f.session).history[0].outputs[0].clone()
}

fn answer(value: &Value) -> &str {
    match value {
        Value::Text(text) => text,
        other => panic!("expected one text answer, got {other:?}"),
    }
}

#[test]
fn the_text_kind_is_registered_beside_the_image_kind_and_answers_in_text() {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let text = kinds.get(TEXT_KIND).unwrap().descriptor();
    assert_eq!(text.label, "Generate text");
    assert_eq!(text.outputs[0].port_type.modality, Modality::Text);
    assert!(!text.outputs[0].port_type.many, "one answer per run");
    assert!(kinds.get(KIND).is_ok(), "the image kind stays registered");
    // The text model is a text card's model, not an image card's.
    assert!(snapshot.model(TEXT_KIND, MODEL).is_some());
    assert!(snapshot.model(KIND, MODEL).is_none());
    assert_eq!(snapshot.models(TEXT_KIND).map(|m| m.id.as_str()).collect::<Vec<_>>(), ["fixture.editor", MODEL]);
    assert_eq!(snapshot.models(KIND).map(|m| m.id.as_str()).collect::<Vec<_>>(), ["fixture.painter"]);
    assert!(GenerationCatalog::is_model_selector(TEXT_KIND, "model"));
}

#[test]
fn a_text_card_runs_through_the_mock_and_records_its_answer() {
    let mut f = fixture(None, false);
    let value = run(&mut f);
    assert!(answer(&value).starts_with("mock-text: Write a haiku #"), "{value:?}");
    let requests = f.mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "mock-text");
    // The answer is undoable like any other result.
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(writer(&f.session).history.is_empty());
}

#[test]
fn connected_text_replaces_the_local_prompt_and_keeps_the_draft() {
    let mut f = fixture(Some("Describe the sea"), false);
    let value = run(&mut f);
    assert!(answer(&value).starts_with("mock-text: Describe the sea #"), "{value:?}");
    assert!(!answer(&value).contains("haiku"), "the local prompt is replaced, not appended");
    // The locked local draft is kept for when the wire goes.
    assert_eq!(writer(&f.session).params.get("prompt"), Some(&Value::Text("Write a haiku".into())));
}

#[test]
fn an_empty_prompt_is_refused_before_any_provider_work() {
    let mut f = fixture(None, false);
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":WRITER,"name":"prompt","value":{"type":"text","value":"   "}})).unwrap();
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    assert!(q["steps"].as_array().is_some_and(Vec::is_empty), "{q}");
    assert!(f.mock.requests().is_empty());
}

#[test]
fn a_wired_image_is_refused_before_paid_submission() {
    let mut f = fixture(None, true);
    assert!(writer(&f.session).inputs.iter().any(|port| port.name == "image"), "the image-to-text mode declares an image port");
    // Media reaches a text model only once the host resolves it; until then the quote blocks.
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    assert!(q["steps"].as_array().is_some_and(Vec::is_empty), "{q}");
    assert!(f.mock.requests().is_empty());
}

#[test]
fn a_model_swap_never_mixes_image_and_text_cards() {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([87; 16]), "Mixed swap").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let painter = NodeId::from_bytes([6; 16]);
    for (id, kind, model) in [(WRITER, TEXT_KIND, MODEL), (painter, KIND, "fixture.painter")] {
        let node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &[("model".into(), Value::Text(model.into()))].into()).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    let mixed = session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[WRITER, painter],"model":"fixture.editor"}));
    assert!(mixed.is_err_and(|e| e.message.contains("one generation kind")));
    // A text card swaps to another text model, and never to an image model.
    session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[WRITER],"model":"fixture.editor"})).unwrap();
    assert!(session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[WRITER],"model":"fixture.painter"})).is_err());
}
