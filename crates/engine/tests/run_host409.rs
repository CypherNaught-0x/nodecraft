//! The run host (#409) drives `run.start` end to end: authorize, queue, the Driver on a real
//! threaded runtime, the mock provider, and `run.record` step by step. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::{MAX_RUNS, RunHost, RunNotice},
    run_jobs::{JobState, JobView},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::ErrorKind;
use nodecraft_providers::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_types::{AssetRef, CanvasId, Currency, EdgeId, MediaMeta, Money, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([40; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
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
    budget: JobBudget,
}

/// A text card whose prompt comes from a wired text source (a local job, then a model job).
fn fixture(mock: MockProvider) -> Fixture {
    fixture_with(mock, Budgets::default())
}

fn fixture_with(mock: MockProvider, budgets: Budgets) -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([41; 16]), "Run host").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let writer = kinds.get(TEXT_KIND).unwrap().instantiate_instance(WRITER, Default::default(), &[("model".into(), Value::Text(MODEL.into()))].into()).unwrap();
    Change::AddNode { canvas: C, node: writer }.apply(&mut project).unwrap();
    let source = kinds
        .get("source.text")
        .unwrap()
        .instantiate_instance(SOURCE, Default::default(), &[("text".into(), Value::Text("Describe the sea".into()))].into())
        .unwrap();
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
        budgets: Arc::new(Mutex::new(budgets)),
        session: "run-host".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    Fixture { session, host, mock, budget }
}

fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[SOURCE, WRITER]})
}

/// Quotes and accepts the run; the host has it in its inbox afterwards.
fn start(f: &mut Fixture) {
    start_request(f, request());
}

fn start_request(f: &mut Fixture, request: serde_json::Value) {
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":amounts});
    f.session.dispatch("run.start", &start).unwrap();
}

/// Pumps like a frame loop until the host is idle.
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

fn history(s: &Session) -> Vec<nodecraft_graph::Generation> {
    s.document().canvases().get(&C).unwrap().nodes().get(&WRITER).unwrap().history.clone()
}

#[test]
fn a_started_run_is_driven_recorded_and_charged_once() {
    // Room for one 0.01 USD call, not two.
    let cap = Money::new(Currency::USD, 15_000);
    let mut f = fixture_with(MockProvider::standard(), Budgets { session: Some(cap), ..Default::default() });
    start(&mut f);
    assert!(f.host.is_busy());
    let notices = drive(&mut f);
    assert!(f.host.running().is_empty());
    assert!(matches!(notices.first(), Some(RunNotice::Started { canvas, .. }) if *canvas == C), "{notices:?}");
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    assert_eq!(notices.last(), Some(&RunNotice::Finished { canvas: C }));
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let history = history(&f.session);
    assert_eq!(history.len(), 1);
    assert!(matches!(&history[0].outputs[0], Value::Text(t) if t.starts_with("mock-text: Describe the sea #")), "{history:?}");
    assert_eq!(f.mock.requests().len(), 1, "one paid call");
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), 1, "the call is accounted once");
    // The quote now counts what the run spent: a second call would pass the session cap.
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false, "the budget resource was refreshed after the charge: {q}");
    // And each recorded step is an ordinary undoable change. Both jobs start together (the
    // source's text is known up front), so either may be recorded last.
    let source = |s: &Session| s.document().canvases().get(&C).unwrap().nodes().get(&SOURCE).unwrap().history.len();
    assert_eq!(source(&f.session), 1);
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(self::history(&f.session).len() + source(&f.session), 1, "one step per undo");
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(self::history(&f.session).is_empty());
    assert_eq!(source(&f.session), 0);
}

#[test]
fn a_provider_failure_lands_on_its_card_and_frees_the_host() {
    let mut failing = MockModel::new(MockOutput::Text);
    failing.fail = Some(MockFailure::Submit(ErrorKind::UnsupportedInput));
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), failing)]));
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { node, .. } if *node == WRITER)), "{notices:?}");
    assert_eq!(notices.last(), Some(&RunNotice::Finished { canvas: C }));
    assert!(history(&f.session).is_empty());
    assert_eq!(f.mock.requests().len(), 1, "a refused submit is not retried");
    // The released reservation doesn't block the next run.
    start(&mut f);
    assert!(f.host.is_busy());
}

#[test]
fn a_run_that_went_stale_before_the_host_took_it_is_refused_without_sending() {
    let mut f = fixture(MockProvider::standard());
    start(&mut f);
    // The document changes between acceptance and the next frame.
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":SOURCE,"name":"text","value":{"type":"text","value":"Describe a hill"}})).unwrap();
    let notices = drive(&mut f);
    assert!(matches!(notices.as_slice(), [RunNotice::Refused { reason }] if reason.contains("stale")), "{notices:?}");
    assert!(f.mock.requests().is_empty());
    assert!(history(&f.session).is_empty());
}

#[test]
fn the_inbox_refuses_starts_past_its_bound() {
    let mut f = fixture(MockProvider::standard());
    for _ in 0..MAX_RUNS {
        start(&mut f);
    }
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    let mut extra = request();
    extra["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    let refused = f.session.dispatch("run.start", &extra).unwrap_err();
    assert!(refused.message.contains("too many runs"), "{}", refused.message);
    let notices = drive(&mut f);
    let recorded = notices.iter().filter(|n| matches!(n, RunNotice::Recorded { node, .. } if *node == WRITER)).count();
    assert_eq!(recorded, MAX_RUNS, "{notices:?}");
    assert_eq!(f.mock.requests().len(), MAX_RUNS);
}

#[test]
fn cancelling_a_run_mid_poll_ends_it_and_settles_the_job() {
    let mut slow = MockModel::new(MockOutput::Text);
    slow.polls = u32::MAX;
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), slow)]));
    start(&mut f);
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.mock.requests().is_empty() {
        f.host.pump(&mut f.session);
        assert!(Instant::now() < deadline, "the job was never submitted");
        std::thread::sleep(Duration::from_millis(1));
    }
    // The source's end may still be on its way from the driver; wait until it is absorbed.
    pump_until(&mut f, "the finished source isn't running", |f| f.host.running().into_iter().collect::<Vec<_>>() == [(C, WRITER)]);
    f.host.cancel_all();
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Cancelled { canvas: C, node: WRITER }), "{notices:?}");
    assert_eq!(notices.last(), Some(&RunNotice::Finished { canvas: C }));
    assert!(history(&f.session).is_empty());
    // Settled, not left reserved: the next quote is unaffected.
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(width, height, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

fn add(f: &mut Fixture, id: NodeId, kind: &str, x: i32) {
    f.session.dispatch("node.create", &json!({"canvas": C, "id": id, "kind": kind, "pos": {"x": x, "y": 300}})).unwrap();
}

fn connect(f: &mut Fixture, id: u8, from: NodeId, to: NodeId, port: &str) {
    let edge = json!({"id": EdgeId::from_bytes([id; 16]), "from": {"node": from, "port": "out"}, "to": {"node": to, "port": port}, "order": 0});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
}

#[test]
fn chained_actions_read_what_the_previous_action_made() {
    let mut f = fixture(MockProvider::standard());
    let hash = f.session.import_asset(&png(16, 12)).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(16), height: Some(12), ..MediaMeta::default() } };
    let (picture, first, second) = (NodeId::from_bytes([10; 16]), NodeId::from_bytes([11; 16]), NodeId::from_bytes([12; 16]));
    add(&mut f, picture, "source.image", 0);
    f.session.dispatch("node.param_set", &json!({"canvas": C, "id": picture, "name": "asset", "value": Value::Media(asset)})).unwrap();
    add(&mut f, first, "image.blur", 400);
    add(&mut f, second, "image.blur", 800);
    connect(&mut f, 20, picture, first, "image");
    connect(&mut f, 21, first, second, "image");
    start_request(&mut f, json!({"canvas":C,"mode":"selected","nodes":[picture, first, second]}));
    let notices = drive(&mut f);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let node = |id: NodeId| f.session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap().clone();
    for id in [first, second] {
        let Some(Value::Media(out)) = node(id).history.first().and_then(|g| g.outputs.first()).cloned() else { panic!("{id:?} made no image: {notices:?}") };
        assert!(f.session.assets().contains(&out.hash), "the action's image is in the project");
    }
    assert!(f.mock.requests().is_empty(), "actions are local");
}

/// A waiting run is bound to the revision it was quoted at, so it can't wait behind a running
/// one: the first record would make it stale. Running runs count toward the bound instead.
#[test]
fn running_runs_count_toward_the_bound_so_nothing_accepted_goes_stale() {
    let mut slow = MockModel::new(MockOutput::Text);
    slow.polls = 3;
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), slow)]));
    for _ in 0..MAX_RUNS {
        start(&mut f);
    }
    let mut notices = f.host.pump(&mut f.session);
    assert!(f.host.is_busy());
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    let mut extra = request();
    extra["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    let refused = f.session.dispatch("run.start", &extra).unwrap_err();
    assert!(refused.message.contains("too many runs"), "{}", refused.message);
    notices.extend(drive(&mut f));
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Refused { .. } | RunNotice::Failed { .. })), "{notices:?}");
    // Finished runs free their slots.
    start(&mut f);
    notices.extend(drive(&mut f));
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Refused { .. })), "{notices:?}");
    let recorded = notices.iter().filter(|n| matches!(n, RunNotice::Recorded { node, .. } if *node == WRITER)).count();
    assert_eq!(recorded, MAX_RUNS + 1);
}

#[test]
fn cancelling_drops_runs_that_have_not_started() {
    let mut f = fixture(MockProvider::standard());
    start(&mut f);
    start(&mut f);
    f.host.cancel_all();
    let notices = drive(&mut f);
    assert_eq!(
        notices,
        [RunNotice::Refused { reason: "cancelled before it started".into() }, RunNotice::Refused { reason: "cancelled before it started".into() }]
    );
    assert!(f.mock.requests().is_empty());
}

// The queue panel's view of jobs (#82).

/// `n` more text cards wired to the source, all routed to the fixture's mock.
fn more_writers(f: &mut Fixture, n: u8) -> Vec<NodeId> {
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(f.mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let ids: Vec<NodeId> = (0..n).map(|i| NodeId::from_bytes([60 + i; 16])).collect();
    for (i, id) in ids.iter().enumerate() {
        add(f, *id, TEXT_KIND, 400 * i as i32);
        connect(f, 70 + i as u8, SOURCE, *id, "prompt");
        routing.select(*id, "mock", binding.target()).unwrap();
    }
    let preview = f.session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":ids,"model":MODEL})).unwrap();
    let commit = json!({"canvas":C,"nodes":ids,"model":MODEL,"expected_revision":f.session.revision(),"preview_binding":preview["binding"]});
    f.session.dispatch("node.set_model", &commit).unwrap();
    f.session.set_provider_routing(routing).unwrap();
    ids
}

fn job(f: &RunHost, node: NodeId) -> JobView {
    f.jobs().into_iter().rev().find(|view| view.node == node).unwrap()
}

/// Pumps until `done` holds.
fn pump_until(f: &mut Fixture, what: &str, done: impl Fn(&Fixture) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(f) {
        f.host.pump(&mut f.session);
        assert!(Instant::now() < deadline, "{what}: {:?}", f.host.jobs());
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn the_queue_shows_each_job_with_its_provider_price_and_end() {
    let mut f = fixture(MockProvider::standard());
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    let writer = job(&f.host, WRITER);
    assert_eq!((writer.provider.as_deref(), writer.model.as_deref()), (Some("mock"), Some("mock-text")));
    assert_eq!(writer.estimate, Some(Money::new(Currency::USD, 10_000)));
    assert_eq!(writer.state, JobState::Done);
    assert!(writer.sent, "the provider had it");
    let source = job(&f.host, SOURCE);
    assert_eq!((source.provider, source.state), (None, JobState::Done), "local work has no provider");
    assert_eq!(f.host.jobs().len(), 2, "one row per job, however many reports it had");
    f.host.clear_finished();
    assert!(f.host.jobs().is_empty());
}

#[test]
fn a_failed_job_names_its_cause() {
    let mut failing = MockModel::new(MockOutput::Text);
    failing.fail = Some(MockFailure::Submit(ErrorKind::Auth));
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), failing)]));
    start(&mut f);
    drive(&mut f);
    let writer = job(&f.host, WRITER);
    assert!(matches!(&writer.state, JobState::Failed { cause: Some(ErrorKind::Auth), .. }), "{writer:?}");
    assert_eq!(writer.status(), "Failed: API key");
    assert!(writer.sent, "a job that started may have reached the provider");
}

#[test]
fn cancelling_a_queued_job_drops_it_unsent_and_a_running_one_leaves_its_cost_unresolved() {
    let mut slow = MockModel::new(MockOutput::Text);
    slow.polls = u32::MAX;
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), slow)]));
    // Five cards on one provider: four run (the lane's limit), one waits.
    let mut cards = more_writers(&mut f, 4);
    cards.push(WRITER);
    start_request(&mut f, json!({"canvas":C,"mode":"selected","nodes":[SOURCE, WRITER, cards[0], cards[1], cards[2], cards[3]]}));
    pump_until(&mut f, "four jobs reached the provider", |f| f.mock.requests().len() == 4 && f.host.jobs().iter().filter(|v| v.sent).count() == 4);
    let waiting: Vec<JobView> = f.host.jobs().into_iter().filter(|v| v.state == JobState::Queued).collect();
    assert_eq!(waiting.len(), 1, "{:?}", f.host.jobs());
    let queued = waiting[0].clone();
    f.host.cancel(queued.key).unwrap();
    assert_eq!(job(&f.host, queued.node).status(), "Cancelling…");
    pump_until(&mut f, "the queued job ended", |f| job(&f.host, queued.node).state.is_final());
    let ended = job(&f.host, queued.node);
    assert_eq!((ended.state.clone(), ended.sent, ended.status()), (JobState::Cancelled, false, "Cancelled before it was sent".to_owned()));
    // A running one: the provider is asked to cancel, and the cost is not shown as refunded.
    let running = f.host.jobs().into_iter().find(|v| !v.state.is_final() && v.provider.is_some()).unwrap();
    f.host.cancel(running.key).unwrap();
    pump_until(&mut f, "the running job ended", |f| job(&f.host, running.node).state.is_final());
    let ended = job(&f.host, running.node);
    assert_eq!(ended.state, JobState::Cancelled);
    assert!(ended.sent && ended.status().contains("cost unresolved"), "{ended:?}");
    assert_eq!(f.mock.requests().len(), 4, "the cancelled queued job was never sent");
    assert!(f.host.cancel(running.key).is_err(), "an ended job can't be cancelled again");
    f.host.cancel_all();
    let notices = drive(&mut f);
    assert_eq!(notices.last(), Some(&RunNotice::Finished { canvas: C }));
    assert!(history(&f.session).is_empty());
}

#[test]
fn a_job_that_finishes_before_its_cancel_keeps_its_result() {
    let mut f = fixture(MockProvider::standard());
    start(&mut f);
    pump_until(&mut f, "the model job was queued", |f| f.host.jobs().iter().any(|v| v.node == WRITER));
    // From here nothing is absorbed until the cancel is in: the model finishes at submit, so its
    // result is already on its way when the cancel is asked for.
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.mock.requests().is_empty() {
        assert!(Instant::now() < deadline, "the job was never sent");
        std::thread::sleep(Duration::from_millis(1));
    }
    let live = job(&f.host, WRITER);
    assert!(!live.state.is_final(), "{live:?}");
    f.host.cancel(live.key).unwrap();
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    let writer = job(&f.host, WRITER);
    assert_eq!(writer.state, JobState::Done);
    assert!(writer.cancel_requested);
    assert_eq!(writer.status(), "Finished before it could be cancelled");
    assert_eq!(history(&f.session).len(), 1);
}

#[test]
fn a_cancel_the_provider_refuses_keeps_following_the_job_to_its_result() {
    let mut stubborn = MockModel::new(MockOutput::Text);
    // About a millisecond a poll: long enough to cancel mid-run, short enough to finish.
    stubborn.polls = 300;
    stubborn.fail = Some(MockFailure::Cancel(ErrorKind::Network));
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), stubborn)]));
    start(&mut f);
    pump_until(&mut f, "the job is running at the provider", |f| {
        f.host.jobs().iter().any(|v| v.node == WRITER && matches!(v.state, JobState::Running { fraction: Some(_), .. }))
    });
    f.host.cancel(job(&f.host, WRITER).key).unwrap();
    pump_until(&mut f, "the provider refused", |f| job(&f.host, WRITER).cancel_refused || job(&f.host, WRITER).state.is_final());
    let refused = job(&f.host, WRITER);
    if !refused.state.is_final() {
        assert_eq!(refused.status(), "Cancel refused; still running (may be billed)");
    }
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "the billed result is kept: {notices:?}");
    let writer = job(&f.host, WRITER);
    assert_eq!((writer.state.clone(), writer.cancel_refused), (JobState::Done, true));
    assert_eq!(writer.status(), "Finished; the provider refused to cancel");
    assert_eq!(f.mock.cancels_refused(), 1);
    assert_eq!(history(&f.session).len(), 1);
}

#[test]
fn a_cancelled_step_is_reported_as_cancelled_not_failed() {
    let mut slow = MockModel::new(MockOutput::Text);
    slow.polls = u32::MAX;
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), slow)]));
    start(&mut f);
    pump_until(&mut f, "the job is with the provider", |f| f.host.jobs().iter().any(|v| v.node == WRITER && v.sent));
    f.host.cancel(job(&f.host, WRITER).key).unwrap();
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Cancelled { canvas: C, node: WRITER }), "{notices:?}");
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
}
