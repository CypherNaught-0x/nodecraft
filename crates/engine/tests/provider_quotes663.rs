//! Provider quotes before a run (#663): `run.estimate` asks the provider what each by-usage job
//! costs, without an idempotency key or a charge; `run.quote` then prices a step every job of
//! which was quoted, budgets check that price, and a step whose quote failed stays unknown.
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
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::ErrorKind;
use nodecraft_providers::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, Currency, Money, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([60; 16]);
const QUOTED: NodeId = NodeId::from_bytes([61; 16]);
const FAILING: NodeId = NodeId::from_bytes([62; 16]);
const PRICE: Money = Money { currency: Currency::USD, micros: 250_000 };

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
        u64::try_from(self.0.elapsed().as_millis()).unwrap()
    }
}

/// Pumps until `done`, returning the notices.
fn pump_until(f: &mut Fixture, done: impl Fn(&RunHost) -> bool) -> Vec<RunNotice> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if done(&f.host) {
            return notices;
        }
        assert!(Instant::now() < deadline, "never done: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn model(id: &str) -> String {
    format!(
        r#"[[model]]
id = "fixture.{id}"
provider = "mock"
provider_model = "{id}"
label = "Fixture {id}"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "by_usage", currency = "USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    )
}

struct Fixture {
    session: Session,
    host: RunHost,
    mock: MockProvider,
}

fn fixture(caps: Budgets) -> Fixture {
    let catalog = Catalog::from_toml(&format!("{}{}", model("usage-quoted"), model("usage-failing"))).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([63; 16]), "Quotes").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (node, model) in [(QUOTED, "fixture.usage-quoted"), (FAILING, "fixture.usage-failing")] {
        let params = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Describe the sea".into()))].into();
        let card = kinds.get(TEXT_KIND).unwrap().instantiate_instance(node, Default::default(), &params).unwrap();
        Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(caps)),
        session: "quotes".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    host.register_estimates(&mut services).unwrap();
    let mock = MockProvider::new("mock", ["usage-quoted", "usage-failing"].map(|id| (id.to_owned(), MockModel::new(MockOutput::Text))))
        .with_quote("usage-quoted", Ok(PRICE))
        .with_quote("usage-failing", Err(ErrorKind::Busy));
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    for node in [QUOTED, FAILING] {
        routing.select(node, "mock", binding.target()).unwrap();
    }
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let _ = host.pump(&mut session);
    Fixture { session, host, mock }
}

fn run() -> serde_json::Value {
    json!({"canvas": C, "mode": "selected", "nodes": [QUOTED, FAILING]})
}

fn step(quote: &serde_json::Value, node: NodeId) -> &serde_json::Value {
    quote["steps"].as_array().unwrap().iter().find(|s| s["node"] == json!(node)).unwrap()
}

/// Asks for prices, returning the request's id and the notices.
fn estimate(f: &mut Fixture) -> (u64, Vec<RunNotice>) {
    let answer = f.session.dispatch("run.estimate", &run()).unwrap();
    assert_eq!(answer["accepted"], true);
    assert!(f.host.is_estimating());
    (answer["request"].as_u64().unwrap(), pump_until(f, |host| !host.is_estimating()))
}

#[test]
fn a_quoted_step_is_priced_and_a_failed_quote_stays_unknown() {
    let mut f = fixture(Budgets::default());
    let before = f.session.dispatch("run.quote", &run()).unwrap();
    assert_eq!(before["quoted"], json!([]));
    assert!(step(&before, QUOTED)["price"].is_null());
    assert_eq!(before["complete"], false);

    let (request, notices) = estimate(&mut f);
    assert_eq!(notices, vec![RunNotice::Estimated { request, canvas: C, quoted: 1, unquoted: 1 }]);
    let asked = f.mock.quoted();
    assert_eq!(asked.len(), 2, "each by-usage job is asked once");
    assert!(asked.iter().all(|job| job.idempotency_key.is_empty()), "an estimate uses no idempotency key");
    assert!(f.mock.requests().is_empty(), "an estimate submits nothing");
    assert_eq!(f.mock.billed(), 0);
    assert!(!f.host.is_busy(), "an estimate starts no run");

    let after = f.session.dispatch("run.quote", &run()).unwrap();
    assert_eq!(after["quoted"], json!([QUOTED]));
    assert_eq!(step(&after, QUOTED)["price"]["micros"], PRICE.micros);
    assert!(step(&after, FAILING)["price"].is_null(), "a failed quote leaves the step unknown before execution");
    assert_eq!(after["totals"]["USD"]["micros"], PRICE.micros);
    assert_eq!(after["complete"], false);
    assert_ne!(before["token"], after["token"], "new quotes invalidate the earlier token");

    // The earlier token no longer starts the run.
    let mut stale = run();
    stale["accept"] = json!({"token": before["token"], "partial": true, "unpriced_jobs": 2, "amounts": {}});
    assert!(f.session.dispatch("run.start", &stale).is_err());

    // The quoted price is the run's ceiling for that step; the run sends a keyed job.
    let mut start = run();
    start["accept"] = json!({"token": after["token"], "partial": true, "unpriced_jobs": 1, "amounts": {"USD": PRICE.micros}});
    f.session.dispatch("run.start", &start).unwrap();
    pump_until(&mut f, |host| !host.is_busy());
    let sent = f.mock.requests();
    assert!(!sent.is_empty());
    assert!(sent.iter().all(|job| !job.idempotency_key.is_empty()));
}

#[test]
fn budgets_check_the_quoted_price() {
    let mut f = fixture(Budgets { per_call: Some(Money { currency: Currency::USD, micros: 100_000 }), ..Default::default() });
    let (request, notices) = estimate(&mut f);
    assert_eq!(notices, vec![RunNotice::Estimated { request, canvas: C, quoted: 1, unquoted: 1 }]);
    let quote = f.session.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "nodes": [QUOTED]})).unwrap();
    assert_eq!(quote["quoted"], json!([QUOTED]));
    assert_eq!(quote["allowed"], false, "a quoted price over the per-call cap is refused: {quote}");
    assert!(!quote["blocks"].as_array().unwrap().is_empty());
}

#[test]
fn asking_again_asks_again_and_a_changed_card_is_not_priced() {
    let mut f = fixture(Budgets::default());
    let (first, _) = estimate(&mut f);
    let (request, notices) = estimate(&mut f);
    assert_ne!(first, request, "each request has its own id");
    assert_eq!(notices, vec![RunNotice::Estimated { request, canvas: C, quoted: 1, unquoted: 1 }]);
    assert_eq!(f.mock.quoted().len(), 4);
    assert_eq!(f.session.dispatch("run.quote", &run()).unwrap()["quoted"], json!([QUOTED]));
    // Another prompt is another job: its old quote doesn't price it.
    f.session
        .dispatch("node.param_set", &json!({"canvas": C, "id": QUOTED, "name": "prompt", "value": {"type": "text", "value": "Describe the hills"}}))
        .unwrap();
    let quote = f.session.dispatch("run.quote", &run()).unwrap();
    assert_eq!(quote["quoted"], json!([]));
    assert!(step(&quote, QUOTED)["price"].is_null());
}
