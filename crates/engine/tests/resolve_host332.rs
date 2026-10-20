//! The run host's resolution service (#332): a person's decision for a reconciled charge is
//! saved in the run journal before the live log keeps it, a save cut short leaves the ledger as
//! it was, and a charge a saved run still follows is refused. No provider calls.
use nodecraft_engine::{
    Session,
    commands::{Registry, Services},
    jobs::JobBudget,
    ownership_cmds,
    run_host::RunHost,
    run_journal::{JOURNAL_VERSION, Journal, JournalStore, MemoryJournal, Submitted},
};
use nodecraft_exec::{
    cost::UsageLog,
    driver::{Runtime, TaskFuture},
};
use nodecraft_graph::Project;
use nodecraft_types::{Currency, Error, ErrorKind, ProjectId};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// The legacy fixture with its quarantined group reconciled: the charge "old" is open again.
fn legacy() -> UsageLog {
    let mut log: UsageLog = serde_json::from_str(include_str!("../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    let token: String = log.ownership_group("a", Currency::USD).unwrap().token.iter().map(|b| format!("{b:02x}")).collect();
    ownership_cmds::reconcile(&mut log, &json!({"session": "a", "currency": "USD", "token": token, "holder": "named"}), 0).unwrap();
    log
}

/// A host over `log` saving to `store`, and its session.
fn launch(log: UsageLog, store: Flaky) -> (Session, RunHost, JobBudget) {
    let budget = JobBudget {
        log: Arc::new(Mutex::new(log)),
        budgets: Arc::new(Mutex::new(Default::default())),
        session: "now".into(),
        wall_clock_ms: Arc::new(|| 1_700_000_000_000),
    };
    let mut host = RunHost::new(Arc::new(Idle), budget.clone());
    host.open_journal(Box::new(store)).unwrap();
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    host.register_ownership(&mut services).unwrap();
    host.register_resolution(&mut services).unwrap();
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Resolution").unwrap();
    let mut session =
        Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap().with_services(services);
    host.install(&mut session);
    (session, host, budget)
}

fn settle(session: &mut Session, micros: u64) -> Value {
    let page = session.dispatch("usage.unsettled", &json!({})).unwrap();
    let charge = &page["charges"][0];
    json!({"provider": charge["provider"], "key": charge["key"], "token": charge["token"], "outcome": "settled", "currency": "USD", "micros": micros})
}

#[test]
fn a_settled_charge_is_saved_kept_and_restored() {
    let store = Flaky::default();
    let (mut session, host, budget) = launch(legacy(), store.clone());
    let decision = settle(&mut session, 7);
    let record = session.dispatch("usage.resolve_charge", &decision).unwrap();
    assert_eq!((record["outcome"].clone(), record["at_ms"].clone()), (json!("settled"), json!(1_700_000_000_000u64)));
    assert_eq!(budget.log.lock().unwrap().charge_resolutions().len(), 1);
    assert_eq!(store.inner.load().unwrap().unwrap().usage.charge_resolutions().len(), 1, "saved first");
    host.sync_usage(&mut session);
    assert_eq!(session.dispatch("usage.unsettled", &json!({})).unwrap()["total"], 0, "the snapshot follows");
    // The next launch reads it back.
    let (mut again, _, _) = launch(store.inner.load().unwrap().unwrap().usage, Flaky::default());
    let page = again.dispatch("usage.unsettled", &json!({})).unwrap();
    assert_eq!((page["total"].clone(), page["resolved"].clone()), (json!(0), json!(1)));
}

#[test]
fn a_save_cut_short_leaves_the_ledger_and_the_decision_can_be_confirmed_again() {
    let store = Flaky::default();
    let (mut session, _host, budget) = launch(legacy(), store.clone());
    let before = budget.log.lock().unwrap().clone();
    let decision = settle(&mut session, 7);
    store.broken.store(true, Ordering::SeqCst);
    let refused = session.dispatch("usage.resolve_charge", &decision).unwrap_err();
    assert!(refused.message.contains("wasn't kept"), "{refused:?}");
    assert_eq!(*budget.log.lock().unwrap(), before);
    store.broken.store(false, Ordering::SeqCst);
    session.dispatch("usage.resolve_charge", &decision).unwrap();
    assert_eq!(budget.log.lock().unwrap().charge_resolutions().len(), 1);
}

#[test]
fn a_charge_a_saved_run_still_follows_is_refused() {
    let store = Flaky::default();
    let follower: Submitted = serde_json::from_value(json!({
        "project": ProjectId::from_bytes([1; 16]), "canvas": nodecraft_types::CanvasId::from_bytes([2; 16]), "node": nodecraft_types::NodeId::from_bytes([3; 16]),
        "target": {"adapter": "t"}, "key": "old", "generation": 0, "handle": {"remote_id": "r"}, "model": "m", "routes": []
    }))
    .unwrap();
    store.inner.save(&Journal { version: JOURNAL_VERSION, usage: legacy(), submitted: vec![follower] }).unwrap();
    let (mut session, _host, budget) = launch(UsageLog::default(), store);
    let before = budget.log.lock().unwrap().clone();
    let decision = settle(&mut session, 7);
    assert_eq!(session.dispatch("usage.resolve_charge", &decision).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(*budget.log.lock().unwrap(), before);
}

#[test]
fn a_decision_kept_before_the_first_save_still_saves_the_parked_runs() {
    let store = Flaky::default();
    let parked: Submitted = serde_json::from_value(json!({
        "project": ProjectId::from_bytes([1; 16]), "canvas": nodecraft_types::CanvasId::from_bytes([2; 16]), "node": nodecraft_types::NodeId::from_bytes([3; 16]),
        "target": {"adapter": "other"}, "key": "later", "generation": 0, "handle": {"remote_id": "r"}, "model": "m", "routes": []
    }))
    .unwrap();
    store.inner.save(&Journal { version: JOURNAL_VERSION, usage: legacy(), submitted: vec![parked] }).unwrap();
    let (mut session, _host, _) = launch(UsageLog::default(), store.clone());
    let decision = settle(&mut session, 7);
    session.dispatch("usage.resolve_charge", &decision).unwrap();
    let saved = store.inner.load().unwrap().unwrap();
    assert_eq!(saved.submitted.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), ["later"], "the parked run is still followed after a restart");
}
