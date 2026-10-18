use super::*;
use crate::provider_routing::RegisteredProvider;
use nodecraft_types::ProviderInstanceId;
fn queued(provider: Arc<dyn Provider>, instance: u8, estimate: u64) -> QueuedJob {
    let mut job = priced(provider.clone(), Some(estimate));
    job.provider = RegisteredProvider::named(provider, ProviderInstanceId::from_bytes([instance; 16])).unwrap();
    job
}
fn start(id: u64) -> Start {
    Start { idempotency_key: "same-base".into(), ..job(id) }
}
#[test]
fn same_adapter_and_key_different_actual_instances_do_not_share_handles_or_money() {
    let (runner, budget) = budgeted(Some(100));
    let first = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
    let second = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
    let a = queued(first.clone(), 0, 30);
    let ta = a.provider.target().clone();
    let b = queued(second.clone(), 1, 40);
    let tb = b.provider.target().clone();
    runner.register(JobId(1), a);
    runner.register(JobId(2), b);
    assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
    assert_eq!(first.submits.load(Ordering::SeqCst), 1);
    assert_eq!(second.submits.load(Ordering::SeqCst), 1, "another endpoint must submit to itself, not poll the first endpoint's handle");
    let log = budget.log.lock().unwrap();
    assert!(log.charge_target(&ta, "same-base").is_some());
    assert!(log.charge_target(&tb, "same-base").is_some());
    assert!(log.charge("flaky", "same-base").is_none());
    drop(log);
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    assert_eq!(budget.log.lock().unwrap().charge_target(&ta, "same-base").unwrap().status, Charge::Unresolved);
}
#[test]
fn global_budget_sums_named_instances_and_refuses_before_second_submit() {
    let (runner, budget) = budgeted(Some(50));
    let first = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
    let second = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
    runner.register(JobId(1), queued(first.clone(), 0, 30));
    runner.register(JobId(2), queued(second.clone(), 1, 30));
    assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert!(matches!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(second.submits.load(Ordering::SeqCst), 0);
    assert_eq!(spent(&budget), 30);
}
#[test]
fn restored_named_handle_and_generation_never_leak_to_default_or_other_instance() {
    let (mut runner, _) = budgeted(None);
    runner.budget = None;
    let first = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
    let second = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
    let a = queued(first.clone(), 0, 1);
    let binding = a.provider.clone();
    runner.restore_target_generation(&binding, "same-base".into(), 3, JobHandle { remote_id: "restored".into(), data: serde_json::Value::Null }).unwrap();
    runner.register(JobId(1), a);
    runner.register(JobId(2), queued(second.clone(), 1, 1));
    assert_eq!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Done);
    assert_eq!(first.submits.load(Ordering::SeqCst), 0);
    assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
    assert_eq!(second.submits.load(Ordering::SeqCst), 1);
    assert!(runner.restore_target_generation(&binding, "x".repeat(65), 0, JobHandle { remote_id: "bad".into(), data: serde_json::Value::Null }).is_err());
}

struct LateEndpoint {
    gate: std::sync::atomic::AtomicBool,
    fail: bool,
}
impl Provider for LateEndpoint {
    fn id(&self) -> &str {
        "same-adapter"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(futures::future::poll_fn(move |_| {
            if self.gate.load(Ordering::SeqCst) {
                std::task::Poll::Ready(Ok(Submission::Queued(JobHandle { remote_id: job.idempotency_key.clone(), data: serde_json::Value::Null })))
            } else {
                std::task::Poll::Pending
            }
        }))
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            if self.fail {
                Err(ProviderError::new(ErrorKind::Network, "fixture poll lost"))
            } else {
                Ok(JobProgress::Done(Completed::new(vec![Output::Text("done".into())])))
            }
        })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
#[test]
fn late_old_instance_callback_cannot_prune_new_instances_shared_key_handle_or_lease() {
    let (mut runner, budget) = budgeted(Some(100));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    runner.on_update = Arc::new(move |_, sent, update| {
        if matches!(update, RunUpdate::Submitted(_)) {
            sink.lock().unwrap().push(sent.target.clone());
        }
    });
    let old = Arc::new(LateEndpoint { gate: std::sync::atomic::AtomicBool::new(false), fail: false });
    let new = Arc::new(LateEndpoint { gate: std::sync::atomic::AtomicBool::new(true), fail: true });
    let old_job = queued(old.clone(), 0, 10);
    let old_target = old_job.provider.target().clone();
    let new_job = queued(new, 1, 20);
    let new_target = new_job.provider.target().clone();
    runner.register(JobId(1), old_job);
    let mut pending = runner.run(start(1), CancelToken::default());
    assert!(pending.as_mut().poll(&mut std::task::Context::from_waker(futures::task::noop_waker_ref())).is_pending());
    runner.register(JobId(1), new_job);
    assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    old.gate.store(true, Ordering::SeqCst);
    assert_eq!(block_on(pending), Outcome::Done);
    assert!(runner.lock().handles.contains_key(&(new_target.clone(), "same-base".into())));
    assert!(!runner.lock().handles.contains_key(&(old_target.clone(), "same-base".into())));
    assert!(runner.lock().jobs.get(&JobId(1)).unwrap().scheduler.as_ref().is_some_and(|lease| lease.attempt.provider == new_target));
    let observed = observed.lock().unwrap();
    assert!(observed.contains(&old_target) && observed.contains(&new_target));
    drop(observed);
    let log = budget.log.lock().unwrap();
    assert_eq!(log.charge_target(&old_target, "same-base").unwrap().status, Charge::Actual);
    assert_eq!(log.charge_target(&new_target, "same-base").unwrap().status, Charge::Estimate);
    drop(log);
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    assert!(runner.lock().handles.is_empty());
}
