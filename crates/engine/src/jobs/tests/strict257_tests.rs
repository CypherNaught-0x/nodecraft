use super::*;
use futures::channel::oneshot;
use nodecraft_exec::cost::{LegacyGroupEvidence, MAX_PENDING, MAX_USAGE_ENTRIES, UsageLogFile};
use std::sync::atomic::AtomicUsize;

struct Held {
    calls: AtomicUsize,
    receiver: Mutex<Option<oneshot::Receiver<Completed>>>,
}
impl Provider for Held {
    fn id(&self) -> &str {
        "held"
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let receiver = self.receiver.lock().unwrap().take().unwrap();
        Box::pin(async move { receiver.await.map(Submission::Finished).map_err(|_| ProviderError::new(ErrorKind::Cancelled, "test sender dropped")) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { panic!("unexpected poll") })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { panic!("unexpected cancel") })
    }
}
fn held() -> (Arc<Held>, oneshot::Sender<Completed>) {
    let (sender, receiver) = oneshot::channel();
    (Arc::new(Held { calls: AtomicUsize::new(0), receiver: Mutex::new(Some(receiver)) }), sender)
}
fn pending(run: &mut TaskFuture<Outcome>) {
    assert!(run.as_mut().poll(&mut std::task::Context::from_waker(&futures::task::noop_waker())).is_pending());
}
fn output(cost: u64) -> Completed {
    let mut done = Completed::new(vec![Output::Text("retained original completion".into())]);
    done.usage.cost = Some(usd(cost));
    done
}
fn collect(runner: &mut ProviderRunner) -> Arc<Mutex<Vec<CompletionDelivery>>> {
    let results = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&results);
    runner.on_output = Arc::new(move |_, delivery| sink.lock().unwrap().push(delivery));
    results
}
fn one_slot() -> UsageLog {
    let count = MAX_USAGE_ENTRIES + MAX_PENDING - 1;
    let n = u64::try_from(count).unwrap();
    let mut file = UsageLogFile { open_session_ownership: 2, ..UsageLogFile::default() };
    file.project.insert(nodecraft_types::Currency::USD, usd(n));
    file.sessions.insert("a".into(), [(nodecraft_types::Currency::USD, usd(1))].into());
    file.sessions.insert(String::new(), [(nodecraft_types::Currency::USD, usd(n - 1))].into());
    file.quarantined = (0..count)
        .map(|i| UsageEntry {
            project: None,
            instance: None,
            at_ms: 0,
            session: "a".into(),
            node: NodeId::from_bytes([1; 16]),
            provider: "old".into(),
            model: "m".into(),
            cost: usd(1),
            idempotency_key: format!("old-{i}"),
            status: Charge::Estimate,
        })
        .collect();
    file.legacy_groups.push(LegacyGroupEvidence {
        session: "a".into(),
        original_named: usd(1),
        original_earlier: usd(n - 1),
        remaining_earlier_after_exact_migrations: usd(n - 1),
        original_project: usd(n),
        original_open: usd(n),
    });
    UsageLog::try_from(file).unwrap()
}

#[test]
fn blocked_unknown_price_holds_last_slot_and_completed_actual_cannot_be_displaced() {
    let (mut runner, budget) = budgeted(None);
    *budget.log.lock().unwrap() = one_slot();
    let results = collect(&mut runner);
    let (a, send) = held();
    runner.register(JobId(1), priced(a.clone(), None));
    let mut run = runner.run(job(1), CancelToken::default());
    pending(&mut run);
    assert_eq!(a.calls.load(Ordering::SeqCst), 1);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    let (b, _send_b) = held();
    runner.register(JobId(2), priced(b.clone(), None));
    assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(b.calls.load(Ordering::SeqCst), 0, "capacity refuses before provider method invocation");
    assert!(
        budget
            .log
            .lock()
            .unwrap()
            .record(UsageEntry {
                project: None,
                instance: None,
                at_ms: 1,
                session: "now".into(),
                node: NodeId::from_bytes([1; 16]),
                provider: "other".into(),
                model: "m".into(),
                cost: usd(2),
                idempotency_key: "competing".into(),
                status: Charge::Actual
            })
            .is_err()
    );
    let done = output(7);
    let Output::Text(text) = &done.outputs[0] else { panic!() };
    let original = text.as_ptr();
    send.send(done).unwrap();
    assert_eq!(block_on(run), Outcome::Done);
    let mut saved = results.lock().unwrap();
    assert_eq!(saved.len(), 1);
    let delivered = saved.pop().unwrap();
    assert!(matches!(delivered.accounting, CompletionAccounting::Recorded));
    let Output::Text(text) = &delivered.completed.outputs[0] else { panic!() };
    assert_eq!(text.as_ptr(), original, "output moved, not cloned");
    assert_eq!(budget.log.lock().unwrap().charge("held", "nodecraft:r:1").unwrap().cost, usd(7));
}

#[test]
fn ledger_replacement_retains_completion_and_exact_accounting_retry_never_calls_provider() {
    let (mut runner, budget) = budgeted(None);
    let results = collect(&mut runner);
    let (provider, send) = held();
    runner.register(JobId(1), priced(provider.clone(), None));
    let mut run = runner.run(job(1), CancelToken::default());
    pending(&mut run);
    let original = std::mem::take(&mut *budget.log.lock().unwrap());
    send.send(output(9)).unwrap();
    assert!(matches!(block_on(run), Outcome::Failed { transient: false, .. }));
    let mut saved = results.lock().unwrap();
    assert_eq!(saved.len(), 1);
    let mut delivery = saved.pop().unwrap();
    assert_eq!(delivery.completed.outputs, vec![Output::Text("retained original completion".into())]);
    let CompletionAccounting::Failed(failure) = &mut delivery.accounting else { panic!("missing failure") };
    let retry = failure.retry.as_mut().unwrap();
    assert_eq!(retry.retry(&budget), Err(AccountingRetryError::Metadata(nodecraft_exec::cost::MetadataFinalizeError::LedgerReplaced)));
    *budget.log.lock().unwrap() = original;
    retry.retry(&budget).unwrap();
    assert_eq!(retry.retry(&budget), Err(AccountingRetryError::Metadata(nodecraft_exec::cost::MetadataFinalizeError::PermitConsumed)));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(saved.is_empty(), "no output redelivery");
    assert_eq!(budget.log.lock().unwrap().charge("held", "nodecraft:r:1").unwrap().cost, usd(9));
}

#[test]
fn same_key_across_runners_is_busy_and_active_drop_releases_only_its_metadata() {
    let (runner, budget) = budgeted(None);
    let (a, _send) = held();
    runner.register(JobId(1), priced(a.clone(), None));
    let mut first = runner.run(job(1), CancelToken::default());
    pending(&mut first);
    let (other, _) = budgeted(None);
    let other = other.with_budget(budget.clone());
    let (b, _send) = held();
    other.register(JobId(1), priced(b.clone(), None));
    assert!(matches!(block_on(other.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    drop(first);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert!(budget.log.lock().unwrap().charge("held", "nodecraft:r:1").is_none(), "abandonment does not fabricate zero-cost or unresolved money");
}

#[test]
fn unknown_transient_resume_retains_one_shell_then_releases_absent_report() {
    let (runner, budget) = budgeted(None);
    let p = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 2 });
    runner.register(JobId(1), priced(p.clone(), None));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    // A later quote must not retroactively fabricate an Actual for the original unknown-price
    // attempt. A real saved handle still resumes even with newly configured caps.
    runner.register(JobId(1), priced(p.clone(), Some(12345)));
    *budget.budgets.lock().unwrap() = Budgets { session: Some(usd(1)), ..Budgets::default() };
    assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    assert_eq!(p.submits.load(Ordering::SeqCst), 1, "resume, not another submission");
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert_eq!(spent(&budget), 0);
}

#[test]
fn deferred_forget_keeps_metadata_through_completion_and_drops_it_after_ack() {
    let (runner, budget) = budgeted(None);
    let (provider, send) = held();
    runner.register(JobId(1), priced(provider, None));
    let mut run = runner.run(job(1), CancelToken::default());
    pending(&mut run);
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    send.send(output(4)).unwrap();
    assert_eq!(block_on(run), Outcome::Done);
    assert!(!runner.lock().jobs.contains_key(&JobId(1)));
    assert_eq!(budget.log.lock().unwrap().charge("held", "nodecraft:r:1").unwrap().cost, usd(4));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
}

#[test]
fn total_ceiling_failure_delivers_completed_without_an_automatic_paid_retry() {
    let (mut runner, budget) = budgeted(None);
    let results = collect(&mut runner);
    let mut file = UsageLogFile { open_session_ownership: 1, ..UsageLogFile::default() };
    file.project.insert(nodecraft_types::Currency::USD, usd(nodecraft_exec::cost::MAX_TOTAL_MICROS));
    file.sessions.insert("now".into(), file.project.clone());
    *budget.log.lock().unwrap() = UsageLog::try_from(file).unwrap();
    let (provider, send) = held();
    runner.register(JobId(1), priced(provider.clone(), None));
    let mut run = runner.run(job(1), CancelToken::default());
    pending(&mut run);
    send.send(output(1)).unwrap();
    assert!(matches!(block_on(run), Outcome::Failed { transient: false, .. }));
    let mut delivery = results.lock().unwrap().pop().unwrap();
    assert_eq!(delivery.completed.outputs, vec![Output::Text("retained original completion".into())]);
    let CompletionAccounting::Failed(failure) = &mut delivery.accounting else { panic!() };
    assert_eq!(failure.retry.as_mut().unwrap().retry(&budget), Err(AccountingRetryError::Metadata(nodecraft_exec::cost::MetadataFinalizeError::TotalCeiling)));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    assert!(budget.log.lock().unwrap().charge("held", "nodecraft:r:1").is_none());
    drop(delivery);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
}

#[test]
fn historical_uncertainty_cannot_bypass_ownership_with_an_unpriced_facade_or_handle() {
    let (runner, budget) = budgeted(None);
    let mut file: UsageLogFile = serde_json::from_str(include_str!("../../../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    for entry in &mut file.entries {
        entry.provider = "held".into();
    }
    *budget.log.lock().unwrap() = UsageLog::try_from(file).unwrap();
    let before = budget.log.lock().unwrap().clone();
    let (provider, _send) = held();
    let mut queued = priced(provider.clone(), None);
    queued.price = None;
    runner.register(JobId(1), queued);
    runner.restore_handle("held", "old".into(), JobHandle { remote_id: "existing".into(), data: serde_json::Value::Null }).unwrap();
    assert!(matches!(
        block_on(runner.run(Start { idempotency_key: "old".into(), ..job(1) }, CancelToken::default())),
        Outcome::Failed { transient: false, .. }
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(*budget.log.lock().unwrap(), before);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
}

#[test]
fn same_key_reregistration_does_not_let_old_success_remove_new_registration() {
    let (runner, budget) = budgeted(None);
    let (old, send) = held();
    runner.register(JobId(1), priced(old, None));
    let mut run = runner.run(job(1), CancelToken::default());
    pending(&mut run);
    let (new, _send) = held();
    runner.register(JobId(1), priced(new.clone(), None));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(new.calls.load(Ordering::SeqCst), 0);
    send.send(output(3)).unwrap();
    assert_eq!(block_on(run), Outcome::Done);
    assert!(runner.lock().jobs.contains_key(&JobId(1)), "captured registration identity protects the replacement");
    assert_eq!(budget.log.lock().unwrap().charge("held", "nodecraft:r:1").unwrap().cost, usd(3));
}

#[test]
fn transient_metadata_refused_on_replacement_is_retained_for_original_ledger() {
    let (runner, budget) = budgeted(None);
    let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 2 });
    runner.register(JobId(1), priced(provider.clone(), None));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let original = std::mem::take(&mut *budget.log.lock().unwrap());
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert_eq!(runner.lock().metadata.len(), 1, "foreign refusal cannot abandon the captured hold");
    *budget.log.lock().unwrap() = original;
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
}

#[test]
fn invocation_serial_exhaustion_refuses_before_callback_or_metadata() {
    let (runner, budget) = budgeted(None);
    let (provider, _send) = held();
    runner.register(JobId(1), priced(provider.clone(), None));
    runner.lock().next_invocation = u64::MAX;
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert!(runner.lock().invocations.is_empty());
}

#[test]
fn initially_cancelled_retry_reclaims_only_its_inactive_metadata() {
    let (runner, budget) = budgeted(None);
    let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 2 });
    runner.register(JobId(1), priced(provider.clone(), None));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let cancel = CancelToken::default();
    cancel.cancel();
    assert_eq!(block_on(runner.run(job(1), cancel)), Outcome::Cancelled);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert!(runner.lock().metadata.is_empty());
}

struct RetryCost {
    submits: AtomicUsize,
    polls: AtomicUsize,
}
impl RetryCost {
    fn new() -> Arc<Self> {
        Arc::new(Self { submits: AtomicUsize::new(0), polls: AtomicUsize::new(0) })
    }
}
impl Provider for RetryCost {
    fn id(&self) -> &str {
        "retry-cost"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let key = job.idempotency_key.clone();
        Box::pin(async move { Ok(Submission::Queued(JobHandle { remote_id: key, data: serde_json::Value::Null })) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let fail = self.polls.fetch_add(1, Ordering::SeqCst) < 2;
        Box::pin(async move { if fail { Err(ProviderError::new(ErrorKind::Network, "authored transient")) } else { Ok(JobProgress::Done(output(7))) } })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn scheduler_fresh_key_abandons_only_obsolete_inactive_metadata() {
    let (runner, budget) = budgeted(None);
    let provider = RetryCost::new();
    runner.register(JobId(1), priced(provider.clone(), None));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    let fresh = Start { idempotency_key: "fresh-key".into(), ..job(1) };
    assert_eq!(block_on(runner.run(fresh, CancelToken::default())), Outcome::Done);
    assert_eq!(budget.log.lock().unwrap().charge("retry-cost", "fresh-key").unwrap().cost, usd(7));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0, "old inactive key must not retain its capacity");
}

#[test]
fn exact_inactive_retry_at_attempt_cap_reuses_its_original_permit() {
    let (runner, budget) = budgeted(None);
    let first = RetryCost::new();
    for n in 0..MAX_METADATA_SHELLS {
        let provider = if n == 0 { Arc::clone(&first) } else { RetryCost::new() };
        let id = u64::try_from(n + 1).unwrap();
        runner.register(JobId(id), priced(provider, None));
        let start = Start { idempotency_key: format!("cap-{n}"), ..job(id) };
        assert!(matches!(block_on(runner.run(start, CancelToken::default())), Outcome::Failed { transient: true, .. }));
    }
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, MAX_METADATA_SHELLS);
    let lease = runner.lock().jobs.get(&JobId(1)).unwrap().scheduler.clone().unwrap();
    let different = || Start { idempotency_key: "different-key".into(), ..job(1) };
    assert!(matches!(block_on(runner.run(different(), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    let cancelled = CancelToken::default();
    cancelled.cancel();
    assert_eq!(block_on(runner.run(different(), cancelled)), Outcome::Cancelled);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, MAX_METADATA_SHELLS);
    assert!(runner.lock().owns_scheduler(JobId(1), &lease));
    assert_eq!(first.submits.load(Ordering::SeqCst), 1);
    let extra = RetryCost::new();
    runner.register(JobId(10001), priced(extra.clone(), None));
    assert!(matches!(block_on(runner.run(job(10001), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(extra.submits.load(Ordering::SeqCst), 0);
    // Prove settle frees a slot WHILE full, then refill it before the original retry can finish.
    runner.settle(JobId(10000));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, MAX_METADATA_SHELLS - 1);
    assert!(matches!(block_on(runner.run(job(10001), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    assert_eq!(extra.submits.load(Ordering::SeqCst), 1);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, MAX_METADATA_SHELLS);
    let original = std::mem::take(&mut *budget.log.lock().unwrap());
    let retry = || Start { idempotency_key: "cap-0".into(), ..job(1) };
    assert!(matches!(block_on(runner.run(retry(), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    *budget.log.lock().unwrap() = original;
    assert_eq!(block_on(runner.run(retry(), CancelToken::default())), Outcome::Done, "reuse is not a new attempt slot");
    assert_eq!(first.submits.load(Ordering::SeqCst), 1, "accepted handle resumed without re-submit");
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, MAX_METADATA_SHELLS - 1);
    assert_eq!(budget.log.lock().unwrap().charge("retry-cost", "cap-0").unwrap().cost, usd(7));
}

struct OverlapFresh {
    calls: AtomicUsize,
    first: Mutex<Option<oneshot::Receiver<Completed>>>,
}
impl Provider for OverlapFresh {
    fn id(&self) -> &str {
        "overlap-fresh"
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let receiver = self.first.lock().unwrap().take().unwrap();
            Box::pin(async move { Ok(Submission::Finished(receiver.await.unwrap())) })
        } else {
            Box::pin(async { Ok(Submission::Finished(output(5))) })
        }
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { panic!("unexpected poll") })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn fresh_scheduler_key_preserves_older_active_guard_and_unrelated_inactive_owner() {
    let (runner, budget) = budgeted(None);
    let unrelated = RetryCost::new();
    runner.register(JobId(2), priced(unrelated, None));
    assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let (sender, receiver) = oneshot::channel();
    let provider = Arc::new(OverlapFresh { calls: AtomicUsize::new(0), first: Mutex::new(Some(receiver)) });
    runner.register(JobId(1), priced(provider, None));
    let mut old = runner.run(job(1), CancelToken::default());
    pending(&mut old);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 2);
    assert_eq!(block_on(runner.run(Start { idempotency_key: "overlap-new".into(), ..job(1) }, CancelToken::default())), Outcome::Done);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 2, "older active and unrelated inactive capabilities survive");
    sender.send(output(7)).unwrap();
    assert_eq!(block_on(old), Outcome::Done);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    assert_eq!(budget.log.lock().unwrap().charge("overlap-fresh", "nodecraft:r:1").unwrap().cost, usd(7));
    assert_eq!(budget.log.lock().unwrap().charge("overlap-fresh", "overlap-new").unwrap().cost, usd(5));
}

#[test]
fn repeated_fresh_transient_keys_keep_only_one_inactive_attempt() {
    let (runner, budget) = budgeted(None);
    let provider = RetryCost::new();
    runner.register(JobId(1), priced(provider.clone(), None));
    for n in 0..8 {
        provider.polls.store(0, Ordering::SeqCst);
        assert!(matches!(
            block_on(runner.run(Start { idempotency_key: format!("transient-{n}"), ..job(1) }, CancelToken::default())),
            Outcome::Failed { transient: true, .. }
        ));
        assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 1);
    }
}

#[test]
fn delivery_evidence_failure_paths_reclaim_unknown_price_metadata_without_fake_money() {
    use nodecraft_providers::llm::{Dialect, Endpoint, LlmClient};
    struct NoHttp(AtomicUsize);
    impl nodecraft_providers::Http for NoHttp {
        fn send(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "public fixture")) })
        }
    }
    let (mut runner, budget) = budgeted(None);
    let delivered = collect(&mut runner);
    let http = Arc::new(NoHttp(AtomicUsize::new(0)));
    let client = LlmClient::new(
        Endpoint { id: "local-proof".into(), dialect: Dialect::OpenAi, base_url: "http://public-fixture.invalid".into(), key: None },
        http.clone(),
    );
    let mut local = priced(Arc::new(client), None);
    local.job.model.clear();
    runner.register(JobId(1), local);
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(http.0.load(Ordering::SeqCst), 0);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
    // Legacy Auth is delivery-unknown, not proven free. With no price/report there is no
    // currency to invent; only the transient metadata capacity can be reclaimed.
    runner.register(JobId(2), priced(Arc::new(SubmitFails(ErrorKind::Auth)), None));
    assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
    assert!(budget.log.lock().unwrap().reported_spent("now").project.is_empty());
    assert!(delivered.lock().unwrap().is_empty(), "failed provider invocation has no completed output to redeliver");
}

struct TerminalNoReplay(AtomicUsize);
impl Provider for TerminalNoReplay {
    fn id(&self) -> &str {
        "terminal-no-replay"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "public terminal lost reply")) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { panic!("no accepted handle") })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
#[test]
fn captured_terminal_no_replay_policy_reclaims_unknown_price_metadata_hold() {
    let (mut runner, budget) = budgeted(None);
    let outputs = collect(&mut runner);
    let provider = Arc::new(TerminalNoReplay(AtomicUsize::new(0)));
    runner.register(JobId(1), priced(provider.clone(), None));
    assert!(
        matches!(block_on(runner.run(job(1),CancelToken::default())),Outcome::Failed{transient:false,error}if error.contains("public terminal lost reply"))
    );
    assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    assert!(outputs.lock().unwrap().is_empty());
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert!(runner.state.lock().unwrap().metadata.is_empty());
    assert!(budget.log.lock().unwrap().entries().next().is_none());
}
