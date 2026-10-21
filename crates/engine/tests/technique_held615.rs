//! A technique's inner paid results are held when its technique node's record is refused, or when
//! it fails (#615): a writer and an editor inlined from a technique both finish, the technique node
//! is removed before it records, and both paid results are held, to be put on the canvas or
//! discarded. A writer that finished before its editor failed, or a branch that finished after
//! another failed, is held too. None of this calls the provider again. A canned provider only; no
//! live calls.
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
use nodecraft_providers::{BoxFuture, Completed, ErrorKind, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, TechniqueId, Value};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const VERSION: CanvasId = CanvasId::from_bytes([62; 16]);
const SOURCE: NodeId = NodeId::from_bytes([1; 16]);
const WRITER: NodeId = NodeId::from_bytes([2; 16]);
const EDITOR: NodeId = NodeId::from_bytes([3; 16]);
const PLACED: NodeId = NodeId::from_bytes([5; 16]);
const MOON: NodeId = NodeId::from_bytes([6; 16]);
const MODEL: &str = "fixture.writer";
/// A second model, so the canned provider can tell two branches with one prompt apart.
const ASIDE: &str = "fixture.aside";

fn catalog() -> Catalog {
    let model = |id: &str, provider_model: &str| {
        format!(
            r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "{provider_model}"
label = "Fixture {provider_model}"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
        )
    };
    Catalog::from_toml(&format!("{}{}", model(MODEL, "canned-text"), model(ASIDE, "canned-aside"))).unwrap()
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

/// Answers a prompt with "edited: <prompt>" once it starts with "draft", and with "draft: <prompt>"
/// otherwise, counting requests. A call whose provider model or prompt starts with `stuck` keeps
/// running until it is cleared; one matching `fails` fails for good.
#[derive(Clone, Default)]
struct Canned {
    requests: Arc<AtomicUsize>,
    stuck: Arc<Mutex<Option<String>>>,
    fails: Arc<Mutex<Option<String>>>,
}

/// Whether `rule` picks the call `call` (`<provider model>|<prompt>`).
fn picks(rule: &Mutex<Option<String>>, call: &str) -> bool {
    let (model, prompt) = call.split_once('|').unwrap_or_default();
    rule.lock().unwrap().as_deref().is_some_and(|rule| model.starts_with(rule) || prompt.starts_with(rule))
}

impl Canned {
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
    fn waits(&self, call: &str) -> bool {
        picks(&self.stuck, call)
    }
    fn result(call: &str) -> Completed {
        let prompt = call.split_once('|').map_or(call, |(_, prompt)| prompt);
        let answer = if prompt.starts_with("draft") { format!("edited: {prompt}") } else { format!("draft: {prompt}") };
        Completed::new(vec![Output::Text(answer)])
    }
}

/// `<provider model>|<prompt>`.
fn call_of(job: &ProviderJob) -> String {
    let prompt = job
        .inputs
        .iter()
        .find_map(|i| match i {
            nodecraft_providers::JobInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    format!("{}|{prompt}", job.model)
}

impl Provider for Canned {
    fn id(&self) -> &str {
        "mock"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let call = call_of(job);
        if picks(&self.fails, &call) {
            return Box::pin(async { Err(ProviderError::new(ErrorKind::BlockedContent, "the fixture refuses this call")) });
        }
        let submission = if self.waits(&call) {
            Submission::Queued(JobHandle { remote_id: call, data: serde_json::Value::Null })
        } else {
            Submission::Finished(Self::result(&call))
        };
        Box::pin(async move { Ok(submission) })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let progress = if self.waits(&handle.remote_id) {
            JobProgress::Running { fraction: None, message: None, partial: None }
        } else {
            JobProgress::Done(Self::result(&handle.remote_id))
        };
        Box::pin(async move { Ok(progress) })
    }
    fn cancel<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
    fn poll_interval_ms(&self, _progress: &JobProgress) -> u64 {
        5
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    canned: Canned,
}

/// Source → writer → editor on C, published as a technique (Subject in, Answer out), placed on C
/// again and fed by its own source asking about the moon.
fn fixture() -> Fixture {
    fixture_with(false)
}

/// [`fixture`], or, `parallel`, with the editor beside the writer instead of after it: it reads
/// the source with the other model, and the technique answers with both.
fn fixture_with(parallel: bool) -> Fixture {
    let canned = Canned::default();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([63; 16]), "Technique held").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut out = String::new();
    for (id, model) in [(WRITER, MODEL), (EDITOR, if parallel { ASIDE } else { MODEL })] {
        let model = [("model".into(), Value::Text(model.into()))].into();
        let writer = kinds.get(TEXT_KIND).unwrap().instantiate_instance(id, Default::default(), &model).unwrap();
        out = writer.outputs[0].name.clone();
        Change::AddNode { canvas: C, node: writer }.apply(&mut project).unwrap();
    }
    for (id, text) in [(SOURCE, "Describe the sea"), (MOON, "Describe the moon")] {
        let source =
            kinds.get("source.text").unwrap().instantiate_instance(id, Default::default(), &[("text".into(), Value::Text(text.into()))].into()).unwrap();
        Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
    }
    let editor_reads = if parallel { (SOURCE, "out") } else { (WRITER, out.as_str()) };
    for (n, (from, port, to)) in [(SOURCE, "out", WRITER), (editor_reads.0, editor_reads.1, EDITOR)].into_iter().enumerate() {
        let edge = Edge {
            id: EdgeId::from_bytes([10 + n as u8; 16]),
            from: Endpoint { node: from, port: port.into() },
            to: Endpoint { node: to, port: "prompt".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "technique-held".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing(&canned, &[WRITER, EDITOR])).unwrap();
    host.install(&mut session);
    let outputs =
        if parallel { json!([{"node": WRITER, "name": "Answer"}, {"node": EDITOR, "name": "Aside"}]) } else { json!([{"node": EDITOR, "name": "Answer"}]) };
    let publish = json!({"canvas": C, "published": VERSION, "seed": NodeId::from_bytes([7; 16]), "technique": TechniqueId::from_bytes([8; 16]), "name": "Describe",
        "inputs": [{"node": SOURCE, "name": "Subject"}], "outputs": outputs});
    session.dispatch("technique.publish", &publish).unwrap();
    session.dispatch("technique.place", &json!({"canvas": C, "version": VERSION, "id": PLACED, "pos": {"x": 400.0, "y": 0.0}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([9; 16]), "from": {"node": MOON, "port": "out"}, "to": {"node": PLACED, "port": "subject"}, "order": 0});
    session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    // Routes are per node, so the nodes inlined for the technique node are routed too.
    let canvas = session.document().canvases().get(&C).unwrap();
    let expanded = nodecraft_exec::technique::expand(session.document().canvases(), canvas, session.kinds()).unwrap();
    let inner: Vec<NodeId> = expanded.inner.keys().copied().collect();
    assert_eq!(inner.len(), 3, "the version's source, writer and editor");
    session.set_provider_routing(routing(&canned, &[[WRITER, EDITOR].as_slice(), &inner].concat())).unwrap();
    Fixture { session, host, canned }
}

fn routing(canned: &Canned, nodes: &[NodeId]) -> ProviderRouting {
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(canned.clone())).unwrap();
    for node in nodes {
        routing.select(*node, "mock", binding.target()).unwrap();
    }
    routing
}

/// Quotes and accepts a run of the technique node and what feeds it.
fn start(f: &mut Fixture) {
    let request = json!({"canvas": C, "mode": "with_upstream", "nodes": [PLACED]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != true, "unpriced_jobs": 0, "amounts": amounts});
    f.session.dispatch("run.start", &start).unwrap();
}

/// Pumps like a frame loop until `done`.
fn pump_until(f: &mut Fixture, what: &str, done: impl Fn(&Fixture) -> bool) -> Vec<RunNotice> {
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(f) {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline, "{what}: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    notices
}

/// Runs the technique node with the editor's call held up until the writer's result has been in
/// for a few frames, removes the technique node, then lets the editor answer.
fn run_then_remove_the_technique(f: &mut Fixture) -> Vec<RunNotice> {
    *f.canned.stuck.lock().unwrap() = Some("draft".into());
    start(f);
    let mut notices = pump_until(f, "the editor was never sent", |f| f.canned.requests() == 2);
    for _ in 0..5 {
        notices.extend(f.host.pump(&mut f.session));
    }
    f.session.dispatch("node.remove", &json!({"canvas": C, "id": PLACED})).unwrap();
    *f.canned.stuck.lock().unwrap() = None;
    notices.extend(pump_until(f, "the run never finished", |f| !f.host.is_busy()));
    notices
}

/// The text of every `source.text` card on C but the two the fixture made.
fn placed(f: &Fixture) -> Vec<String> {
    let canvas = f.session.document().canvases().get(&C).unwrap();
    let mut texts: Vec<String> = canvas
        .nodes()
        .values()
        .filter(|n| n.kind == "source.text" && ![SOURCE, MOON].contains(&n.id))
        .filter_map(|n| match n.params.get("text") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    texts.sort();
    texts
}

#[test]
fn a_recorded_technique_node_holds_nothing() {
    let mut f = fixture();
    start(&mut f);
    let notices = pump_until(&mut f, "the run never finished", |f| !f.host.is_busy());
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: PLACED }), "{notices:?}");
    assert!(f.host.held_results().is_empty(), "{:?}", f.host.held_results());
}

#[test]
fn a_refused_technique_record_holds_every_inner_paid_result() {
    let mut f = fixture();
    let notices = run_then_remove_the_technique(&mut f);
    let failed = notices.iter().find_map(|n| match n {
        RunNotice::Failed { node, reason, .. } if *node == PLACED => Some(reason.clone()),
        _ => None,
    });
    assert!(failed.as_deref().is_some_and(|reason| reason.contains("could not be saved") && reason.contains("held")), "{notices:?}");
    let held = f.host.held_results();
    assert_eq!(held.len(), 2, "the writer's and the editor's: {held:?}");
    assert!(held.iter().all(|h| h.canvas == C && h.node == PLACED && h.outputs == 1 && !h.retry), "placed or discarded, never retried alone: {held:?}");
    assert_eq!(f.canned.requests(), 2);

    for view in &held {
        f.session.dispatch("run.held_place", &json!({"id": view.id})).unwrap();
    }
    let notices = pump_until(&mut f, "placing never settled", |f| f.host.held_results().is_empty());
    assert_eq!(notices.iter().filter(|n| matches!(n, RunNotice::Placed { canvas: C, node: PLACED })).count(), 2, "{notices:?}");
    assert_eq!(placed(&f), vec!["draft: Describe the moon".to_owned(), "edited: draft: Describe the moon".to_owned()]);
    assert_eq!(f.canned.requests(), 2, "nothing is sent again");
}

#[test]
fn a_refused_technique_records_inner_results_can_be_discarded() {
    let mut f = fixture();
    run_then_remove_the_technique(&mut f);
    let held = f.host.held_results();
    assert_eq!(held.len(), 2, "{held:?}");
    for view in &held {
        f.session.dispatch("run.held_discard", &json!({"id": view.id})).unwrap();
    }
    pump_until(&mut f, "discarding never settled", |f| f.host.held_results().is_empty());
    assert!(placed(&f).is_empty());
    assert_eq!(f.canned.requests(), 2);
}

fn failed(notices: &[RunNotice]) -> Option<String> {
    notices.iter().find_map(|n| match n {
        RunNotice::Failed { node, reason, .. } if *node == PLACED => Some(reason.clone()),
        _ => None,
    })
}

#[test]
fn a_writer_that_finished_before_its_editor_failed_is_held() {
    let mut f = fixture();
    *f.canned.fails.lock().unwrap() = Some("draft".into());
    start(&mut f);
    let notices = pump_until(&mut f, "the run never finished", |f| !f.host.is_busy());
    assert!(failed(&notices).is_some_and(|reason| reason.contains("held")), "{notices:?}");
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("the writer's result alone: {held:?}") };
    assert!(view.node == PLACED && view.outputs == 1 && !view.retry, "{view:?}");
    assert!(view.reason.starts_with("its technique failed: "), "{}", view.reason);
    f.session.dispatch("run.held_place", &json!({"id": view.id})).unwrap();
    pump_until(&mut f, "placing never settled", |f| f.host.held_results().is_empty());
    assert_eq!(placed(&f), vec!["draft: Describe the moon".to_owned()]);
    assert_eq!(f.canned.requests(), 2, "nothing is sent again");
}

#[test]
fn a_branch_that_finishes_after_another_failed_is_held() {
    let mut f = fixture_with(true);
    *f.canned.stuck.lock().unwrap() = Some("canned-aside".into());
    *f.canned.fails.lock().unwrap() = Some("canned-text".into());
    start(&mut f);
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while failed(&notices).is_none() {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline && f.host.is_busy(), "the writer never failed: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(f.host.held_results().is_empty(), "nothing is paid for yet");
    *f.canned.stuck.lock().unwrap() = None;
    notices.extend(pump_until(&mut f, "the run never finished", |f| !f.host.is_busy()));
    assert!(failed(&notices).is_some(), "{notices:?}");
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("the aside's result: {held:?}") };
    assert_eq!((view.node, view.outputs, view.reason.as_str()), (PLACED, 1, "its technique failed"));
    f.session.dispatch("run.held_discard", &json!({"id": view.id})).unwrap();
    pump_until(&mut f, "discarding never settled", |f| f.host.held_results().is_empty());
    assert_eq!(f.canned.requests(), 2);
}
