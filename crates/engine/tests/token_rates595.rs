//! A text model's catalog token rates (#595): an authorized run is charged the tokens its reply
//! measured, not its estimate, unless the provider says what it cost. Mock provider only.
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
    cost::{Budgets, Charge, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, Currency, Money, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const MODEL: &str = "fixture.writer";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-text"
label = "Fixture writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
usage = {{ input = "0.0001 USD", output = "0.0005 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
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

/// Runs one writer card on a mock model that reports `tokens` and maybe `cost`; the usage log's
/// charges afterwards.
fn charged(tokens: Option<(u64, u64)>, cost: Option<Money>) -> Vec<(u64, Charge)> {
    let model = MockModel { tokens, cost, ..MockModel::new(MockOutput::Text) };
    let mock = MockProvider::new("mock", [("mock-text".to_owned(), model)]);
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([62; 16]), "Token rates").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("A haiku".into()))].into();
    let node = kinds.get(TEXT_KIND).unwrap().instantiate_instance(WRITER, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let log = Arc::new(Mutex::new(UsageLog::default()));
    let budget = JobBudget { log: Arc::clone(&log), budgets: Arc::new(Mutex::new(Budgets::default())), session: "rates".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock)).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);

    let request = json!({"canvas":C,"mode":"selected","nodes":[WRITER]});
    let q = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["totals"]["USD"]["micros"], 10_000, "quoted from `pricing`: {q}");
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let log = log.lock().unwrap();
    log.entries().map(|e| (e.cost.micros, e.status)).collect()
}

#[test]
fn a_run_is_charged_its_measured_tokens_at_the_catalog_rates() {
    // 1,000 × 0.0001 + 2,000 × 0.0005 USD per 1K tokens: 0.0011 USD, not the 0.01 USD quoted.
    assert_eq!(charged(Some((1_000, 2_000)), None), [(1_100, Charge::Actual)]);
    // What the provider says it cost wins over the rates.
    assert_eq!(charged(Some((1_000, 2_000)), Some(Money::new(Currency::USD, 7))), [(7, Charge::Actual)]);
    // No tokens reported: the estimate.
    assert_eq!(charged(None, None), [(10_000, Charge::Actual)]);
}
