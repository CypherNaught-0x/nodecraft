use super::*;

#[test]
fn paid_attempt_survives_unpriced_zero_cost_replacement() {
    let (runner, budget) = budgeted(Some(1_000_000));
    let a = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
    runner.register(JobId(1), priced(a, Some(300_000)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let mut replacement = priced(Arc::new(Reports(Some(usd(0)))), None);
    replacement.price = None;
    runner.register(JobId(1), replacement);
    assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    let log = budget.log.lock().unwrap();
    assert_eq!(log.charge("flaky", "nodecraft:r:1").map(|entry| (entry.cost.micros, entry.status)), Some((300_000, Charge::Unresolved)));
    assert_eq!(log.entries().count(), 1, "unpriced work must not acquire the old reservation");
}

fn unpriced(provider: Arc<dyn Provider>) -> QueuedJob {
    let mut queued = priced(provider, None);
    queued.price = None;
    queued
}

fn paid_failed_attempt() -> (ProviderRunner, JobBudget) {
    let (runner, budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), priced(Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 }), Some(300_000)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    (runner, budget)
}

fn assert_old_charge(budget: &JobBudget) {
    let log = budget.log.lock().unwrap();
    assert_eq!(log.charge("flaky", "nodecraft:r:1").map(|entry| (entry.cost.micros, entry.status)), Some((300_000, Charge::Unresolved)));
}

#[test]
fn no_report_and_unpriced_failures_do_not_consume_old_paid_charge() {
    for provider in [Arc::new(Reports(None)) as Arc<dyn Provider>, Arc::new(SubmitFails(ErrorKind::Auth)), Arc::new(SubmitFails(ErrorKind::Network))] {
        let (runner, budget) = paid_failed_attempt();
        runner.register(JobId(1), unpriced(provider));
        let _ = block_on(runner.run(job(1), CancelToken::default()));
        runner.settle(JobId(1));
        runner.settle(JobId(1));
        runner.forget(JobId(1));
        assert_old_charge(&budget);
        assert_eq!(budget.log.lock().unwrap().entries().count(), 1);
        assert!(runner.lock().reserved.is_empty() && runner.lock().running.is_empty());
    }
}

#[test]
fn cancellation_and_drop_of_unpriced_replacement_preserve_old_charge() {
    for terminal in 0..3 {
        let (runner, budget) = paid_failed_attempt();
        runner.register(JobId(1), unpriced(Arc::new(Never::default())));
        let cancel = CancelToken::default();
        if terminal == 0 {
            cancel.cancel();
        }
        let mut future = runner.run(job(1), cancel.clone());
        if terminal == 0 {
            assert_eq!(block_on(future), Outcome::Cancelled);
        } else {
            poll_pending(&mut future);
            if terminal == 1 {
                cancel.cancel();
                assert_eq!(block_on(future), Outcome::Cancelled);
            } else {
                drop(future);
            }
        }
        runner.settle(JobId(1));
        runner.forget(JobId(1));
        assert_old_charge(&budget);
        assert_eq!(budget.log.lock().unwrap().entries().count(), 1);
        assert!(runner.lock().reserved.is_empty() && runner.lock().running.is_empty());
    }
}

#[test]
fn same_provider_new_key_closes_old_charge_even_without_a_new_price() {
    let (runner, budget) = paid_failed_attempt();
    let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
    runner.register(JobId(1), unpriced(provider.clone()));
    let fresh = Start { idempotency_key: "replacement-key".into(), ..job(1) };
    assert_eq!(block_on(runner.run(fresh, CancelToken::default())), Outcome::Done);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert_old_charge(&budget);
    assert_eq!(budget.log.lock().unwrap().entries().count(), 1);
}

#[test]
fn matching_unpriced_retry_still_owns_its_existing_reservation_once() {
    let (runner, budget) = budgeted(Some(1_000_000));
    let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 2 });
    runner.register(JobId(1), priced(provider.clone(), Some(300_000)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(1), unpriced(provider.clone()));
    assert_eq!(block_on(runner.run(Start { attempt: 2, ..job(1) }, CancelToken::default())), Outcome::Done);
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1, "same identity resumes without a second submission");
    let log = budget.log.lock().unwrap();
    assert_eq!(log.entries().map(|entry| (entry.cost.micros, entry.status)).collect::<Vec<_>>(), [(300_000, Charge::Actual)]);
}

#[derive(Clone, Copy)]
enum Reply {
    Finished,
    Refused,
    Accepted,
}

struct Gated {
    ready: Arc<std::sync::atomic::AtomicBool>,
    reply: Reply,
}

impl Provider for Gated {
    fn id(&self) -> &str {
        "gated"
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(futures::future::poll_fn(move |_| {
            if !self.ready.load(Ordering::SeqCst) {
                return std::task::Poll::Pending;
            }
            std::task::Poll::Ready(match self.reply {
                Reply::Finished => {
                    let mut completed = Completed::new(vec![]);
                    completed.usage.cost = Some(usd(0));
                    Ok(Submission::Finished(completed))
                }
                Reply::Refused => Err(ProviderError::new(ErrorKind::Auth, "refused")),
                Reply::Accepted => Ok(Submission::Queued(JobHandle { remote_id: "old".into(), data: serde_json::Value::Null })),
            })
        }))
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(futures::future::pending())
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

fn poll_pending(future: &mut TaskFuture<Outcome>) {
    let waker = futures::task::noop_waker();
    assert!(future.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
}

#[test]
fn older_success_failure_or_drop_cannot_settle_or_unprotect_a_replacement_attempt() {
    for reply in [Reply::Finished, Reply::Refused, Reply::Accepted] {
        let (runner, budget) = budgeted(Some(1_000_000));
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        runner.register(JobId(1), priced(Arc::new(Gated { ready: ready.clone(), reply }), Some(300_000)));
        let mut old = runner.run(job(1), CancelToken::default());
        poll_pending(&mut old);
        // Submit is in flight, with no Submitted handle yet. The old charge must be retained.
        runner.register(JobId(1), priced(Arc::new(Never::default()), Some(100_000)));
        let mut replacement = runner.run(job(1), CancelToken::default());
        poll_pending(&mut replacement);
        {
            let log = budget.log.lock().unwrap();
            assert_eq!(log.charge("gated", "nodecraft:r:1").map(|entry| entry.status), Some(Charge::Unresolved));
            assert_eq!(log.charge("never", "nodecraft:r:1").map(|entry| (entry.cost.micros, entry.status)), Some((100_000, Charge::Estimate)));
        }
        ready.store(true, Ordering::SeqCst);
        if matches!(reply, Reply::Accepted) {
            poll_pending(&mut old); // Late Submitted must not mark the replacement reservation.
            assert!(!runner.lock().reserved.get(&JobId(1)).unwrap().maybe_billed);
            drop(old);
        } else {
            let outcome = block_on(old);
            assert!(matches!(outcome, Outcome::Done | Outcome::Failed { transient: false, .. }));
        }
        runner.settle(JobId(1));
        runner.forget(JobId(1));
        // Old guard did not clear the replacement's running mark: host close must wait.
        assert_eq!(budget.log.lock().unwrap().charge("never", "nodecraft:r:1").map(|entry| entry.status), Some(Charge::Estimate));
        assert!(runner.lock().running.contains_key(&JobId(1)));
        assert_eq!(runner.lock().reserved.get(&JobId(1)).unwrap().estimate, usd(100_000));
        drop(replacement);
        let log = budget.log.lock().unwrap();
        assert_eq!(log.charge("never", "nodecraft:r:1").map(|entry| (entry.cost.micros, entry.status)), Some((100_000, Charge::Unresolved)));
        let expected = if matches!(reply, Reply::Finished) { 100_000 } else { 400_000 };
        assert_eq!(log.spent(&budget.session).project.get(&nodecraft_types::Currency::USD).map(|money| money.micros), Some(expected));
        drop(log);
        assert!(runner.lock().reserved.is_empty() && runner.lock().running.is_empty());
    }
}

#[test]
fn a_new_unpriced_job_still_skips_charges_even_when_it_reports_a_cost() {
    let (runner, budget) = budgeted(Some(1));
    runner.register(JobId(1), unpriced(Arc::new(Reports(Some(usd(4_200))))));
    assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    assert_eq!(spent(&budget), 0);
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
}
