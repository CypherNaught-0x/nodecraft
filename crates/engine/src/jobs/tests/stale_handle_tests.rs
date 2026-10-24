use super::*;
use std::sync::atomic::AtomicBool;

struct Controlled {
    id: &'static str,
    submit: AtomicBool,
    fail_poll: AtomicBool,
    submits: AtomicU64,
}
impl Controlled {
    fn new(id: &'static str, ready: bool) -> Self {
        Self { id, submit: AtomicBool::new(ready), fail_poll: AtomicBool::new(false), submits: AtomicU64::new(0) }
    }
}
impl Provider for Controlled {
    fn id(&self) -> &str {
        self.id
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::future::poll_fn(move |_| {
            if self.submit.load(Ordering::SeqCst) {
                std::task::Poll::Ready(Ok(Submission::Queued(handle(&job.idempotency_key))))
            } else {
                std::task::Poll::Pending
            }
        }))
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(futures::future::poll_fn(move |_| {
            if self.fail_poll.load(Ordering::SeqCst) {
                std::task::Poll::Ready(Err(ProviderError::new(ErrorKind::Network, "controlled poll failure")))
            } else {
                std::task::Poll::Pending
            }
        }))
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
fn handle(id: &str) -> JobHandle {
    JobHandle { remote_id: id.into(), data: serde_json::Value::Null }
}
fn queued(provider: Arc<dyn Provider>, key: &str) -> QueuedJob {
    let mut q = priced(provider, Some(100_000));
    q.job.idempotency_key = key.into();
    q
}
fn starting(id: u64, key: &str) -> Start {
    Start { idempotency_key: key.into(), ..job(id) }
}
fn pending(run: &mut TaskFuture<Outcome>) {
    assert!(run.as_mut().poll(&mut std::task::Context::from_waker(&futures::task::noop_waker())).is_pending());
}
fn setup(paid: bool) -> (ProviderRunner, JobBudget) {
    let (mut runner, budget) = budgeted(Some(1_000_000));
    if !paid {
        runner.budget = None;
    }
    (runner, budget)
}
fn has(runner: &ProviderRunner, provider: &str, key: &str) -> bool {
    runner.lock().handles.contains_key(&(nodecraft_types::ProviderTarget::default_instance(provider).unwrap(), key.into()))
}
fn charge(budget: &JobBudget, provider: &str, key: &str) -> Option<(u64, Charge)> {
    budget.log.lock().unwrap().charge(provider, key).map(|e| (e.cost.micros, e.status))
}

#[test]
fn same_provider_proposed_key_keeps_scheduler_handle_and_matching_retry_resumes() {
    for paid in [false, true] {
        let (runner, budget) = setup(paid);
        let provider = Arc::new(Controlled::new("controlled", true));
        provider.fail_poll.store(true, Ordering::SeqCst);
        runner.register(JobId(1), queued(provider.clone(), "old"));
        assert!(matches!(block_on(runner.run(starting(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert!(has(&runner, "controlled", "old"));
        runner.register(JobId(1), queued(provider.clone(), "old"));
        assert!(matches!(block_on(runner.run(starting(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(provider.submits.load(Ordering::SeqCst), 1, "matching retry resumes");
        runner.register(JobId(1), queued(provider, "new"));
        assert!(has(&runner, "controlled", "old"), "proposed registration does not retire the scheduler retry");
        assert_eq!(runner.lock().jobs[&JobId(1)].job.idempotency_key, "new");
        if paid {
            runner.settle(JobId(1));
            assert_eq!(charge(&budget, "controlled", "old"), Some((100_000, Charge::Unresolved)));
            assert_eq!(spent(&budget), 100_000);
        }
    }
}

#[test]
fn late_submitted_old_transient_or_drop_preserves_running_replacement() {
    for paid in [false, true] {
        for drop_old in [false, true] {
            for replacement_provider in ["controlled", "replacement"] {
                let (runner, budget) = setup(paid);
                let old_provider = Arc::new(Controlled::new("controlled", false));
                runner.register(JobId(1), queued(old_provider.clone(), "old"));
                let mut old = runner.run(starting(1, "old"), CancelToken::default());
                pending(&mut old);
                let replacement = Arc::new(Controlled::new(replacement_provider, true));
                runner.register(JobId(1), queued(replacement, "new"));
                let mut new = runner.run(starting(1, "new"), CancelToken::default());
                pending(&mut new);
                assert!(has(&runner, replacement_provider, "new"));
                runner.settle(JobId(1));
                runner.forget(JobId(1));
                old_provider.submit.store(true, Ordering::SeqCst);
                pending(&mut old); // Submitted arrives after the replacement's handle.
                if drop_old {
                    drop(old);
                } else {
                    old_provider.fail_poll.store(true, Ordering::SeqCst);
                    assert!(matches!(block_on(old), Outcome::Failed { transient: true, .. }));
                }
                assert!(!has(&runner, "controlled", "old"), "late obsolete handle cannot leak");
                assert!(has(&runner, replacement_provider, "new"));
                {
                    let state = runner.lock();
                    assert_eq!(state.jobs[&JobId(1)].job.idempotency_key, "new");
                    assert_eq!(state.running[&JobId(1)].key, "new");
                    assert!(state.settle_after.contains(&JobId(1)) && state.forget_after.contains(&JobId(1)));
                }
                if paid {
                    assert_eq!(charge(&budget, "controlled", "old"), Some((100_000, Charge::Unresolved)));
                    assert_eq!(charge(&budget, replacement_provider, "new"), Some((100_000, Charge::Estimate)));
                    assert_eq!(spent(&budget), 200_000);
                }
                drop(new);
                assert!(!has(&runner, replacement_provider, "new"));
                assert!(runner.lock().jobs.is_empty() && runner.lock().running.is_empty());
                if paid {
                    assert_eq!(charge(&budget, replacement_provider, "new"), Some((100_000, Charge::Unresolved)));
                    assert_eq!(spent(&budget), 200_000);
                    assert_eq!(budget.log.lock().unwrap().entries().count(), 2);
                } else {
                    assert_eq!(spent(&budget), 0);
                }
            }
        }
    }
}

#[test]
fn idle_shared_key_survives_register_forget_and_requeue_until_last_reference() {
    for change_provider in [false, true] {
        let (runner, _) = setup(false);
        let shared = Arc::new(Controlled::new("shared", true));
        shared.fail_poll.store(true, Ordering::SeqCst);
        runner.register(JobId(1), queued(shared.clone(), "key"));
        runner.register(JobId(2), queued(shared.clone(), "key"));
        runner.restore_handle("shared", "key".into(), handle("accepted")).unwrap();
        let replacement = Arc::new(Controlled::new(if change_provider { "other" } else { "shared" }, true));
        runner.register(JobId(1), queued(replacement, "new"));
        assert!(has(&runner, "shared", "key"), "JobId2 still needs this global handle");
        assert!(matches!(block_on(runner.run(starting(2, "key"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(shared.submits.load(Ordering::SeqCst), 0, "shared handle resumes without submit");
        runner.register(JobId(3), queued(shared.clone(), "key"));
        runner.forget(JobId(2));
        assert!(has(&runner, "shared", "key"));
        assert!(matches!(block_on(runner.run(starting(3, "fresh"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert!(!has(&runner, "shared", "key"), "last registration requeued");
    }
}

#[test]
fn running_shared_key_retains_handle_until_old_run_drops() {
    let (runner, _) = setup(false);
    let shared = Arc::new(Controlled::new("shared", false));
    runner.register(JobId(1), queued(shared.clone(), "key"));
    runner.register(JobId(2), queued(shared.clone(), "key"));
    runner.restore_handle("shared", "key".into(), handle("accepted")).unwrap();
    runner.restore_handle("unregistered", "saved".into(), handle("unrelated")).unwrap();
    let mut running = runner.run(starting(2, "key"), CancelToken::default());
    pending(&mut running);
    runner.register(JobId(1), queued(shared.clone(), "new1"));
    runner.register(JobId(2), queued(shared, "new2"));
    assert!(has(&runner, "shared", "key"), "live other JobId still references old identity");
    drop(running);
    assert!(!has(&runner, "shared", "key"));
    assert!(has(&runner, "unregistered", "saved"), "no global sweep of restored entries");
}

#[test]
fn completing_one_job_preserves_another_jobs_same_key_resume() {
    let (runner, _) = setup(false);
    let provider = Arc::new(ResumesWith(usd(10)));
    runner.register(JobId(1), queued(provider.clone(), "shared"));
    runner.register(JobId(2), queued(provider, "shared"));
    runner.restore_handle("resumes", "shared".into(), handle("accepted")).unwrap();
    assert_eq!(block_on(runner.run(starting(1, "shared"), CancelToken::default())), Outcome::Done);
    assert!(has(&runner, "resumes", "shared"));
    // ResumesWith refuses submit, so this proves the second registration can still resume.
    assert_eq!(block_on(runner.run(starting(2, "shared"), CancelToken::default())), Outcome::Done);
    assert!(!has(&runner, "resumes", "shared"));
}

#[test]
fn requeue_and_stale_reservation_cleanup_keep_another_jobs_shared_key() {
    let (runner, budget) = setup(true);
    let provider = Arc::new(Controlled::new("shared", true));
    provider.fail_poll.store(true, Ordering::SeqCst);
    runner.register(JobId(1), queued(provider.clone(), "old"));
    assert!(matches!(block_on(runner.run(starting(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(2), queued(provider, "old"));
    assert!(matches!(block_on(runner.run(starting(1, "fresh"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert!(has(&runner, "shared", "old"), "requeue and old reservation close must preserve JobId2");
    assert_eq!(charge(&budget, "shared", "old"), Some((100_000, Charge::Unresolved)));
    assert_eq!(charge(&budget, "shared", "fresh"), Some((100_000, Charge::Estimate)));
    assert_eq!(spent(&budget), 200_000);
    runner.forget(JobId(2));
    assert!(!has(&runner, "shared", "old"));
}

#[test]
fn empty_registration_key_preserves_retry_until_scheduler_assigns_a_fresh_key() {
    for paid in [false, true] {
        let (runner, budget) = setup(paid);
        let provider = Arc::new(Controlled::new("shared", true));
        provider.fail_poll.store(true, Ordering::SeqCst);
        runner.register(JobId(1), queued(provider.clone(), "old"));
        assert!(matches!(block_on(runner.run(starting(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        runner.register(JobId(1), queued(provider.clone(), ""));
        assert!(has(&runner, "shared", "old"));
        assert_eq!(runner.lock().jobs[&JobId(1)].job.idempotency_key, "old");
        assert!(matches!(block_on(runner.run(starting(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(provider.submits.load(Ordering::SeqCst), 1, "empty placeholder matching Start resumes");
        assert!(matches!(block_on(runner.run(starting(1, "fresh"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert!(!has(&runner, "shared", "old"));
        assert!(has(&runner, "shared", "fresh"));
        assert_eq!(provider.submits.load(Ordering::SeqCst), 2);
        if paid {
            assert_eq!(charge(&budget, "shared", "old"), Some((100_000, Charge::Unresolved)));
            assert_eq!(charge(&budget, "shared", "fresh"), Some((100_000, Charge::Estimate)));
            assert_eq!(spent(&budget), 200_000);
        }
    }
}

#[test]
fn empty_placeholder_keeps_shared_old_key_but_never_carries_it_to_another_provider() {
    for changed_provider in [false, true] {
        for paid in [false, true] {
            let (runner, budget) = setup(paid);
            let old = Arc::new(Controlled::new("shared", true));
            old.fail_poll.store(true, Ordering::SeqCst);
            runner.register(JobId(1), queued(old.clone(), "old"));
            assert!(matches!(block_on(runner.run(starting(1, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
            runner.register(JobId(2), queued(old.clone(), "old"));
            let next = if changed_provider { Arc::new(Controlled::new("other", true)) } else { old.clone() };
            next.fail_poll.store(true, Ordering::SeqCst);
            runner.register(JobId(1), queued(next, ""));
            assert_eq!(runner.lock().jobs[&JobId(1)].job.idempotency_key, if changed_provider { "" } else { "old" });
            assert!(matches!(block_on(runner.run(starting(1, "fresh"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
            assert!(has(&runner, "shared", "old"));
            let submits = old.submits.load(Ordering::SeqCst);
            assert!(matches!(block_on(runner.run(starting(2, "old"), CancelToken::default())), Outcome::Failed { transient: true, .. }));
            assert_eq!(old.submits.load(Ordering::SeqCst), submits, "other JobId still resumes shared old handle");
            if paid {
                let log = budget.log.lock().unwrap();
                let spent = log.spent(&budget.session);
                assert_eq!(spent.project[&nodecraft_types::Currency::USD], usd(200_000));
                assert_eq!(spent.session[&nodecraft_types::Currency::USD], usd(200_000));
                assert_eq!(log.entries().count(), 2);
            }
        }
    }
}
