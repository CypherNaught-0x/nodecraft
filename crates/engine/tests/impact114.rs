//! `Session::impact` (#114) classifies a command by the effect its handler describes, without
//! applying it: reads, document edits, host requests, and runs with what they accepted.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    impact::{Impact, RunImpact},
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::RunHost,
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, Currency, ErrorKind, NodeId, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const C: CanvasId = CanvasId::from_bytes([50; 16]);
const WRITER: NodeId = NodeId::from_bytes([51; 16]);
const MODEL: &str = "fixture.writer";

/// Never runs a task: `impact` must not start anything.
struct Inert;
impl Runtime for Inert {
    fn spawn(&self, _task: TaskFuture<()>) {
        panic!("impact started a task");
    }
    fn sleep(&self, _ms: u64) -> TaskFuture<()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

fn session() -> (Session, RunHost) {
    let catalog = Catalog::from_toml(&format!(
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
"#
    ))
    .unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([52; 16]), "Impact").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let params = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Describe the sea".into()))].into();
    let writer = kinds.get(TEXT_KIND).unwrap().instantiate_instance(WRITER, Default::default(), &params).unwrap();
    Change::AddNode { canvas: C, node: writer }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "impact".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Inert), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(MockProvider::standard())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    (session, host)
}

fn run() -> serde_json::Value {
    json!({"canvas": C, "mode": "selected", "nodes": [WRITER]})
}

#[test]
fn reads_edits_and_runs_are_told_apart_and_nothing_is_applied() {
    let (mut session, host) = session();
    let revision = session.revision();
    assert_eq!(session.impact("node.describe", &json!({"canvas": C, "id": WRITER})).unwrap(), Impact::Read);
    assert_eq!(session.impact("run.quote", &run()).unwrap(), Impact::Read);
    assert_eq!(session.impact("node.title", &json!({"canvas": C, "id": WRITER, "title": "Sea"})).unwrap(), Impact::Edit);
    assert_eq!(session.revision(), revision, "describing an edit doesn't apply it");
    assert!(session.document().canvases().get(&C).unwrap().nodes().get(&WRITER).unwrap().title != "Sea");

    let quote = session.dispatch("run.quote", &run()).unwrap();
    let mut start = run();
    start["accept"] = json!({"token": quote["token"], "amounts": {"USD": 10_000}});
    let impact = session.impact("run.start", &start).unwrap();
    assert_eq!(impact, Impact::Run(RunImpact { ceiling: BTreeMap::from([(Currency::USD, 10_000)]), partial: false, unpriced_jobs: 0 }));
    assert!(!host.is_busy(), "describing a run doesn't start it");
    assert_eq!(session.revision(), revision);
}

#[test]
fn what_dispatch_would_refuse_is_refused() {
    let (session, _host) = session();
    let unknown = session.impact("no.such_command", &json!({})).unwrap_err();
    assert_eq!(unknown.kind, ErrorKind::NotFound);
    let missing = session.impact("node.title", &json!({"canvas": C, "id": NodeId::from_bytes([9; 16]), "title": "x"})).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound, "{missing:?}");
    let stale = {
        let mut start = run();
        start["accept"] = json!({"token": "not-the-quote", "amounts": {"USD": 10_000}});
        session.impact("run.start", &start).unwrap_err()
    };
    assert!(stale.message.contains("quote again"), "{stale:?}");
}
