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
    cost::{Budgets, Charge, UsageLog},
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
    fixture_retrying(mock, budgets, RetryPolicy { max_attempts: 1, ..Default::default() })
}

fn fixture_retrying(mock: MockProvider, budgets: Budgets, retry: RetryPolicy) -> Fixture {
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
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(retry);
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

/// #437: the units a provider says it billed are kept with the job's settings on the result, and
/// are never taken for money: the call is charged its quote.
#[test]
fn billed_units_are_recorded_with_the_jobs_settings() {
    let units = nodecraft_types::BillableUnits::parse("2.5").unwrap();
    let mut billing = MockModel::new(MockOutput::Text);
    billing.billable_units = Some(units);
    billing.execution_ms = Some(1250);
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), billing)]));
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    let history = history(&f.session);
    let settings = &history[0].settings;
    assert_eq!(settings.len(), 1, "{history:?}");
    assert_eq!((settings[0].provider.as_str(), settings[0].model.as_str(), settings[0].billable_units), ("mock", "mock-text", Some(units)));
    let charged: Vec<u64> = f.budget.log.lock().unwrap().entries().map(|e| e.cost.micros).collect();
    assert_eq!(charged, [10_000], "charged the quote, not the units");
    // It survives saving the project, and a result recorded without it reads back as before.
    let saved = serde_json::to_value(&history[0]).unwrap();
    assert_eq!(saved["settings"][0]["billable_units"], "2.5");
    assert_eq!(settings[0].execution_ms, Some(1250));
    assert_eq!(saved["settings"][0]["execution_ms"], 1250);
    assert_eq!(serde_json::from_value::<nodecraft_graph::Generation>(saved).unwrap(), history[0]);
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

/// #84: a prompt past the model's context window fails with what to do about it, is not
/// retried, is billed nothing, leaves the card's history alone, and once shortened runs and is
/// charged once.
#[test]
fn a_prompt_past_the_context_window_fails_with_advice_and_is_charged_once_after_shortening() {
    use nodecraft_engine::run_jobs::{CONTEXT_LIMIT_HINT, failure_hint};
    // The wired "Describe the sea" is sixteen bytes: four mock tokens, one past this window.
    let window = MockModel { fail: Some(MockFailure::ContextLimit(3)), ..MockModel::new(MockOutput::Text) };
    let mut f = fixture_retrying(MockProvider::new("mock", [("mock-text".to_owned(), window)]), Budgets::default(), RetryPolicy::default());
    start(&mut f);
    let notices = drive(&mut f);
    let reason = notices
        .iter()
        .find_map(|n| match n {
            RunNotice::Failed { node, reason, .. } if *node == WRITER => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the card's failure is reported: {notices:?}"));
    assert!(nodecraft_providers::is_context_limit_text(&reason), "{reason}");
    assert!(reason.contains(nodecraft_providers::CONTEXT_LIMIT_FIX) && reason.contains("context window is 3 tokens"), "says what to do: {reason}");
    let writer = job(&f.host, WRITER);
    assert_eq!(writer.status(), "Failed: too long for the model");
    let JobState::Failed { cause: Some(cause), reason: failed } = &writer.state else { panic!("{writer:?}") };
    assert_eq!((*cause, failure_hint(*cause, failed)), (ErrorKind::UnsupportedInput, CONTEXT_LIMIT_HINT));
    assert_eq!(f.mock.requests().len(), 1, "a context refusal is not retried, even with retries allowed");
    assert_eq!(f.mock.billed(), 0, "the provider billed nothing");
    // The mock gives no audited proof that nothing was accepted, so the runner holds the quote
    // once as unresolved, as for any refused submit; it is never held or charged again.
    let held: Vec<Charge> = f.budget.log.lock().unwrap().entries().map(|e| e.status).collect();
    assert_eq!(held, [Charge::Unresolved], "one hold, never a settled charge");
    assert!(history(&f.session).is_empty(), "no answer, partial or otherwise, is recorded");
    // Shortened, the next run fits and is the only charge.
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":SOURCE,"name":"text","value":{"type":"text","value":"Sea"}})).unwrap();
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    assert_eq!(history(&f.session).len(), 1);
    let charges: Vec<Charge> = f.budget.log.lock().unwrap().entries().map(|e| e.status).collect();
    assert_eq!((f.mock.requests().len(), f.mock.billed()), (2, 1));
    assert_eq!(charges, [Charge::Unresolved, Charge::Actual], "the answer is charged once, and the refusal's hold is not charged again");
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
    // The model finishes at submit, so its result is on its way when the cancel is asked for. A
    // slow machine can absorb that result while the run is still being set up, before there is
    // anything to cancel; then the scenario runs again, and the result is kept either way.
    for _ in 0..20 {
        let mut f = fixture(MockProvider::standard());
        start(&mut f);
        pump_until(&mut f, "the model job was queued", |f| f.host.jobs().iter().any(|v| v.node == WRITER));
        let deadline = Instant::now() + Duration::from_secs(20);
        while f.mock.requests().is_empty() {
            assert!(Instant::now() < deadline, "the job was never sent");
            std::thread::sleep(Duration::from_millis(1));
        }
        let live = job(&f.host, WRITER);
        let raced = !live.state.is_final();
        if raced {
            f.host.cancel(live.key).unwrap();
        }
        let notices = drive(&mut f);
        let writer = job(&f.host, WRITER);
        assert_eq!(writer.state, JobState::Done, "{writer:?}");
        assert_eq!(history(&f.session).len(), 1);
        if raced {
            assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
            assert!(writer.cancel_requested);
            assert_eq!(writer.status(), "Finished before it could be cancelled");
            return;
        }
    }
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

/// A text model that streams `pieces` and then waits at the provider until cancelled.
fn streaming(pieces: &[&str], polls: u32) -> MockProvider {
    let mut model = MockModel::new(MockOutput::Text);
    model.polls = polls;
    MockProvider::new("mock", [("mock-text".to_owned(), model)]).with_stream("mock-text", pieces.iter().copied())
}

fn partial(f: &Fixture) -> Option<String> {
    f.host.jobs().into_iter().rev().find(|view| view.node == WRITER).and_then(|view| view.partial.map(|text| text.as_str().to_owned()))
}

fn active(s: &Session) -> Option<nodecraft_types::GenerationId> {
    s.document().canvases().get(&C).unwrap().nodes().get(&WRITER).unwrap().active
}

#[test]
fn a_text_answer_streams_into_its_job_and_only_the_finished_answer_is_recorded() {
    let mut f = fixture(streaming(&["The sea ", "is wide"], 0));
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    let history = history(&f.session);
    assert_eq!(history.len(), 1, "one result, one undo step");
    assert!(matches!(&history[0].outputs[0], Value::Text(t) if t.starts_with("mock-text: Describe the sea #")), "{history:?}");
    assert_eq!(partial(&f), None, "a finished job shows its answer, not the stream");
}

#[test]
fn a_cancelled_answer_keeps_what_streamed_until_kept_and_never_replaces_the_shown_one() {
    // A finished answer first, so the card shows something.
    let mut f = fixture(streaming(&["The sea ", "is wide"], u32::MAX));
    let shown = nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([9; 16]), vec![Value::Text("Shown".into())]);
    f.session.dispatch("run.record", &json!({"canvas": C, "results": [{"node": WRITER, "generation": shown}]})).unwrap();
    start(&mut f);
    pump_until(&mut f, "the stream arrives", |f| partial(f).as_deref() == Some("The sea is wide"));
    let key = job(&f.host, WRITER).key;
    assert_eq!(history(&f.session).len(), 1, "a streaming answer isn't recorded");
    assert!(f.host.keep_partial(&mut f.session, key).is_err(), "a running job's answer can't be kept yet");
    f.host.cancel_all();
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Cancelled { canvas: C, node: WRITER }), "{notices:?}");
    assert_eq!(history(&f.session).len(), 1, "cancelling records nothing by itself");
    assert_eq!(partial(&f).as_deref(), Some("The sea is wide"), "the cancelled job keeps what streamed");

    f.host.keep_partial(&mut f.session, key).unwrap();
    let history = history(&f.session);
    assert_eq!(history.len(), 2);
    assert!(matches!(&history[1].outputs[0], Value::Text(t) if t == "The sea is wide"), "{history:?}");
    assert_eq!(active(&f.session), Some(nodecraft_types::GenerationId::from_bytes([9; 16])), "keeping doesn't change the shown answer");
    assert_eq!(partial(&f), None);
    assert!(f.host.keep_partial(&mut f.session, key).is_err(), "it is kept once");
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(self::history(&f.session).len(), 1, "keeping is one undo step");
}

#[test]
fn a_kept_answer_on_an_empty_card_becomes_the_shown_one() {
    let mut f = fixture(streaming(&["Half"], u32::MAX));
    start(&mut f);
    pump_until(&mut f, "the stream arrives", |f| partial(f).is_some());
    let key = job(&f.host, WRITER).key;
    f.host.cancel_all();
    drive(&mut f);
    f.host.keep_partial(&mut f.session, key).unwrap();
    assert_eq!(active(&f.session), history(&f.session).first().map(|g| g.id));
}

#[test]
fn a_failed_answer_keeps_what_streamed_and_can_be_kept() {
    let mut model = MockModel::new(MockOutput::Text);
    model.fail = Some(MockFailure::Submit(ErrorKind::UnsupportedInput));
    let mock = MockProvider::new("mock", [("mock-text".to_owned(), model)]).with_stream("mock-text", ["Half ", "way"]);
    let mut f = fixture(mock);
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { node: WRITER, .. })), "{notices:?}");
    assert_eq!(partial(&f).as_deref(), Some("Half way"), "a failed job keeps what streamed");
    let key = job(&f.host, WRITER).key;
    f.host.keep_partial(&mut f.session, key).unwrap();
    assert!(matches!(&history(&f.session)[0].outputs[0], Value::Text(t) if t == "Half way"));
}

#[test]
fn a_discarded_answer_is_gone_and_records_nothing() {
    let mut f = fixture(streaming(&["Half"], u32::MAX));
    start(&mut f);
    pump_until(&mut f, "the stream arrives", |f| partial(f).is_some());
    let key = job(&f.host, WRITER).key;
    f.host.cancel_all();
    drive(&mut f);
    f.host.discard_partial(key);
    assert_eq!(partial(&f), None);
    assert!(f.host.keep_partial(&mut f.session, key).is_err(), "nothing is left to keep");
    assert!(history(&f.session).is_empty());
}

/// A batch of prompts into a Generate card runs it once per prompt (#515), each prompt in the
/// batch wire's place among the card's prompt wires.
#[test]
fn a_batch_into_a_generate_card_runs_once_per_prompt() {
    let mut f = fixture(MockProvider::standard());
    let batch = NodeId::from_bytes([13; 16]);
    add(&mut f, batch, nodecraft_nodes::batch::ID, 0);
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":batch,"name":"text","value":{"type":"text","value":"a fox\nan owl"}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([22; 16]), "from": {"node": batch, "port": "item"}, "to": {"node": WRITER, "port": "prompt"}, "order": 1});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    let request = json!({"canvas":C,"mode":"selected","nodes":[SOURCE, WRITER]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["totals"]["USD"]["micros"], 20_000, "two calls: {q}");
    start_request(&mut f, request);
    let notices = drive(&mut f);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let done = history(&f.session).last().cloned().unwrap();
    assert_eq!(done.items.len(), 2);
    let Value::List(texts) = &done.outputs[0] else { panic!("{done:?}") };
    let [Value::Text(fox), Value::Text(owl)] = texts.as_slice() else { panic!("{texts:?}") };
    // The source's prompt comes first, then the item.
    assert!(fox.contains("Describe the sea\na fox") && owl.contains("Describe the sea\nan owl"), "{fox} / {owl}");
    assert_eq!(f.mock.requests().len(), 2);
}

/// One prompt of a batch that ends early leaves a partial result with the other prompt's answer;
/// running the card again quotes and sends only the missing prompt (#480, #515).
#[test]
fn a_batch_item_that_ends_early_is_the_only_one_run_again() {
    let mut slow = MockModel::new(MockOutput::Text);
    slow.polls = 20;
    let mut f = fixture(MockProvider::new("mock", [("mock-text".to_owned(), slow)]));
    let batch = NodeId::from_bytes([13; 16]);
    add(&mut f, batch, nodecraft_nodes::batch::ID, 0);
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":batch,"name":"text","value":{"type":"text","value":"a fox\nan owl"}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([22; 16]), "from": {"node": batch, "port": "item"}, "to": {"node": WRITER, "port": "prompt"}, "order": 1});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    let request = json!({"canvas":C,"mode":"selected","nodes":[SOURCE, WRITER]});
    start_request(&mut f, request.clone());
    pump_until(&mut f, "both prompts reached the provider", |f| f.mock.requests().len() == 2);
    let first = f.host.jobs().into_iter().find(|v| v.node == WRITER && !v.state.is_final()).unwrap();
    f.host.cancel(first.key).unwrap();
    drive(&mut f);
    let partial = history(&f.session).last().cloned().unwrap();
    assert!(partial.is_partial(), "{partial:?}");
    assert_eq!(partial.incomplete.as_ref().map(|i| i.missing.len()), Some(1));
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["totals"]["USD"]["micros"], 10_000, "only the missing prompt is quoted: {q}");
    start_request(&mut f, request);
    let notices = drive(&mut f);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let whole = history(&f.session).last().cloned().unwrap();
    assert!(!whole.is_partial(), "{whole:?}");
    let Value::List(texts) = &whole.outputs[0] else { panic!("{whole:?}") };
    assert_eq!(texts.len(), 2);
    assert_eq!(f.mock.requests().len(), 3, "the finished prompt is never sent again");
}
