//! Running a Batch Generate sheet (#163) through the one execution engine: rows are quoted
//! together, budgets and the provider's lane limit hold, every job keeps one idempotency key
//! across retries and a restart, a regenerate is a new run, "retry failed" runs only the rows
//! that failed, cancelling stops queued rows unsent, and a run is the snapshot it was started
//! from. The mock provider only; no live calls.
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
    run_jobs::JobState,
    run_journal::MemoryJournal,
    sheet_cmds,
};
use nodecraft_exec::{
    cost::{Budgets, Charge, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation;
use nodecraft_providers::{
    ErrorKind,
    mock::{MockFailure, MockModel, MockOutput, MockProvider},
};
use nodecraft_types::{CanvasId, Currency, Money, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SHEET: CanvasId = CanvasId::from_bytes([163; 16]);
const PLAIN: CanvasId = CanvasId::from_bytes([164; 16]);
const PROJECT: ProjectId = ProjectId::from_bytes([165; 16]);
const PAINTER: &str = "fixture.painter";
const FLAKY: &str = "fixture.flaky";
/// What one row costs: the catalog's per-call price.
const ROW_MICROS: u64 = 40_000;

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
pricing = {{ type = "per_call", price = "0.04 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
        )
    };
    Catalog::from_toml(&format!("{}{}", model(PAINTER, "mock-image"), model(FLAKY, "mock-flaky"))).unwrap()
}

/// Each task on its own thread, like the app's; `kill` aborts every task, as quitting would.
/// Every sleep lasts at least `min_ms`, so a provider's polls can be made to take a while.
struct Killable {
    tasks: Mutex<Vec<AbortHandle>>,
    min_ms: u64,
    start: Instant,
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
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    runtime: Arc<Killable>,
    binding: RegisteredProvider,
    budget: JobBudget,
    rows: Vec<NodeId>,
}

/// How a launch is set up.
struct Launch<'a> {
    budgets: Budgets,
    retry: RetryPolicy,
    min_ms: u64,
    journal: Option<&'a MemoryJournal>,
    /// The project a restart reopens; a new sheet of these rows' (prompt, model) otherwise.
    project: Option<Project>,
}

impl Default for Launch<'_> {
    fn default() -> Self {
        Self { budgets: Budgets::default(), retry: RetryPolicy::NEVER, min_ms: 0, journal: None, project: None }
    }
}

fn row_id(n: usize) -> NodeId {
    NodeId::from_bytes([u8::try_from(n + 1).unwrap(); 16])
}

/// A project with a plain canvas and sheet `SHEET`, one row per (prompt, model), top to bottom.
fn project(kinds: &nodecraft_nodes::Registry, rows: &[(&str, &str)]) -> Project {
    let mut project = Project::new(PROJECT, "Sheet runs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(PLAIN, "Canvas") }.apply(&mut project).unwrap();
    Change::AddCanvas { canvas: Canvas::sheet(SHEET, "Mugs") }.apply(&mut project).unwrap();
    for (n, (prompt, model)) in rows.iter().enumerate() {
        let params = [(generation::MODEL.into(), Value::Text((*model).into())), (generation::PROMPT.into(), Value::Text((*prompt).into()))].into();
        let mut node = kinds.get(generation::KIND).unwrap().instantiate_instance(row_id(n), Default::default(), &params).unwrap();
        node.pos = nodecraft_graph::Position { x: 0.0, y: n as f32 * sheet_cmds::ROW_SPACING };
        Change::AddNode { canvas: SHEET, node }.apply(&mut project).unwrap();
    }
    project
}

fn launch(mock: &MockProvider, rows: &[(&str, &str)], how: Launch<'_>) -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let project = how.project.unwrap_or_else(|| project(&kinds, rows));
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(how.budgets)),
        session: "sheet-runs".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let runtime = Arc::new(Killable { tasks: Mutex::default(), min_ms: how.min_ms, start: Instant::now() });
    let mut host = RunHost::new(runtime.clone(), budget.clone()).with_retry(how.retry);
    if let Some(journal) = how.journal {
        host.open_journal(Box::new(journal.clone())).unwrap();
    }
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    let rows: Vec<NodeId> = sheet_cmds::rows(session.document().canvases().get(&SHEET).unwrap()).iter().map(|row| row.id).collect();
    let binding = route(&mut session, mock, &rows);
    host.install(&mut session);
    Fixture { session, host, runtime, binding, budget, rows }
}

/// Routes `rows` to `mock`, as the app does once a provider is set up.
fn route(session: &mut Session, mock: &MockProvider, rows: &[NodeId]) -> RegisteredProvider {
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    for row in rows {
        routing.select(*row, "mock", binding.target()).unwrap();
    }
    session.set_provider_routing(routing).unwrap();
    binding
}

fn request(rows: &[NodeId]) -> Json {
    json!({"canvas": SHEET, "mode": "selected", "nodes": rows})
}

fn quote(f: &mut Fixture, rows: &[NodeId]) -> Json {
    f.session.dispatch("run.quote", &request(rows)).unwrap()
}

/// `run.start` accepting exactly what `quoted` priced.
fn accept(f: &mut Fixture, rows: &[NodeId], quoted: &Json) -> Result<Json, nodecraft_types::Error> {
    let amounts: serde_json::Map<_, _> =
        quoted["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request(rows);
    start["accept"] = json!({"token": quoted["token"], "partial": false, "unpriced_jobs": 0, "amounts": amounts});
    f.session.dispatch("run.start", &start)
}

fn start(f: &mut Fixture, rows: &[NodeId]) {
    let quoted = quote(f, rows);
    assert_eq!(quoted["allowed"], true, "{quoted}");
    accept(f, rows, &quoted).unwrap();
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

fn drive(f: &mut Fixture) -> Vec<RunNotice> {
    let mut notices = pump_until(f, "the run never finished", |f| !f.host.is_busy());
    notices.extend(f.host.pump(&mut f.session));
    notices
}

fn history(f: &Fixture, row: NodeId) -> Vec<nodecraft_types::GenerationId> {
    f.session.document().canvases().get(&SHEET).unwrap().nodes().get(&row).unwrap().history.iter().map(|g| g.id).collect()
}

fn keys(mock: &MockProvider) -> Vec<String> {
    mock.requests().into_iter().map(|job| job.idempotency_key).collect()
}

fn charges(f: &Fixture) -> Vec<(u64, Charge)> {
    f.budget.log.lock().unwrap().entries().map(|entry| (entry.cost.micros, entry.status)).collect()
}

fn usd(micros: u64) -> Option<Money> {
    Some(Money::new(Currency::USD, micros))
}

#[test]
fn a_sheet_is_quoted_as_one_total_and_each_row_is_charged_once() {
    let mock = MockProvider::standard();
    let mut f = launch(&mock, &[("a red mug", PAINTER), ("a blue mug", PAINTER), ("a green mug", PAINTER)], Launch::default());
    let rows = f.rows.clone();
    let quoted = quote(&mut f, &rows);
    assert_eq!(quoted["steps_total"], 3, "{quoted}");
    assert_eq!(quoted["totals"]["USD"]["micros"], 3 * ROW_MICROS, "every row in one total: {quoted}");
    accept(&mut f, &rows, &quoted).unwrap();
    let notices = drive(&mut f);
    for row in &rows {
        assert!(notices.contains(&RunNotice::Recorded { canvas: SHEET, node: *row }), "{notices:?}");
        assert_eq!(history(&f, *row).len(), 1);
    }
    let sent = keys(&mock);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent.iter().collect::<BTreeSet<_>>().len(), 3, "one key per job: {sent:?}");
    assert_eq!(mock.billed(), 3);
    assert_eq!(charges(&f), vec![(ROW_MICROS, Charge::Actual); 3]);
    let listed = f.session.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap();
    assert!(listed["rows"].as_array().unwrap().iter().all(|row| row["result"] == "complete" && row["stale"] == false), "{listed}");
}

#[test]
fn session_and_project_budgets_refuse_a_sheet_run_that_would_pass_them() {
    let rows = [("a red mug", PAINTER), ("a blue mug", PAINTER), ("a green mug", PAINTER)];
    for budgets in [Budgets { session: usd(100_000), ..Budgets::default() }, Budgets { this_project: usd(100_000), ..Budgets::default() }] {
        let mock = MockProvider::standard();
        let mut f = launch(&mock, &rows, Launch { budgets: budgets.clone(), ..Launch::default() });
        let all = f.rows.clone();
        let quoted = quote(&mut f, &all);
        assert_eq!(quoted["allowed"], false, "0.12 USD is over a 0.10 USD cap: {quoted}");
        assert!(!quoted["blocks"].as_array().unwrap().is_empty(), "the quote says why: {quoted}");
        assert!(accept(&mut f, &all, &quoted).is_err(), "{budgets:?}");
        drive(&mut f);
        assert!(mock.requests().is_empty(), "nothing is sent past a budget");
        // Two rows fit under the same cap, and then the third doesn't.
        start(&mut f, &all[..2]);
        drive(&mut f);
        assert_eq!(mock.requests().len(), 2);
        let quoted = quote(&mut f, &all[2..]);
        assert_eq!(quoted["allowed"], false, "what was spent counts: {quoted}");
    }
}

#[test]
fn a_sheet_runs_at_most_the_providers_lane_limit_at_once_and_cancelling_stops_the_rest_unsent() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), slow)]);
    let prompts: Vec<String> = (0..6).map(|n| format!("mug {n}")).collect();
    let rows: Vec<(&str, &str)> = prompts.iter().map(|prompt| (prompt.as_str(), PAINTER)).collect();
    let mut f = launch(&mock, &rows, Launch { min_ms: 5, ..Launch::default() });
    let all = f.rows.clone();
    start(&mut f, &all);
    pump_until(&mut f, "four rows reached the provider", |f| f.host.jobs().iter().filter(|job| job.sent).count() == 4);
    for _ in 0..50 {
        f.host.pump(&mut f.session);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(mock.requests().len(), 4, "the lane's limit holds while they run");
    let queued: Vec<NodeId> = f.host.jobs().into_iter().filter(|job| job.state == JobState::Queued).map(|job| job.node).collect();
    assert_eq!(queued.len(), 2, "{:?}", f.host.jobs());
    f.host.cancel_all();
    let notices = drive(&mut f);
    for row in &all {
        assert!(notices.contains(&RunNotice::Cancelled { canvas: SHEET, node: *row }), "{row}: {notices:?}");
        assert!(history(&f, *row).is_empty(), "a cancelled row has no result");
    }
    assert_eq!(mock.requests().len(), 4, "the queued rows were never sent");
    for row in queued {
        let job = f.host.jobs().into_iter().rev().find(|job| job.node == row).unwrap();
        assert_eq!((job.state, job.sent), (JobState::Cancelled, false));
    }
    // Settled, not left reserved: the whole sheet can be quoted again.
    assert_eq!(quote(&mut f, &all)["allowed"], true);
}

#[test]
fn a_retry_after_a_lost_reply_keeps_the_jobs_key_so_the_row_is_charged_once() {
    let mut lost = MockModel::new(MockOutput::Image);
    lost.fail = Some(MockFailure::LostReply(ErrorKind::Timeout));
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), lost)]);
    let retry = RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 };
    let mut f = launch(&mock, &[("a red mug", PAINTER), ("a blue mug", PAINTER)], Launch { retry, ..Launch::default() });
    let all = f.rows.clone();
    start(&mut f, &all);
    let notices = drive(&mut f);
    for row in &all {
        assert!(notices.contains(&RunNotice::Recorded { canvas: SHEET, node: *row }), "{notices:?}");
    }
    let sent = keys(&mock);
    assert_eq!(sent.len(), 4, "each row's submit timed out once and was retried: {sent:?}");
    assert_eq!(sent.iter().collect::<BTreeSet<_>>().len(), 2, "a retry is sent under its job's key: {sent:?}");
    assert_eq!(mock.billed(), 2, "the provider ran each row once");
    assert_eq!(charges(&f), vec![(ROW_MICROS, Charge::Actual); 2]);
}

#[test]
fn a_row_cut_off_by_a_restart_is_followed_under_its_key_and_charged_once() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = 1_000;
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), slow)]);
    let journal = MemoryJournal::default();
    let rows = [("a red mug", PAINTER)];
    let mut first = launch(&mock, &rows, Launch { min_ms: 50, journal: Some(&journal), ..Launch::default() });
    let all = first.rows.clone();
    start(&mut first, &all);
    pump_until(&mut first, "the row's handle was never saved", |_| journal.saved().is_some_and(|saved| !saved.submitted.is_empty()));
    first.runtime.kill();
    let project = first.session.document().clone();
    drop(first);
    let key = keys(&mock);
    assert_eq!(key.len(), 1);

    let mut second = launch(&mock, &rows, Launch { journal: Some(&journal), project: Some(project), ..Launch::default() });
    assert!(second.host.has_waiting());
    let binding = second.binding.clone();
    second.host.resume(&second.session, &binding);
    let notices = drive(&mut second);
    assert!(notices.contains(&RunNotice::Recorded { canvas: SHEET, node: all[0] }), "{notices:?}");
    assert_eq!(keys(&mock), key, "followed under its key, never sent again");
    assert_eq!(mock.billed(), 1);
    assert_eq!(history(&second, all[0]).len(), 1);
    assert_eq!(charges(&second), [(ROW_MICROS, Charge::Actual)]);
}

#[test]
fn a_regenerate_is_a_new_run_with_new_keys_that_keeps_the_older_result() {
    let mock = MockProvider::standard();
    let mut f = launch(&mock, &[("a red mug", PAINTER)], Launch::default());
    let row = f.rows[0];
    start(&mut f, &[row]);
    drive(&mut f);
    let first = history(&f, row);
    // The row is up to date, so this is a deliberate regenerate: quoted and charged again.
    let quoted = quote(&mut f, &[row]);
    assert_eq!(quoted["totals"]["USD"]["micros"], ROW_MICROS, "{quoted}");
    accept(&mut f, &[row], &quoted).unwrap();
    drive(&mut f);
    let second = history(&f, row);
    assert_eq!(second.len(), 2, "a new result, not an overwrite");
    assert_eq!(second[0], first[0]);
    let active = f.session.document().canvases().get(&SHEET).unwrap().nodes().get(&row).unwrap().active;
    assert_eq!(active, Some(second[1]), "the new result is active");
    let sent = keys(&mock);
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0], sent[1], "a new run sends under new keys");
    assert_eq!(mock.billed(), 2);
}

#[test]
fn retry_failed_runs_only_the_rows_that_failed_and_leaves_the_successes_alone() {
    let mut broken = MockModel::new(MockOutput::Image);
    broken.fail = Some(MockFailure::Job(ErrorKind::Server));
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), MockModel::new(MockOutput::Image)), ("mock-flaky".to_owned(), broken)]);
    let mut f = launch(&mock, &[("a red mug", PAINTER), ("a blue mug", FLAKY), ("a green mug", PAINTER), ("a grey mug", FLAKY)], Launch::default());
    let all = f.rows.clone();
    start(&mut f, &all);
    let notices = drive(&mut f);
    // A partial failure: the rows that worked keep their results, the others say why.
    let failed: BTreeSet<NodeId> = notices.iter().filter_map(|notice| if let RunNotice::Failed { node, .. } = notice { Some(*node) } else { None }).collect();
    assert_eq!(failed, [all[1], all[3]].into(), "{notices:?}");
    assert_eq!((history(&f, all[0]).len(), history(&f, all[2]).len()), (1, 1));
    assert!(history(&f, all[1]).is_empty() && history(&f, all[3]).is_empty());
    let succeeded = [history(&f, all[0]), history(&f, all[2])];

    let canvas = f.session.document().canvases().get(&SHEET).unwrap();
    let retry = sheet_cmds::batch::retry_failed(canvas, &failed, &f.host.running().into_iter().map(|(_, node)| node).collect());
    assert_eq!(retry, [all[1], all[3]]);
    // The provider is fixed; the retry quotes and runs those rows only.
    let fixed = MockProvider::new("mock", [("mock-flaky".to_owned(), MockModel::new(MockOutput::Image))]);
    route(&mut f.session, &fixed, &all);
    let quoted = quote(&mut f, &retry);
    assert_eq!((quoted["steps_total"].clone(), quoted["totals"]["USD"]["micros"].clone()), (json!(2), json!(2 * ROW_MICROS)), "{quoted}");
    accept(&mut f, &retry, &quoted).unwrap();
    drive(&mut f);
    assert_eq!(fixed.requests().len(), 2, "only the failed rows were sent");
    assert!(fixed.requests().iter().all(|job| job.model == "mock-flaky"));
    assert_eq!([history(&f, all[0]), history(&f, all[2])], succeeded, "the successes were not run again");
    assert_eq!((history(&f, all[1]).len(), history(&f, all[3]).len()), (1, 1));
    let canvas = f.session.document().canvases().get(&SHEET).unwrap();
    assert!(sheet_cmds::batch::retry_failed(canvas, &BTreeSet::new(), &BTreeSet::new()).is_empty(), "nothing is left to retry");
}

/// What a queued or running sheet run does when the sheet is edited: the run is the immutable
/// snapshot `run.start` accepted. An edit between the quote and the start makes the quote stale,
/// so the start is refused and nothing is sent; an edit while the row runs changes nothing the
/// provider was sent, and the result is recorded as out of date.
#[test]
fn a_run_is_the_snapshot_it_was_started_from() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = 20;
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), slow)]);
    let mut f = launch(&mock, &[("a red mug", PAINTER), ("a blue mug", PAINTER)], Launch { min_ms: 10, ..Launch::default() });
    let all = f.rows.clone();
    let edit = |f: &mut Fixture, row: NodeId, prompt: &str| {
        f.session.dispatch("node.param_set", &json!({"canvas": SHEET, "id": row, "name": "prompt", "value": Value::Text(prompt.into())})).unwrap();
    };
    let quoted = quote(&mut f, &all);
    edit(&mut f, all[0], "a red mug, glazed");
    let refused = accept(&mut f, &all, &quoted).unwrap_err();
    assert!(refused.message.contains("quote") || refused.message.contains("changed"), "{}", refused.message);
    drive(&mut f);
    assert!(mock.requests().is_empty(), "a stale quote starts nothing");

    start(&mut f, &all);
    pump_until(&mut f, "both rows were sent", |_| mock.requests().len() == 2);
    edit(&mut f, all[0], "a red mug, matte");
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: SHEET, node: all[0] }), "{notices:?}");
    let sent = mock.requests().into_iter().map(|job| format!("{:?} {:?}", job.params, job.inputs)).collect::<Vec<_>>();
    assert!(sent.iter().any(|params| params.contains("a red mug, glazed")) && !sent.iter().any(|params| params.contains("matte")), "{sent:?}");
    let listed = f.session.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap();
    assert_eq!(listed["rows"][0]["result"], "complete");
    assert_eq!(listed["rows"][0]["stale"], true, "the result is of the prompt it was run with: {listed}");
    assert_eq!(listed["rows"][1]["stale"], false);
}

#[test]
fn a_rows_prompt_that_mentions_a_removed_card_is_blocked_before_anything_is_paid() {
    let mock = MockProvider::standard();
    let gone = NodeId::from_bytes([99; 16]);
    let prompt = format!("a mug in the style of @[Brand notes]({gone})");
    let mut f = launch(&mock, &[(prompt.as_str(), PAINTER), ("a blue mug", PAINTER)], Launch::default());
    let all = f.rows.clone();
    let quoted = quote(&mut f, &all);
    assert_eq!(quoted["allowed"], false, "{quoted}");
    let blocked = quoted["blocked"].to_string();
    assert!(blocked.contains("no longer on this canvas"), "the reason says what to fix: {quoted}");
    assert!(accept(&mut f, &all, &quoted).is_err());
    drive(&mut f);
    assert!(mock.requests().is_empty());
}
