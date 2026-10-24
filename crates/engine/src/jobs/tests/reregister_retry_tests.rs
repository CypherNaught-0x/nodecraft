use super::*;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy)]
enum Reply {
    Lost,
    Failed,
    Done,
}
struct Script {
    keys: Mutex<Vec<String>>,
    polled: Mutex<Vec<String>>,
    replies: Mutex<Vec<Reply>>,
    refuse: AtomicBool,
    gate: AtomicBool,
}
impl Script {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            keys: Mutex::default(),
            polled: Mutex::default(),
            replies: Mutex::new(replies.into_iter().rev().collect()),
            refuse: AtomicBool::new(false),
            gate: AtomicBool::new(true),
        })
    }
}
impl Provider for Script {
    fn id(&self) -> &str {
        "reregister"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.keys.lock().unwrap().push(job.idempotency_key.clone());
        Box::pin(futures::future::poll_fn(move |_| {
            if !self.gate.load(Ordering::SeqCst) {
                return std::task::Poll::Pending;
            }
            std::task::Poll::Ready(if self.refuse.load(Ordering::SeqCst) {
                Err(ProviderError::new(ErrorKind::Auth, "local fixture refusal"))
            } else {
                Ok(Submission::Queued(JobHandle { remote_id: job.idempotency_key.clone(), data: serde_json::Value::Null }))
            })
        }))
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.polled.lock().unwrap().push(handle.remote_id.clone());
        let reply = self.replies.lock().unwrap().pop().unwrap_or(Reply::Done);
        Box::pin(async move {
            match reply {
                Reply::Lost => Err(ProviderError::new(ErrorKind::Network, "poll lost")),
                Reply::Failed => Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, "generation failed"))),
                Reply::Done => Ok(JobProgress::Done(Completed::new(vec![Output::Text("done".into())]))),
            }
        })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
fn q(provider: Arc<dyn Provider>, key: &str, price: Option<u64>) -> QueuedJob {
    let mut q = priced(provider, price);
    q.job.idempotency_key = key.into();
    q
}
fn start(id: u64, key: &str) -> Start {
    Start { idempotency_key: key.into(), ..job(id) }
}

#[test]
fn reregister_new_payload_key_keeps_old_scheduler_retry_handle() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
    let (runner, budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), q(p.clone(), "old", Some(10)));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(1), q(p.clone(), "proposed", Some(10)));
    assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old"], "retry must poll, not submit again");
    assert!(p.polled.lock().unwrap().iter().all(|h| h == "old"));
    assert_eq!(budget.log.lock().unwrap().entries().count(), 1);
}
#[test]
fn reregister_new_payload_key_keeps_provider_failed_generation_count() {
    let p = Script::new(vec![Reply::Failed, Reply::Done]);
    let (runner, _) = budgeted(Some(1_000_000));
    runner.register(JobId(1), q(p.clone(), "old", Some(10)));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(1), q(p.clone(), "proposed", Some(10)));
    assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old", "old#1"]);
}
#[test]
fn reregister_preserves_inactive_unknown_price_metadata_permit() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
    let (runner, _) = budgeted(None);
    runner.register(JobId(1), q(p.clone(), "old", None));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(runner.lock().metadata.len(), 1);
    runner.register(JobId(1), q(p.clone(), "proposed", None));
    assert_eq!(runner.lock().metadata.len(), 1, "held accounting capability survives scheduler retry");
    assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old"]);
}
#[test]
fn local_no_handle_failure_does_not_delete_other_jobs_shared_accepted_handle() {
    let p = Script::new(vec![Reply::Done]);
    let (mut runner, _) = budgeted(None);
    runner.budget = None;
    p.gate.store(false, Ordering::SeqCst);
    p.refuse.store(true, Ordering::SeqCst);
    runner.register(JobId(1), q(p.clone(), "shared", None));
    let mut run = runner.run(start(1, "shared"), CancelToken::default());
    assert!(run.as_mut().poll(&mut std::task::Context::from_waker(futures::task::noop_waker_ref())).is_pending());
    runner.register(JobId(2), q(p.clone(), "shared", None));
    runner.restore_handle("reregister", "shared".into(), JobHandle { remote_id: "accepted".into(), data: serde_json::Value::Null }).unwrap();
    p.gate.store(true, Ordering::SeqCst);
    assert!(matches!(block_on(run), Outcome::Failed { transient: false, .. }));
    assert_eq!(block_on(runner.run(start(2, "shared"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["shared"]);
    assert_eq!(*p.polled.lock().unwrap(), ["accepted"]);
}

#[test]
fn fresh_start_and_terminal_settle_retire_only_the_scheduler_lease() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Lost, Reply::Lost, Reply::Done]);
    let (runner, _) = budgeted(None);
    runner.register(JobId(1), q(p.clone(), "old", None));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    // Settling leaves an ordinary same-key registration reference intact.
    runner.settle(JobId(1));
    assert!(runner.lock().jobs.get(&JobId(1)).unwrap().scheduler.is_none());
    assert!(runner.lock().handles.contains_key(&(nodecraft_types::ProviderTarget::default_instance("reregister").unwrap(), "old".into())));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(1), q(p.clone(), "proposed", None));
    runner.settle(JobId(1));
    assert!(!runner.lock().handles.contains_key(&(nodecraft_types::ProviderTarget::default_instance("reregister").unwrap(), "old".into())));
    assert_eq!(block_on(runner.run(start(1, "fresh"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old", "fresh"]);
    assert!(runner.lock().jobs.is_empty() && runner.lock().failures.is_empty() && runner.lock().handles.is_empty());
}
#[test]
fn register_churn_is_bounded_and_fresh_start_forgets_old_generation() {
    let p = Script::new(vec![Reply::Failed, Reply::Done]);
    let (runner, _) = budgeted(None);
    runner.register(JobId(1), q(p.clone(), "old", None));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    for i in 0..100 {
        runner.register(JobId(1), q(p.clone(), &format!("proposed{i}"), None));
    }
    assert_eq!(runner.lock().jobs.len(), 1);
    assert_eq!(runner.lock().jobs.values().filter(|j| j.scheduler.is_some()).count(), 1);
    assert_eq!(block_on(runner.run(start(1, "fresh"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old", "fresh"]);
    runner.forget(JobId(1));
    assert!(runner.lock().failures.is_empty() && runner.lock().handles.is_empty() && runner.lock().metadata.is_empty());
}
#[test]
fn same_provider_replacement_during_submit_keeps_late_retry_ownership() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
    let (runner, _) = budgeted(None);
    p.gate.store(false, Ordering::SeqCst);
    runner.register(JobId(1), q(p.clone(), "old", None));
    let mut old = runner.run(start(1, "old"), CancelToken::default());
    assert!(old.as_mut().poll(&mut std::task::Context::from_waker(futures::task::noop_waker_ref())).is_pending());
    runner.register(JobId(1), q(p.clone(), "proposed", None));
    p.gate.store(true, Ordering::SeqCst);
    assert!(matches!(block_on(old), Outcome::Failed { transient: true, .. }));
    assert_eq!(runner.lock().metadata.len(), 1);
    assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old"]);
}
#[test]
fn failed_admission_does_not_retire_the_current_scheduler_lease() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
    let (runner, budget) = budgeted(None);
    runner.register(JobId(1), q(p.clone(), "old", None));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(1), q(p.clone(), "proposed", None));
    budget.budgets.lock().unwrap().per_call = Some(usd(1));
    assert!(matches!(block_on(runner.run(start(1, "fresh"), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert!(runner.lock().jobs.get(&JobId(1)).unwrap().scheduler.is_some());
    assert_eq!(runner.lock().metadata.len(), 1);
    budget.budgets.lock().unwrap().per_call = None;
    assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old"]);
}

#[derive(Default)]
struct PumpRuntime {
    tasks: Mutex<Vec<TaskFuture<()>>>,
    now: AtomicU64,
}
impl Runtime for PumpRuntime {
    fn spawn(&self, task: TaskFuture<()>) {
        self.tasks.lock().unwrap().push(task);
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        self.now.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}
#[test]
fn driver_retry_after_reregister_resumes_remote_handle_once() {
    use nodecraft_exec::{
        driver::driver,
        scheduler::{Event, JobSpec, Limits, RetryPolicy, Scheduler},
    };
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
    let rt = Arc::new(PumpRuntime::default());
    let completed = Arc::new(AtomicU64::new(0));
    let c = completed.clone();
    let runner = Arc::new(ProviderRunner::new(
        rt.clone(),
        RunLimits { max_poll_errors: 1, ..RunLimits::default() },
        Arc::new(move |_, _| {
            c.fetch_add(1, Ordering::SeqCst);
        }),
        Arc::new(|_, _, _| {}),
    ));
    runner.register(JobId(1), q(p.clone(), "old", None));
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler
        .add(vec![JobSpec { id: JobId(1), deps: vec![], lane: Lane::new("reregister"), retry: RetryPolicy::default(), idempotency_key: "old".into() }])
        .unwrap();
    let (driver, handle) = driver(scheduler);
    drop(handle);
    let retries = Arc::new(AtomicU64::new(0));
    let count = retries.clone();
    let re = runner.clone();
    let provider = p.clone();
    let mut task = Box::pin(driver.run(runner.clone(), rt.clone(), move |event| {
        if matches!(event, Event::Retrying { .. }) {
            count.fetch_add(1, Ordering::SeqCst);
            re.register(JobId(1), q(provider.clone(), "proposed", None));
        }
    }));
    let mut cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
    let mut finished = None;
    for _ in 0..100 {
        if let std::task::Poll::Ready(value) = task.as_mut().poll(&mut cx) {
            finished = Some(value);
            break;
        }
        let tasks = std::mem::take(&mut *rt.tasks.lock().unwrap());
        for mut pending in tasks {
            if pending.as_mut().poll(&mut cx).is_pending() {
                rt.tasks.lock().unwrap().push(pending);
            }
        }
    }
    assert_eq!(finished.expect("bounded driver completion").summary().done, 1);
    assert_eq!(retries.load(Ordering::SeqCst), 1);
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(*p.keys.lock().unwrap(), ["old"]);
}
#[test]
fn initially_cancelled_matching_retry_retires_lease_without_forgetting_registration() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost]);
    let (runner, _) = budgeted(None);
    runner.register(JobId(1), q(p.clone(), "old", None));
    assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let cancel = CancelToken::default();
    cancel.cancel();
    assert_eq!(block_on(runner.run(start(1, "old"), cancel)), Outcome::Cancelled);
    assert!(runner.lock().jobs.get(&JobId(1)).unwrap().scheduler.is_none());
    assert!(runner.lock().metadata.is_empty());
    assert_eq!(*p.keys.lock().unwrap(), ["old"]);
    runner.forget(JobId(1));
    assert!(runner.lock().handles.is_empty());
}

#[test]
fn precancelled_duplicate_start_cannot_retire_an_active_scheduler_lease() {
    let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
    let (runner, _) = budgeted(None);
    p.gate.store(false, Ordering::SeqCst);
    runner.register(JobId(1), q(p.clone(), "old", None));
    let mut old = runner.run(start(1, "old"), CancelToken::default());
    assert!(old.as_mut().poll(&mut std::task::Context::from_waker(futures::task::noop_waker_ref())).is_pending());
    let cancelled = CancelToken::default();
    cancelled.cancel();
    assert_eq!(block_on(runner.run(start(1, "old"), cancelled)), Outcome::Cancelled);
    runner.register(JobId(1), q(p.clone(), "proposed", None));
    p.gate.store(true, Ordering::SeqCst);
    assert!(matches!(block_on(old), Outcome::Failed { transient: true, .. }));
    let retained = runner.lock().metadata.len();
    assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
    assert_eq!(*p.keys.lock().unwrap(), ["old"], "cancelled duplicate must not erase active retry ownership");
    assert_eq!(retained, 1);
}
struct Generations {
    old_ready: AtomicBool,
    new_ready: AtomicBool,
    polls: Mutex<Vec<String>>,
}
impl Provider for Generations {
    fn id(&self) -> &str {
        "generations"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { Ok(Submission::Queued(JobHandle { remote_id: job.idempotency_key.clone(), data: serde_json::Value::Null })) })
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.polls.lock().unwrap().push(h.remote_id.clone());
        Box::pin(futures::future::poll_fn(move |_| {
            let ready = if h.remote_id.ends_with("#1") { &self.new_ready } else { &self.old_ready };
            if ready.load(Ordering::SeqCst) {
                std::task::Poll::Ready(Ok(JobProgress::Done(Completed::new(vec![Output::Text(h.remote_id.clone())]))))
            } else {
                std::task::Poll::Pending
            }
        }))
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
#[test]
fn older_restored_generation_success_preserves_newer_same_base_lease() {
    let p = Arc::new(Generations { old_ready: AtomicBool::new(false), new_ready: AtomicBool::new(false), polls: Mutex::default() });
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let sink = outputs.clone();
    let runner = ProviderRunner::new(
        Arc::new(InstantRuntime(AtomicU64::new(0))),
        RunLimits::default(),
        Arc::new(move |_, done| {
            sink.lock().unwrap().push(done.completed);
        }),
        Arc::new(|_, _, _| {}),
    );
    runner.register(JobId(1), q(p.clone(), "base", None));
    let mut old = runner.run(start(1, "base"), CancelToken::default());
    let mut cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(old.as_mut().poll(&mut cx).is_pending());
    runner.restore_generation("generations", "base".into(), 1, JobHandle { remote_id: "base#1".into(), data: serde_json::Value::Null }).unwrap();
    let mut new = runner.run(start(1, "base"), CancelToken::default());
    assert!(new.as_mut().poll(&mut cx).is_pending());
    let serial = runner.lock().running_serial[&JobId(1)];
    let refused = runner
        .restore_generation("invalid-address".repeat(40), "base".into(), u32::MAX, JobHandle { remote_id: "uninstalled".into(), data: serde_json::Value::Null })
        .unwrap_err();
    assert_eq!(refused.into_handle().remote_id, "uninstalled");
    assert_eq!(runner.lock().running_serial[&JobId(1)], serial, "refusal preserves the live replacement");
    p.old_ready.store(true, Ordering::SeqCst);
    assert_eq!(block_on(old), Outcome::Done);
    let retained = runner.lock().jobs.get(&JobId(1)).and_then(|j| j.scheduler.as_ref()).is_some_and(|lease| lease.invocation == serial);
    p.new_ready.store(true, Ordering::SeqCst);
    assert_eq!(block_on(new), Outcome::Done);
    assert!(retained, "old success cannot delete a newer same-base scheduler lease");
    assert_eq!(outputs.lock().unwrap().len(), 2, "both completed results stay caller-owned");
    assert_eq!(*p.polls.lock().unwrap(), ["base", "base#1"]);
    assert!(runner.lock().jobs.is_empty() && runner.lock().running.is_empty());
}

#[test]
fn different_key_precancel_preserves_old_retry_until_admission_or_explicit_settle() {
    for settle_first in [false, true] {
        let p = Script::new(vec![Reply::Lost, Reply::Lost, Reply::Done]);
        let (mut runner, budget) = budgeted(None);
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let delivered = Arc::clone(&outputs);
        runner.on_output = Arc::new(move |_, output| delivered.lock().unwrap().push(output));
        runner.register(JobId(1), q(p.clone(), "old", None));
        assert!(matches!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
        let lease = runner.lock().jobs.get(&JobId(1)).unwrap().scheduler.clone().unwrap();
        let registration = Arc::clone(&runner.lock().jobs.get(&JobId(1)).unwrap().identity);
        for key in ["proposed", "another-proposal"] {
            let cancelled = CancelToken::default();
            cancelled.cancel();
            assert_eq!(block_on(runner.run(start(1, key), cancelled)), Outcome::Cancelled);
            assert_eq!(*p.keys.lock().unwrap(), ["old"], "a cancelled proposal cannot reach the provider");
            assert_eq!(*p.polled.lock().unwrap(), ["old", "old"]);
            assert!(outputs.lock().unwrap().is_empty());
            assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
            let state = runner.lock();
            assert_eq!(state.metadata.len(), 1, "proposed keys must not accumulate inactive permits");
            assert!(state.owns_scheduler(JobId(1), &lease));
            assert!(Arc::ptr_eq(&state.jobs.get(&JobId(1)).unwrap().identity, &registration));
        }
        if settle_first {
            runner.settle(JobId(1));
            assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
            let state = runner.lock();
            assert!(state.metadata.is_empty());
            assert!(state.jobs.get(&JobId(1)).unwrap().scheduler.is_none());
            assert!(!state.handles.contains_key(&(nodecraft_types::ProviderTarget::default_instance("reregister").unwrap(), "old".into())));
            drop(state);
            assert_eq!(block_on(runner.run(start(1, "fresh"), CancelToken::default())), Outcome::Done);
            assert_eq!(*p.keys.lock().unwrap(), ["old", "fresh"], "host acknowledgement ends the old retry lifetime");
            assert_eq!(*p.polled.lock().unwrap(), ["old", "old", "fresh"]);
        } else {
            assert_eq!(block_on(runner.run(start(1, "old"), CancelToken::default())), Outcome::Done);
            assert_eq!(*p.keys.lock().unwrap(), ["old"], "the original accepted handle must resume without a second submit");
            assert_eq!(*p.polled.lock().unwrap(), ["old", "old", "old"]);
        }
        let outputs = outputs.lock().unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0].completed.outputs.as_slice(), [Output::Text(text)] if text == "done"));
        assert!(matches!(outputs[0].accounting, CompletionAccounting::NoReportedCost));
        assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "unknown cost must not fabricate zero money");
        runner.forget(JobId(1));
        let state = runner.lock();
        assert!(state.jobs.is_empty() && state.metadata.is_empty() && state.handles.is_empty());
    }
}
