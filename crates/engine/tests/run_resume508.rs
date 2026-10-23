//! Runs survive a restart (#508): the host journals the usage log and every accepted job's
//! handle, and after a "kill" (every task of the first host aborted mid-poll, its session gone) a
//! new host restores the journal, follows the job from its handle without sending it again,
//! charges it once and records its result. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use futures::future::{AbortHandle, abortable};
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::{ProviderRouting, RegisteredProvider},
    run_cmds::RunCatalog,
    run_host::{RunHost, RunNotice},
    run_journal::{JOURNAL_VERSION, Journal, JournalStore, MemoryJournal},
};
use nodecraft_exec::{
    cost::{Charge, UsageEntry, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, Currency, EdgeId, Money, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([50; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const PROJECT: ProjectId = ProjectId::from_bytes([51; 16]);
const MODEL: &str = "fixture.writer";
/// Polls the mock takes to finish: long enough that the first host is killed well before.
const POLLS: u32 = 1_000;

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

/// Each task on its own thread, like the app's; `kill` aborts every task, as a process exit would.
/// Every sleep lasts at least `min_ms`, so a launch can be made too slow to finish before its kill;
/// with 0 a zero-length sleep is immediate.
#[derive(Default)]
struct Killable {
    tasks: Mutex<Vec<AbortHandle>>,
    min_ms: u64,
}
impl Killable {
    fn kill(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}
impl Runtime for Killable {
    fn spawn(&self, task: TaskFuture<()>) {
        let (task, handle) = abortable(task);
        self.tasks.lock().unwrap().push(handle);
        std::thread::spawn(move || {
            let _ = futures::executor::block_on(task);
        });
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let ms = ms.max(self.min_ms);
        if ms == 0 {
            return Box::pin(async {});
        }
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
        0
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    runtime: Arc<Killable>,
    binding: RegisteredProvider,
    budget: JobBudget,
}

fn project(kinds: &nodecraft_nodes::Registry) -> Project {
    let mut project = Project::new(PROJECT, "Resume").unwrap();
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
    project
}

/// A launch: a new session on `project` (or a new one), a new host restoring `journal`.
fn launch(mock: &MockProvider, journal: &MemoryJournal, project_from: Option<Project>, min_ms: u64) -> (Fixture, Result<(), nodecraft_types::Error>) {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let project = project_from.unwrap_or_else(|| project(&kinds));
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Default::default())),
        session: "resume".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let runtime = Arc::new(Killable { min_ms, ..Killable::default() });
    let mut host = RunHost::new(runtime.clone(), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let opened = host.open_journal(Box::new(journal.clone()));
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    (Fixture { session, host, runtime, binding, budget }, opened)
}

fn slow_mock() -> MockProvider {
    let mut slow = MockModel::new(MockOutput::Text);
    slow.polls = POLLS;
    slow.billable_units = Some(nodecraft_types::BillableUnits::parse("3").unwrap());
    MockProvider::new("mock", [("mock-text".to_owned(), slow)])
}

fn start(f: &mut Fixture) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[SOURCE, WRITER]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":amounts});
    f.session.dispatch("run.start", &start).unwrap();
}

fn pump_until(f: &mut Fixture, what: &str, done: impl Fn(&Fixture) -> bool) -> Vec<RunNotice> {
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(f) {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline, "{what}: {notices:?} {:?}", f.host.jobs());
        std::thread::sleep(Duration::from_millis(1));
    }
    notices
}

fn history(s: &Session) -> Vec<nodecraft_graph::Generation> {
    s.document().canvases().get(&C).unwrap().nodes().get(&WRITER).unwrap().history.clone()
}

#[test]
fn a_job_cut_off_by_a_kill_is_followed_after_restart_charged_once_and_recorded() {
    let mock = slow_mock();
    let journal = MemoryJournal::default();
    let (mut first, opened) = launch(&mock, &journal, None, 50);
    opened.unwrap();
    start(&mut first);
    pump_until(&mut first, "the job's handle was never saved", |_| journal.saved().is_some_and(|saved| !saved.submitted.is_empty()));
    // Kill: every task stops mid-poll, and the session goes with the process. The source was
    // recorded (and saved with the project); the model job's result never arrived.
    first.runtime.kill();
    let project = first.session.document().clone();
    drop(first);
    assert!(mock.polls() < u64::from(POLLS), "the job finished before the kill; raise POLLS");
    assert_eq!(mock.requests().len(), 1);
    let saved = journal.saved().unwrap();
    assert_eq!(saved.submitted.len(), 1, "{saved:?}");
    assert_eq!(saved.submitted[0].node, WRITER);
    assert!(saved.submitted[0].recipe.is_some(), "a single-job step can be recorded after a restart");
    let open: Vec<Charge> = saved.usage.entries().map(|e| e.status).collect();
    assert_eq!(open, [Charge::Estimate], "the reservation is saved open");

    let (mut second, opened) = launch(&mock, &journal, Some(project), 0);
    opened.unwrap();
    assert!(history(&second.session).is_empty());
    assert!(second.host.has_waiting());
    assert_eq!(second.host.waiting_adapters(PROJECT).into_iter().collect::<Vec<_>>(), ["mock"]);
    assert_eq!(second.host.waiting_adapters(ProjectId::from_bytes([9; 16])).len(), 0);
    let binding = second.binding.clone();
    second.host.resume(&second.session, &binding);
    assert!(!second.host.has_waiting());
    assert!(second.host.is_busy());
    assert_eq!(second.host.running().into_iter().collect::<Vec<_>>(), [(C, WRITER)]);
    let notices = pump_until(&mut second, "the resumed job never ended", |f| !f.host.is_busy());
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    assert_eq!(mock.requests().len(), 1, "followed, never sent again");
    assert_eq!(mock.billed(), 1);
    let history = history(&second.session);
    assert_eq!(history.len(), 1);
    assert!(matches!(&history[0].outputs[0], Value::Text(t) if t.starts_with("mock-text: Describe the sea #")), "{history:?}");
    let billed: Vec<_> = history[0].settings.iter().map(|s| s.billable_units.map(|u| u.to_string())).collect();
    assert_eq!(billed, [Some("3".to_owned())], "a result recorded after a restart keeps its billed units (#437)");
    let log = second.budget.log.lock().unwrap().clone();
    let rows: Vec<(u64, Charge)> = log.entries().map(|e| (e.cost.micros, e.status)).collect();
    assert_eq!(rows, [(10_000, Charge::Actual)], "charged once, at the estimate the mock didn't replace");
    // The journal now holds the settled log and no jobs.
    let saved = journal.saved().unwrap();
    assert!(saved.submitted.is_empty(), "{saved:?}");
    assert_eq!(saved.usage, log);
}

#[test]
fn an_estimate_left_open_without_a_handle_is_charged_as_unresolved() {
    // A submit that may or may not have reached the provider before the kill: its estimate was
    // saved but no handle came back, so the job can't be followed.
    let mut log = UsageLog::default();
    let entry = |key: &str| UsageEntry {
        at_ms: 0,
        session: "before".into(),
        node: WRITER,
        provider: "mock".into(),
        instance: None,
        model: MODEL.into(),
        cost: Money::new(Currency::USD, 10_000),
        idempotency_key: key.into(),
        status: Charge::Estimate,
    };
    log.record(entry("lost")).unwrap();
    let journal = MemoryJournal::default();
    journal.save(&Journal { version: JOURNAL_VERSION, usage: log, submitted: Vec::new() }).unwrap();
    let (mut f, opened) = launch(&MockProvider::standard(), &journal, None, 0);
    opened.unwrap();
    let rows: Vec<(u64, Charge)> = f.budget.log.lock().unwrap().entries().map(|e| (e.cost.micros, e.status)).collect();
    assert_eq!(rows, [(10_000, Charge::Unresolved)]);
    assert!(!f.host.has_waiting());
    f.host.pump(&mut f.session);
    let saved = journal.saved().unwrap();
    assert_eq!(saved.usage.entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved], "the settlement is saved");
}

#[test]
fn a_damaged_journal_is_reported_and_replaced_by_a_new_one() {
    #[derive(Clone, Default)]
    struct Damaged(MemoryJournal, Arc<Mutex<bool>>);
    impl JournalStore for Damaged {
        fn load(&self) -> Result<Option<Journal>, nodecraft_types::Error> {
            if *self.1.lock().unwrap() { self.0.load() } else { Journal::from_bytes(b"{").map(Some) }
        }
        fn save(&self, journal: &Journal) -> Result<(), nodecraft_types::Error> {
            self.0.save(journal)
        }
        fn set_aside(&self) -> Result<(), nodecraft_types::Error> {
            *self.1.lock().unwrap() = true;
            Ok(())
        }
    }
    let store = Damaged::default();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Default::default())),
        session: "resume".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let mut host = RunHost::new(Arc::new(Killable::default()), budget);
    assert!(host.open_journal(Box::new(store.clone())).is_err());
    assert!(*store.1.lock().unwrap(), "moved aside");
    let kinds = snapshot.registry().unwrap();
    let mut session = Session::with_registries(project(&kinds), Registry::builtins().unwrap(), kinds).unwrap();
    host.pump(&mut session);
    assert!(store.0.saved().is_some(), "a new journal is started");
}

#[test]
fn quitting_with_a_journal_suspends_the_job_instead_of_cancelling_it() {
    let mock = slow_mock();
    let journal = MemoryJournal::default();
    let (mut f, _) = launch(&mock, &journal, None, 50);
    start(&mut f);
    pump_until(&mut f, "the job was never submitted", |_| !mock.requests().is_empty());
    f.host.shut_down();
    f.runtime.kill();
    assert!(mock.cancelled().is_empty(), "left running at the provider");
    let saved = journal.saved().unwrap();
    assert_eq!(saved.submitted.len(), 1, "saved for the next launch even before a pump: {saved:?}");
}

/// A store that notes whether a save held an open estimate before the provider saw any request.
#[derive(Clone)]
struct Spy {
    inner: MemoryJournal,
    mock: MockProvider,
    ahead: Arc<Mutex<bool>>,
    fail: bool,
}
impl JournalStore for Spy {
    fn load(&self) -> Result<Option<Journal>, nodecraft_types::Error> {
        self.inner.load()
    }
    fn save(&self, journal: &Journal) -> Result<(), nodecraft_types::Error> {
        if journal.usage.entries().any(|e| e.status == Charge::Estimate) && self.mock.requests().is_empty() {
            *self.ahead.lock().unwrap() = true;
        }
        if self.fail {
            return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "the disk is full"));
        }
        self.inner.save(journal)
    }
}

fn spied(mock: &MockProvider, fail: bool) -> (Fixture, Spy) {
    let spy = Spy { inner: MemoryJournal::default(), mock: mock.clone(), ahead: Arc::default(), fail };
    let (mut f, _) = launch(mock, &MemoryJournal::default(), None, 0);
    f.host = RunHost::new(f.runtime.clone(), f.budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    f.host.open_journal(Box::new(spy.clone())).unwrap();
    let mut services = Services::default();
    f.host.register(&mut services).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let kinds = snapshot.registry().unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    f.session = Session::with_registries(project(&kinds), Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    f.session.set_provider_routing(routing).unwrap();
    f.host.install(&mut f.session);
    (f, spy)
}

#[test]
fn a_paid_jobs_estimate_is_saved_before_it_is_sent() {
    // Otherwise a crash right after the submit leaves neither a handle nor an estimate, and a
    // billed job is never counted.
    let mock = MockProvider::standard();
    let (mut f, spy) = spied(&mock, false);
    start(&mut f);
    pump_until(&mut f, "the job never ended", |f| !f.host.is_busy());
    assert_eq!(mock.requests().len(), 1);
    assert!(*spy.ahead.lock().unwrap(), "the estimate was saved before the request");
}

#[test]
fn a_paid_job_whose_charge_cant_be_saved_is_not_sent() {
    let mock = MockProvider::standard();
    let (mut f, _) = spied(&mock, true);
    start(&mut f);
    let notices = pump_until(&mut f, "the job never ended", |f| !f.host.is_busy());
    assert!(mock.requests().is_empty(), "nothing reached the provider");
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { node: WRITER, reason, .. } if reason.contains("wasn't sent"))), "{notices:?}");
    let open = f.budget.log.lock().unwrap().entries().any(|e| e.status == Charge::Estimate);
    assert!(!open, "the estimate of a job never sent is released");
}

#[test]
fn nothing_is_sent_once_the_host_is_shutting_down() {
    let mock = MockProvider::standard();
    let journal = MemoryJournal::default();
    let (mut f, _) = launch(&mock, &journal, None, 0);
    start(&mut f);
    // Quitting saved the journal; a job sent after that save would be lost with the process.
    f.host.shut_down();
    pump_until(&mut f, "the job never ended", |f| !f.host.is_busy());
    assert!(mock.requests().is_empty());
}

#[test]
fn a_job_whose_project_never_comes_back_is_eventually_settled() {
    let mock = slow_mock();
    let journal = MemoryJournal::default();
    let (mut first, _) = launch(&mock, &journal, None, 50);
    start(&mut first);
    pump_until(&mut first, "the job's handle was never saved", |_| journal.saved().is_some_and(|saved| !saved.submitted.is_empty()));
    first.runtime.kill();
    drop(first);
    let mut saved = journal.saved().unwrap();
    saved.submitted[0].launches = nodecraft_engine::run_journal::MAX_PARKED_LAUNCHES;
    journal.save(&saved).unwrap();
    let (mut second, opened) = launch(&mock, &journal, None, 0);
    opened.unwrap();
    assert!(!second.host.has_waiting());
    let rows: Vec<Charge> = second.budget.log.lock().unwrap().entries().map(|e| e.status).collect();
    assert_eq!(rows, [Charge::Unresolved], "it may have been billed");
    let notices = second.host.pump(&mut second.session);
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { node: WRITER, .. })), "{notices:?}");
    assert!(journal.saved().unwrap().submitted.is_empty());
}

/// #369: a result a restarted job delivers to a card that is gone is held, not dropped, and is
/// recorded once the card is back.
#[test]
fn a_resumed_result_whose_card_is_gone_is_held_until_it_can_be_recorded() {
    let mock = slow_mock();
    let journal = MemoryJournal::default();
    let (mut first, _) = launch(&mock, &journal, None, 50);
    start(&mut first);
    pump_until(&mut first, "the job's handle was never saved", |_| journal.saved().is_some_and(|saved| !saved.submitted.is_empty()));
    first.runtime.kill();
    let project = first.session.document().clone();
    drop(first);
    let (mut second, opened) = launch(&mock, &journal, Some(project), 0);
    opened.unwrap();
    second.session.dispatch("node.remove", &json!({"canvas": C, "id": WRITER})).unwrap();
    let binding = second.binding.clone();
    second.host.resume(&second.session, &binding);
    let notices = pump_until(&mut second, "the resumed job never ended", |f| !f.host.is_busy());
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { node: WRITER, reason, .. } if reason.contains("the paid result is held"))), "{notices:?}");
    let held = second.host.held_results();
    assert!(matches!(held.as_slice(), [view] if view.node == WRITER && view.retry), "{held:?}");
    let id = held[0].id;

    second.session.dispatch("edit.undo", &json!({})).unwrap();
    second.session.dispatch("run.held_retry", &json!({"id": id})).unwrap();
    let notices = pump_until(&mut second, "the retry never settled", |f| f.host.held_results().is_empty());
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    assert_eq!(history(&second.session).len(), 1);
    assert_eq!(mock.requests().len(), 1, "never sent again");
    assert_eq!(second.budget.log.lock().unwrap().entries().count(), 1, "charged once");
}

/// Three variations of the writer cut off by a kill and followed after a restart (#808): they
/// land in any order, yet end up as an uninterrupted run leaves them.
/// Returns the session and the variations' ids in variant order, read from the saved recipes.
fn variations_after_restart(cards: bool) -> (Session, Vec<nodecraft_types::GenerationId>) {
    let mock = slow_mock();
    let journal = MemoryJournal::default();
    let (mut first, opened) = launch(&mock, &journal, None, 50);
    opened.unwrap();
    first.session.dispatch("node.variations_set", &json!({"canvas": C, "id": WRITER, "count": 3})).unwrap();
    first.session.dispatch("canvas.set_variation_cards", &json!({"canvas": C, "cards": cards})).unwrap();
    start(&mut first);
    pump_until(&mut first, "the variations' handles were never saved", |_| journal.saved().is_some_and(|saved| saved.submitted.len() == 3));
    first.runtime.kill();
    let project = first.session.document().clone();
    drop(first);
    let mut order: Vec<_> = journal.saved().unwrap().submitted.iter().map(|job| job.recipe.clone().unwrap()).collect();
    // The first variant has the most after it.
    order.sort_by_key(|recipe| std::cmp::Reverse(recipe.later.len()));
    assert_eq!(order.iter().map(|r| r.later.len()).collect::<Vec<_>>(), [2, 1, 0]);
    let order: Vec<_> = order.into_iter().map(|recipe| recipe.generation).collect();
    let (mut second, opened) = launch(&mock, &journal, Some(project), 0);
    opened.unwrap();
    let binding = second.binding.clone();
    second.host.resume(&second.session, &binding);
    pump_until(&mut second, "the resumed variations never ended", |f| !f.host.is_busy());
    assert_eq!(mock.requests().len(), 3, "followed, never sent again");
    (second.session, order)
}

#[test]
fn variations_followed_after_a_restart_keep_their_order_in_the_history() {
    let (session, order) = variations_after_restart(false);
    let writer = session.document().canvases().get(&C).unwrap().nodes().get(&WRITER).unwrap();
    assert_eq!(writer.history.iter().map(|g| g.id).collect::<Vec<_>>(), order, "in variant order, however they landed");
    assert_eq!(writer.active, order.last().copied(), "the last variation is active, as uninterrupted");
}

#[test]
fn variations_followed_after_a_restart_go_on_cards_but_the_last() {
    let (session, order) = variations_after_restart(true);
    let canvas = session.document().canvases().get(&C).unwrap();
    let writer = canvas.nodes().get(&WRITER).unwrap();
    assert_eq!(writer.history.iter().map(|g| g.id).collect::<Vec<_>>(), order.get(2..).unwrap(), "the last variation stays on the writer");
    let cards: Vec<_> = canvas.nodes().values().filter(|n| n.id != WRITER && n.id != SOURCE).collect();
    assert_eq!(cards.len(), 2, "the two before it get cards");
    for card in cards {
        assert_eq!(card.history.len(), 1);
        assert!(canvas.edges().values().any(|e| e.from.node == SOURCE && e.to.node == card.id), "fed as the writer is");
    }
}

/// #174: the restored job of a provider that is gone (a plugin whose grant was withdrawn) is
/// given up: reported, and its charge settled as unresolved since it may have been billed.
#[test]
fn a_job_whose_provider_is_gone_is_given_up_and_charged_as_unresolved() {
    let mock = slow_mock();
    let journal = MemoryJournal::default();
    let (mut first, _) = launch(&mock, &journal, None, 50);
    start(&mut first);
    pump_until(&mut first, "the job's handle was never saved", |_| journal.saved().is_some_and(|saved| !saved.submitted.is_empty()));
    first.runtime.kill();
    let project = first.session.document().clone();
    drop(first);
    let (mut second, opened) = launch(&mock, &journal, Some(project), 0);
    opened.unwrap();
    second.host.abandon(PROJECT, "elsewhere", "its plugin is no longer enabled or granted");
    second.host.abandon(ProjectId::from_bytes([9; 16]), "mock", "its plugin is no longer enabled or granted");
    assert!(second.host.has_waiting(), "another adapter's or project's jobs are kept");
    second.host.abandon(PROJECT, "mock", "its plugin is no longer enabled or granted");
    assert!(!second.host.has_waiting());
    assert!(!second.host.is_busy(), "nothing is followed");
    let rows: Vec<Charge> = second.budget.log.lock().unwrap().entries().map(|e| e.status).collect();
    assert_eq!(rows, [Charge::Unresolved]);
    let notices = second.host.pump(&mut second.session);
    let failed: Vec<_> =
        notices.iter().filter_map(|n| if let RunNotice::Failed { node, reason, .. } = n { Some((*node, reason.clone())) } else { None }).collect();
    assert_eq!(failed, [(WRITER, "it couldn't be followed after a restart: its plugin is no longer enabled or granted".to_owned())]);
    let saved = journal.saved().unwrap();
    assert!(saved.submitted.is_empty(), "{saved:?}");
    assert_eq!(saved.usage.entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved]);
    assert_eq!(mock.requests().len(), 1, "never sent again");
}
