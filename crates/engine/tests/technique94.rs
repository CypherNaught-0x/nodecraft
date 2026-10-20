//! A placed technique (#94) runs end to end: quoted on the technique node, its version's
//! nodes inlined and driven through the run host and the mock provider, and only the technique
//! node's result recorded. No live calls.
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
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, TechniqueId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([40; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const VERSION: CanvasId = CanvasId::from_bytes([42; 16]);
const PLACED: NodeId = NodeId::from_bytes([5; 16]);
const MOON: NodeId = NodeId::from_bytes([6; 16]);
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
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .unwrap()
}

/// Each task on its own thread; sleeps on a timer thread. Real concurrency, like the app's.
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

/// Source → writer on C, published as a technique (Subject in, Answer out), placed on C again and
/// fed by its own source asking about the moon.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([41; 16]), "Technique run").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let writer = kinds.get(TEXT_KIND).unwrap().instantiate_instance(WRITER, Default::default(), &[("model".into(), Value::Text(MODEL.into()))].into()).unwrap();
    Change::AddNode { canvas: C, node: writer }.apply(&mut project).unwrap();
    for (id, text) in [(SOURCE, "Describe the sea"), (MOON, "Describe the moon")] {
        let source =
            kinds.get("source.text").unwrap().instantiate_instance(id, Default::default(), &[("text".into(), Value::Text(text.into()))].into()).unwrap();
        Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
    }
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
        session: "technique-run".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing(&mock, &[WRITER])).unwrap();
    host.install(&mut session);
    let publish = json!({"canvas": C, "published": VERSION, "seed": NodeId::from_bytes([7; 16]), "technique": TechniqueId::from_bytes([8; 16]), "name": "Describe",
        "inputs": [{"node": SOURCE, "name": "Subject"}], "outputs": [{"node": WRITER, "name": "Answer"}]});
    session.dispatch("technique.publish", &publish).unwrap();
    session.dispatch("technique.place", &json!({"canvas": C, "version": VERSION, "id": PLACED, "pos": {"x": 400.0, "y": 0.0}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([9; 16]), "from": {"node": MOON, "port": "out"}, "to": {"node": PLACED, "port": "subject"}, "order": 0});
    session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    // Routes are per node, so a host routes the nodes inlined for the technique node too.
    let canvas = session.document().canvases().get(&C).unwrap();
    let expanded = nodecraft_exec::technique::expand(session.document().canvases(), canvas, session.kinds()).unwrap();
    let inner: Vec<NodeId> = expanded.inner.keys().copied().collect();
    assert_eq!(inner.len(), 2, "the version's source and writer");
    session.set_provider_routing(routing(&mock, &[[WRITER].as_slice(), &inner].concat())).unwrap();
    Fixture { session, host, mock }
}

fn routing(mock: &MockProvider, nodes: &[NodeId]) -> ProviderRouting {
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    for node in nodes {
        routing.select(*node, "mock", binding.target()).unwrap();
    }
    routing
}

fn drive(f: &mut Fixture) -> Vec<RunNotice> {
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            return notices;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn node(s: &Session, id: NodeId) -> nodecraft_graph::Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap().clone()
}

#[test]
fn a_placed_technique_runs_its_version_and_records_only_its_own_result() {
    let mut f = fixture();
    let request = json!({"canvas": C, "mode": "with_upstream", "nodes": [PLACED]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], json!(10_000), "one paid call inside: {q}");
    let priced: Vec<NodeId> = q["steps"].as_array().unwrap().iter().map(|step| serde_json::from_value(step["node"].clone()).unwrap()).collect();
    assert!(priced.iter().all(|id| [MOON, PLACED].contains(id)), "inner steps are priced on the technique node: {q}");
    assert!(priced.contains(&PLACED));
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": false, "unpriced_jobs": 0, "amounts": amounts});
    f.session.dispatch("run.start", &start).unwrap();
    let notices = drive(&mut f);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: PLACED }), "{notices:?}");
    let placed = node(&f.session, PLACED);
    assert_eq!(placed.history.len(), 1);
    assert!(matches!(&placed.history[0].outputs[0], Value::Text(t) if t.starts_with("mock-text: Describe the moon #")), "{:?}", placed.history);
    assert_eq!(f.mock.requests().len(), 1, "one paid call");
    // The writer the technique was published from is untouched, and nothing inlined was added.
    assert!(node(&f.session, WRITER).history.is_empty());
    assert_eq!(f.session.document().canvases().get(&C).unwrap().nodes().len(), 4);
    assert!(f.session.document().canvases().get(&VERSION).unwrap().nodes().values().all(|n| n.kind != TEXT_KIND || n.history.is_empty()));
    // Now fresh, it isn't planned again.
    let again = f.session.dispatch("run.quote", &json!({"canvas": C, "mode": "all_stale"})).unwrap();
    let stale: Vec<NodeId> = again["steps"].as_array().unwrap().iter().map(|step| serde_json::from_value(step["node"].clone()).unwrap()).collect();
    assert!(!stale.contains(&PLACED), "{again}");
}

#[test]
fn a_removed_version_blocks_only_its_technique_node() {
    let mut f = fixture();
    f.session.dispatch("technique.remove", &json!({"canvas": VERSION})).unwrap();
    let q = f.session.dispatch("run.quote", &json!({"canvas": C, "mode": "with_upstream", "nodes": [PLACED]})).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    assert!(!q["blocked"].as_array().unwrap().is_empty(), "{q}");
    let fine = f.session.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "nodes": [SOURCE, WRITER]})).unwrap();
    assert_eq!(fine["allowed"], true, "{fine}");
}
