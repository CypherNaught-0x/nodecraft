//! An over-long text request to a model with a declared context window (#1074) is refused when
//! it is quoted, with the input's size and what to do, before anything is sent: the mock provider
//! sees no request, and the run can't start. A prompt within the window quotes as usual.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::RunHost,
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};

const C: CanvasId = CanvasId::from_bytes([74; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const MODEL: &str = "fixture.short_reader";

/// A text model that reads at most 100 tokens.
fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-text"
label = "Fixture short reader"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
reads = {{ context_tokens = 100 }}
"#
    ))
    .unwrap()
}

/// Each task on its own thread, as the app's: a started run would really reach the mock.
struct Threads;
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        Box::pin(async move { std::thread::sleep(std::time::Duration::from_millis(ms)) })
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

/// A text source holding `text`, wired into the writer's prompt.
fn fixture(text: &str) -> (Session, MockProvider, RunHost) {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([75; 16]), "Context window").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let writer = kinds.get(TEXT_KIND).unwrap().instantiate_instance(WRITER, Default::default(), &[("model".into(), Value::Text(MODEL.into()))].into()).unwrap();
    let source =
        kinds.get("source.text").unwrap().instantiate_instance(SOURCE, Default::default(), &[("text".into(), Value::Text(text.into()))].into()).unwrap();
    Change::AddNode { canvas: C, node: writer }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([3; 16]),
        from: Endpoint { node: SOURCE, port: "out".into() },
        to: Endpoint { node: WRITER, port: "prompt".into() },
        order: 0,
    };
    Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "context-window".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    (session, mock, host)
}

#[test]
fn an_over_long_prompt_is_refused_at_the_quote_and_nothing_is_sent() {
    // 1,000 bytes is at least 167 tokens; the model reads 100.
    let (mut session, mock, mut host) = fixture(&"word ".repeat(200));
    let request = json!({"canvas": C, "mode": "selected", "nodes": [WRITER]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], false, "{quote}");
    assert_eq!(quote["steps"], json!([]), "nothing is priced: {quote}");
    let reason = quote["blocked"].to_string();
    assert!(reason.contains(nodecraft_types::CONTEXT_LIMIT), "{reason}");
    assert!(reason.contains("1000 bytes of text") && reason.contains("at least 167 tokens") && reason.contains("reads at most 100"), "{reason}");
    assert!(reason.contains(nodecraft_types::CONTEXT_LIMIT_FIX), "says what to do: {reason}");
    // Nothing can start it: there is no accepted quote to start from.
    let mut start = request.clone();
    start["accept"] = json!({"token": quote["token"], "partial": false, "unpriced_jobs": 0, "amounts": {}});
    let refused = session.dispatch("run.start", &start).unwrap_err();
    assert!(!refused.message.is_empty(), "{refused:?}");
    // And whatever the host has, it sends nothing.
    for _ in 0..50 {
        host.pump(&mut session);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!host.is_busy());
    assert!(mock.requests().is_empty(), "no provider call");
}

#[test]
fn a_prompt_within_the_window_is_quoted_as_usual() {
    // 600 bytes is at least 100 tokens: at the window, not past it.
    let (mut session, mock, mut host) = fixture(&"x".repeat(600));
    let request = json!({"canvas": C, "mode": "selected", "nodes": [WRITER]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], json!(10_000), "{quote}");
    assert!(mock.requests().is_empty(), "quoting sends nothing");
    // Accepted, it does reach the provider: the runtime above really runs what is started.
    let mut start = request;
    start["accept"] = json!({"token": quote["token"], "partial": false, "unpriced_jobs": 0, "amounts": {"USD": 10_000}});
    session.dispatch("run.start", &start).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while host.is_busy() || mock.requests().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the run never reached the provider");
        host.pump(&mut session);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(mock.requests().len(), 1);
}
