//! A person resolves an ordinary unresolved charge through the run host (#298): a job that may
//! have been billed (a 401 after sending) keeps its estimate; the person's decision is refused
//! while any run might still settle it, is saved in the run journal before the live log keeps
//! it, survives a save cut short, and is never applied twice. Mock provider only.
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
    run_journal::{JOURNAL_VERSION, Journal, JournalStore, MemoryJournal, Submitted},
};
use nodecraft_exec::{
    cost::{Charge, UsageEntry, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, Currency, EdgeId, Error, ErrorKind, Money, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([98; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const MODEL: &str = "fixture.writer";

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

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

/// Each task on its own thread; sleeps on a timer thread.
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

/// Nothing here runs a job.
struct Idle;
impl Runtime for Idle {
    fn spawn(&self, _: TaskFuture<()>) {}
    fn sleep(&self, _: u64) -> TaskFuture<()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

/// A journal whose saves fail while `broken` is set, as a full disk would.
#[derive(Clone, Default)]
struct Flaky {
    inner: MemoryJournal,
    broken: Arc<AtomicBool>,
}
impl JournalStore for Flaky {
    fn load(&self) -> Result<Option<Journal>, Error> {
        self.inner.load()
    }
    fn save(&self, journal: &Journal) -> Result<(), Error> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::Internal, "no space left on device"));
        }
        self.inner.save(journal)
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    mock: MockProvider,
    budget: JobBudget,
}

/// A text card on a provider that answers every submit with 401 after it was sent.
fn fixture() -> Fixture {
    let mut failing = MockModel::new(MockOutput::Text);
    failing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::Auth));
    let mock = MockProvider::new("mock", [("mock-text".to_owned(), failing)]);
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([97; 16]), "Billing").unwrap();
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
        budgets: Arc::new(Mutex::new(Default::default())),
        session: "billing".into(),
        wall_clock_ms: Arc::new(|| 1_700_000_000_000),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    host.register_resolution(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    Fixture { session, host, mock, budget }
}

/// Quotes and accepts the run; the host has it in its inbox afterwards.
fn start(f: &mut Fixture) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[SOURCE, WRITER]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":amounts});
    f.session.dispatch("run.start", &start).unwrap();
}

fn drive(f: &mut Fixture) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        f.host.pump(&mut f.session);
        if !f.host.is_busy() {
            return;
        }
        assert!(Instant::now() < deadline, "the run never finished");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The decision for the first waiting charge.
fn release_first(session: &mut Session) -> serde_json::Value {
    let page = session.dispatch("usage.unresolved", &json!({})).unwrap();
    let charge = &page["charges"][0];
    json!({"provider": charge["provider"], "instance": charge["instance"], "key": charge["key"], "token": charge["token"], "outcome": "released", "note": "not on the invoice"})
}

fn spent(budget: &JobBudget) -> u64 {
    budget.log.lock().unwrap().reported_spent("billing").session.get(&Currency::USD).map_or(0, |m| m.micros)
}

#[test]
fn a_charge_left_unresolved_by_a_401_is_released_only_once_no_run_can_settle_it() {
    let mut f = fixture();
    start(&mut f);
    drive(&mut f);
    assert_eq!(f.mock.requests().len(), 1, "sent once, then refused with 401");
    // A 401 after sending doesn't prove nothing was billed: the estimate stays, unresolved.
    assert_eq!(f.budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved]);
    assert_eq!(spent(&f.budget), 10_000);
    let page = f.session.dispatch("usage.unresolved", &json!({})).unwrap();
    assert_eq!((page["total"].clone(), page["charges"][0]["amount"]["micros"].clone()), (json!(1), json!(10_000)));
    let decision = release_first(&mut f.session);
    let mut preview = decision.clone();
    preview["dry_run"] = json!(true);

    // Another run waits to start: it might retry under the same key, so the decision waits.
    start(&mut f);
    let before = f.budget.log.lock().unwrap().clone();
    assert_eq!(f.session.dispatch("usage.resolve_unresolved", &decision).unwrap_err().kind, ErrorKind::Busy);
    // Started and going: still refused. A preview changes nothing and stays available.
    f.host.pump(&mut f.session);
    assert!(f.host.is_busy());
    assert_eq!(f.session.dispatch("usage.resolve_unresolved", &decision).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(f.session.dispatch("usage.resolve_unresolved", &preview).unwrap()["session_total"]["micros"], 0);
    drive(&mut f);
    assert!(f.budget.log.lock().unwrap().entries().count() >= 2, "the second run charged under its own key");
    let first = before.entries().next().cloned().unwrap();
    let now = f.budget.log.lock().unwrap().entries().find(|e| e.idempotency_key == first.idempotency_key).cloned();
    assert_eq!(now, Some(first), "the waiting charge was left as it was");

    // Idle again: the person's decision is kept, and only once.
    let total = spent(&f.budget);
    let record = f.session.dispatch("usage.resolve_unresolved", &decision).unwrap();
    assert_eq!((record["outcome"].clone(), record["note"].clone()), (json!("released"), json!("not on the invoice")));
    assert_eq!(spent(&f.budget), total - 10_000, "the released estimate leaves the totals");
    assert_eq!(f.session.dispatch("usage.resolve_unresolved", &decision).unwrap_err().kind, ErrorKind::NotFound, "never twice");
    assert_eq!(spent(&f.budget), total - 10_000);
    f.host.sync_usage(&mut f.session);
    let page = f.session.dispatch("usage.unresolved", &json!({})).unwrap();
    assert_eq!(page["resolved"], 1, "the snapshot follows");
}

/// A host over a journal that saved `usage` (and `submitted`) at the last quit.
fn launch(store: Flaky) -> (Session, RunHost, JobBudget) {
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Default::default())),
        session: "now".into(),
        wall_clock_ms: Arc::new(|| 1_700_000_000_000),
    };
    let mut host = RunHost::new(Arc::new(Idle), budget.clone());
    host.open_journal(Box::new(store)).unwrap();
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    host.register_resolution(&mut services).unwrap();
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Billing").unwrap();
    let mut session =
        Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap().with_services(services);
    host.install(&mut session);
    (session, host, budget)
}

/// An estimate the last launch left open with no handle: its submit may have reached the
/// provider, so reopening the journal charges it as unresolved.
fn left_open(key: &str) -> UsageLog {
    let mut log = UsageLog::default();
    log.record(UsageEntry {
        at_ms: 5,
        session: "before".into(),
        node: NodeId::from_bytes([3; 16]),
        provider: "t".into(),
        instance: None,
        project: None,
        model: "m".into(),
        cost: usd(40),
        idempotency_key: key.into(),
        status: Charge::Estimate,
    })
    .unwrap();
    log
}

fn saved(store: &Flaky, usage: UsageLog, submitted: Vec<Submitted>) {
    store.inner.save(&Journal { version: JOURNAL_VERSION, usage, submitted }).unwrap();
}

#[test]
fn a_decision_is_saved_before_it_is_kept_and_a_save_cut_short_changes_nothing() {
    let store = Flaky::default();
    saved(&store, left_open("lost"), Vec::new());
    let (mut session, _host, budget) = launch(store.clone());
    assert_eq!(budget.log.lock().unwrap().charge("t", "lost").map(|e| e.status), Some(Charge::Unresolved));
    let mut decision = release_first(&mut session);
    decision["outcome"] = json!("settled");
    decision["currency"] = json!("USD");
    decision["micros"] = json!(55);
    let before = budget.log.lock().unwrap().clone();
    store.broken.store(true, Ordering::SeqCst);
    let refused = session.dispatch("usage.resolve_unresolved", &decision).unwrap_err();
    assert!(refused.message.contains("wasn't kept"), "{refused:?}");
    assert_eq!(*budget.log.lock().unwrap(), before, "neither the live log nor the journal took it");
    assert!(store.inner.load().unwrap().unwrap().usage.billing_resolutions().next().is_none());
    store.broken.store(false, Ordering::SeqCst);
    session.dispatch("usage.resolve_unresolved", &decision).unwrap();
    assert_eq!(budget.log.lock().unwrap().charge("t", "lost").map(|e| (e.cost, e.status)), Some((usd(55), Charge::Actual)));
    let journal = store.inner.load().unwrap().unwrap();
    assert_eq!(journal.usage.billing_resolutions().len(), 1, "saved first");
    // The next launch reads the decision back, and has nothing left to decide.
    let again = Flaky::default();
    saved(&again, journal.usage, Vec::new());
    let (mut next, _, budget) = launch(again);
    let page = next.dispatch("usage.unresolved", &json!({})).unwrap();
    assert_eq!((page["total"].clone(), page["resolved"].clone()), (json!(0), json!(1)));
    assert_eq!(budget.log.lock().unwrap().reported_spent("before").session[&Currency::USD], usd(55));
}

#[test]
fn a_charge_a_parked_run_still_follows_is_refused() {
    let store = Flaky::default();
    let mut usage = left_open("held");
    usage.settle("t", "held", usd(40), Charge::Unresolved).unwrap();
    let follower: Submitted = serde_json::from_value(json!({
        "project": ProjectId::from_bytes([9; 16]), "canvas": CanvasId::from_bytes([2; 16]), "node": NodeId::from_bytes([3; 16]),
        "target": {"adapter": "t"}, "key": "held", "generation": 0, "handle": {"remote_id": "r"}, "model": "m", "routes": []
    }))
    .unwrap();
    saved(&store, usage, vec![follower]);
    let (mut session, host, budget) = launch(store);
    assert!(host.has_waiting(), "parked for its project");
    let before = budget.log.lock().unwrap().clone();
    let decision = release_first(&mut session);
    assert_eq!(session.dispatch("usage.resolve_unresolved", &decision).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(*budget.log.lock().unwrap(), before);
}
