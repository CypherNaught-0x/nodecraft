//! The run host's ownership service (#362, #384): a confirmed decision is saved in the run
//! journal before the live log keeps it, the snapshot reviews read follows, and a save that is
//! cut short leaves the ledger as it was. No provider calls.
use nodecraft_engine::{
    Session,
    commands::{Registry, Services},
    jobs::JobBudget,
    run_host::RunHost,
    run_journal::{Journal, JournalStore, MemoryJournal},
};
use nodecraft_exec::{
    cost::UsageLog,
    driver::{Runtime, TaskFuture},
};
use nodecraft_graph::Project;
use nodecraft_types::{Error, ErrorKind, ProjectId};
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

fn legacy() -> UsageLog {
    serde_json::from_str(include_str!("../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap()
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
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Ownership").unwrap();
    let mut session =
        Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap().with_services(services);
    host.install(&mut session);
    (session, host, budget)
}

fn decision(session: &mut Session) -> Value {
    let groups = session.dispatch("usage.ownership", &json!({})).unwrap();
    let group = &groups["groups"][0];
    let detail = session.dispatch("usage.ownership", &json!({"session": group["session"], "currency": group["currency"]})).unwrap();
    json!({"session": group["session"], "currency": group["currency"], "token": detail["group"]["token"], "holder": "earlier"})
}

#[test]
fn a_confirmed_decision_is_saved_then_kept_and_reviews_see_it() {
    let store = Flaky::default();
    let (mut session, mut host, budget) = launch(legacy(), store.clone());
    let before = session.dispatch("usage.ownership", &json!({})).unwrap();
    assert_eq!(before["total"], 1);
    let decision = decision(&mut session);

    let mut preview = decision.clone();
    preview["dry_run"] = json!(true);
    assert_eq!(session.dispatch("usage.reconcile_ownership", &preview).unwrap()["dry_run"], true);
    assert_eq!(*budget.log.lock().unwrap(), legacy(), "a preview changes nothing");

    let record = session.dispatch("usage.reconcile_ownership", &decision).unwrap();
    assert_eq!(record["partial"], false);
    let saved = store.inner.saved().expect("the decision is saved");
    assert_eq!(saved.usage.ownership_reconciliations().len(), 1);
    assert_eq!(saved.usage, *budget.log.lock().unwrap());

    // The next frame's pump refreshes the snapshot: the group is gone and the record counted.
    host.pump(&mut session);
    let after = session.dispatch("usage.ownership", &json!({})).unwrap();
    assert_eq!((after["total"].clone(), after["reconciled"].clone()), (json!(0), json!(1)));
    // The same token again is refused: the group changed.
    assert!(session.dispatch("usage.reconcile_ownership", &decision).is_err());

    // A later launch restores the reconciled log.
    let (mut again, _host, _) = launch(UsageLog::default(), store);
    assert_eq!(again.dispatch("usage.ownership", &json!({})).unwrap()["reconciled"], 1);
}

#[test]
fn a_save_cut_short_keeps_the_ledger_as_it_was_and_the_decision_can_be_confirmed_again() {
    let store = Flaky::default();
    let (mut session, mut host, budget) = launch(legacy(), store.clone());
    let decision = decision(&mut session);
    store.broken.store(true, Ordering::SeqCst);
    let error = session.dispatch("usage.reconcile_ownership", &decision).unwrap_err();
    assert!(error.message.contains("wasn't kept"), "{}", error.message);
    assert!(error.message.contains("no space left on device"), "{}", error.message);
    assert_eq!(*budget.log.lock().unwrap(), legacy(), "the live log is untouched");
    assert!(store.inner.saved().is_none_or(|saved| saved.usage.ownership_reconciliations().is_empty()));
    host.pump(&mut session);
    assert_eq!(session.dispatch("usage.ownership", &json!({})).unwrap()["total"], 1);

    // Once saving works again, the reviewed token still holds.
    store.broken.store(false, Ordering::SeqCst);
    session.dispatch("usage.reconcile_ownership", &decision).unwrap();
    host.sync_usage(&mut session);
    assert_eq!(session.dispatch("usage.ownership", &json!({})).unwrap()["total"], 0);
}

#[test]
fn without_a_journal_the_decision_is_kept_in_memory() {
    let budget = JobBudget {
        log: Arc::new(Mutex::new(legacy())),
        budgets: Arc::new(Mutex::new(Default::default())),
        session: "now".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Idle), budget.clone());
    let mut services = Services::default();
    host.register_ownership(&mut services).unwrap();
    let project = Project::new(ProjectId::from_bytes([1; 16]), "Ownership").unwrap();
    let mut session =
        Session::with_registries(project, Registry::builtins().unwrap(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap().with_services(services);
    host.install(&mut session);
    let decision = decision(&mut session);
    session.dispatch("usage.reconcile_ownership", &decision).unwrap();
    assert_eq!(budget.log.lock().unwrap().ownership_reconciliations().len(), 1);
}

#[test]
fn a_running_job_still_records_its_charge_after_a_decision() {
    let store = Flaky::default();
    let (mut session, _host, budget) = launch(legacy(), store);
    // A paid job in flight holds a reservation in the live log.
    let mut permit = budget
        .log
        .lock()
        .unwrap()
        .reserve_metadata(nodecraft_exec::cost::UsageMetadata {
            at_ms: 1,
            session: "now",
            node: nodecraft_types::NodeId::from_bytes([9; 16]),
            provider: "mock",
            instance: None,
            model: "m",
            idempotency_key: "running-job",
        })
        .unwrap();
    let decision = decision(&mut session);
    session.dispatch("usage.reconcile_ownership", &decision).unwrap();
    let mut log = budget.log.lock().unwrap();
    assert_eq!(log.ownership_reconciliations().len(), 1);
    log.validate_metadata(&permit).unwrap();
    log.finalize_metadata(&mut permit, nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1)).unwrap();
}
