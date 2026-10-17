//! Runs provider jobs on the exec scheduler: the bridge between `exec` and `providers`, which
//! share a layer and can't depend on each other.
//!
//! [`ProviderRunner`] implements `exec::driver::JobRunner`. For each scheduled job it looks up the
//! [`ProviderJob`] and its provider, and runs it with `providers::run_to_completion`. When a run
//! fails after the provider accepted the job, the handle is kept: the scheduler's retry then
//! *resumes* that job instead of submitting it again, so a retry never pays twice.
//!
//! With a [`Budget`], every submission is held to the spending caps (#207): see
//! [`ProviderRunner::with_budget`]. The host must pass the driver's events to
//! [`ProviderRunner::observe`], which closes the budget account of a job that failed or was
//! cancelled for good.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use nodecraft_exec::budget::{Ledger, Refusal, Settlement};
use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{Event, JobId, Outcome, Start};
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, Completed, ErrorKind, JobHandle, Provider, ProviderError, ProviderJob, RunLimits, RunUpdate};
use nodecraft_types::{Money, NodeId};

/// Where a finished job's outputs and usage go (the engine turns the outputs into an
/// `AddGeneration` changeset; budgets charge the usage's reported cost).
pub type OutputSink = dyn Fn(JobId, Completed) + Send + Sync;

/// Progress reports while a job runs (queue panel, status cards).
pub type UpdateSink = dyn Fn(JobId, RunUpdate) + Send + Sync;

/// A provider job the scheduler will run, and the provider to run it on.
#[derive(Clone)]
pub struct QueuedJob {
    pub provider: Arc<dyn Provider>,
    pub job: ProviderJob,
    /// The quoted price, reserved against the budget before the job starts. `None` = unknown.
    pub estimate: Option<Money>,
    /// The canvas node it runs for, for the usage log.
    pub node: NodeId,
}

/// A job's open budget account: the reservation of one submission, until it is charged or
/// released.
#[derive(Clone, Debug)]
struct Account {
    /// The submission it covers, and the usage-log key it is charged under: the scheduler's
    /// idempotency key, with `#n` appended for the n-th fresh submission after the provider
    /// itself failed earlier ones (each of those is a separate generation, and charge).
    usage_key: String,
    provider: String,
    model: String,
    node: NodeId,
    /// The submission reached the provider, or its submit was interrupted with no answer: it
    /// may have been billed, so closing the account charges the estimate.
    maybe_billed: bool,
}

#[derive(Default)]
struct State {
    jobs: HashMap<JobId, QueuedJob>,
    /// Handles of jobs the provider accepted, by idempotency key, so retries resume them.
    handles: HashMap<String, JobHandle>,
    /// Open budget accounts, one per job at most.
    accounts: HashMap<JobId, Account>,
    /// Per idempotency key, how many of its submissions the provider itself failed.
    provider_failures: HashMap<String, u32>,
}

/// See the module docs.
pub struct ProviderRunner {
    state: Arc<Mutex<State>>,
    runtime: Arc<dyn Runtime>,
    limits: RunLimits,
    on_output: Arc<OutputSink>,
    on_update: Arc<UpdateSink>,
    budget: Option<Budget>,
}

/// The spending ledger and a wall clock (milliseconds since the Unix epoch) for the usage log.
#[derive(Clone)]
pub struct Budget {
    pub ledger: Arc<Mutex<Ledger>>,
    pub wall_clock_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Budget {
    fn with<T>(&self, f: impl FnOnce(&mut Ledger) -> T) -> T {
        f(&mut self.ledger.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Charges a job's submission (the reported `actual` cost, else the estimate) and frees its
    /// reservation. If the charge can't be recorded, the reservation stays held, so the money
    /// still counts against the caps.
    /// `unresolved`: without a reported cost, the job may or may not have been billed.
    fn charge(&self, job: JobId, account: &Account, actual: Option<Money>, unresolved: bool) {
        let charge = Settlement {
            actual,
            unresolved,
            provider: &account.provider,
            model: &account.model,
            idempotency_key: &account.usage_key,
            node: account.node,
            at_ms: (self.wall_clock_ms)(),
        };
        self.with(|ledger| {
            let _ = ledger.commit(job, charge);
        });
    }

    /// Closes an account that won't be resumed: charged as unresolved if it may have been billed,
    /// else released.
    fn close(&self, job: JobId, account: &Account) {
        if account.maybe_billed {
            self.charge(job, account, None, true);
        } else {
            self.with(|ledger| ledger.release(job));
        }
    }
}

/// The usage-log key of a submission: the scheduler's key, then `#2`, `#3`, … for fresh
/// submissions after the provider failed earlier ones.
fn usage_key(key: &str, provider_failures: u32) -> String {
    match provider_failures {
        0 => key.to_owned(),
        n => format!("{key}#{}", n.saturating_add(1)),
    }
}

impl ProviderRunner {
    pub fn new(runtime: Arc<dyn Runtime>, limits: RunLimits, on_output: Arc<OutputSink>, on_update: Arc<UpdateSink>) -> Self {
        Self { state: Arc::default(), runtime, limits, on_output, on_update, budget: None }
    }

    /// Holds every job to the budget:
    ///
    /// - A submission reserves its estimate first, and is refused (a non-transient `budget: `
    ///   error, before it reaches the provider) if that would break a cap. A job resumed from a
    ///   stored handle is adopted without a check: it already runs on the provider.
    /// - A finished job is charged its reported cost, or the estimate.
    /// - A job that certainly never reached the provider (refused, cancelled before submit, a
    ///   provider refusal such as a bad key) releases its reservation.
    /// - A job that may have been billed (accepted, or its submit was interrupted) keeps its
    ///   reservation while it can be retried or resumed, and is charged its estimate when it
    ///   ends for good ([`ProviderRunner::observe`]), when it is requeued under a new key, or
    ///   when the provider itself fails it.
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Registers the provider job behind a scheduler job; add it before scheduling the `JobId`.
    pub fn register(&self, id: JobId, job: QueuedJob) {
        self.lock().jobs.insert(id, job);
    }

    /// Restores handles saved before a restart (from `RunUpdate::Submitted`), keyed by
    /// idempotency key, so those jobs are resumed rather than submitted again.
    pub fn restore_handle(&self, idempotency_key: String, handle: JobHandle) {
        self.lock().handles.insert(idempotency_key, handle);
    }

    /// Pass every driver event here (`driver.run(runner, runtime, |e| runner.observe(&e))`): a
    /// job that failed or was cancelled for good has its budget account closed.
    pub fn observe(&self, event: &Event) {
        if let Event::Failed { job, .. } | Event::Cancelled { job } = event {
            self.settle(*job);
        }
    }

    /// Closes a job's budget account (it won't run again under its current key): a job that may
    /// have been billed is charged its estimate, otherwise its reservation is released.
    pub fn settle(&self, id: JobId) {
        let Some(budget) = &self.budget else { return };
        let account = self.lock().accounts.remove(&id);
        if let Some(account) = account {
            budget.close(id, &account);
        }
    }

    /// Drops a finished job's registration and handle (call when the scheduler forgets it), after
    /// settling its budget account.
    pub fn forget(&self, id: JobId) {
        self.settle(id);
        let mut s = self.lock();
        if let Some(q) = s.jobs.remove(&id) {
            s.handles.remove(&q.job.idempotency_key);
            s.provider_failures.remove(&q.job.idempotency_key);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// exec's runtime as a providers clock.
struct RuntimeClock(Arc<dyn Runtime>);

impl Clock for RuntimeClock {
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        self.0.sleep(ms)
    }
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
}

/// exec's cancel token as a providers cancel signal.
struct TokenSignal(CancelToken);

impl CancelSignal for TokenSignal {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.0.cancelled())
    }
}

fn outcome(error: &ProviderError) -> Outcome {
    match error.kind {
        ErrorKind::Cancelled => Outcome::Cancelled,
        _ => Outcome::Failed { error: error.to_string(), transient: error.is_transient() },
    }
}

impl JobRunner for ProviderRunner {
    fn run(&self, start: Start, cancel: CancelToken) -> TaskFuture<Outcome> {
        let state = Arc::clone(&self.state);
        let clock = RuntimeClock(Arc::clone(&self.runtime));
        let limits = self.limits;
        let on_output = Arc::clone(&self.on_output);
        let on_update = Arc::clone(&self.on_update);
        let budget = self.budget.clone();
        Box::pin(async move {
            let lock = || state.lock().unwrap_or_else(PoisonError::into_inner);
            let (queued, handle) = {
                let s = lock();
                (s.jobs.get(&start.job).cloned(), s.handles.get(&start.idempotency_key).cloned())
            };
            let Some(mut queued) = queued else {
                return Outcome::Failed { error: format!("{} has no provider job registered", start.job), transient: false };
            };
            // The scheduler's key wins: requeued jobs get a fresh one. Keep the registration in
            // step so `forget` finds the handle stored under it.
            queued.job.idempotency_key = start.idempotency_key.clone();
            if let Some(registered) = lock().jobs.get_mut(&start.job) {
                registered.job.idempotency_key = start.idempotency_key.clone();
            }
            let job_id = start.job;
            if let Some(budget) = &budget {
                if start.idempotency_key.is_empty() {
                    return Outcome::Failed { error: Refusal::MissingKey.job_error(), transient: false };
                }
                let key = usage_key(&start.idempotency_key, lock().provider_failures.get(&start.idempotency_key).copied().unwrap_or(0));
                let held = lock().accounts.get(&job_id).cloned();
                // An account for another submission (the job was requeued under a new key) is
                // closed first: its submission won't be resumed.
                if let Some(old) = held.as_ref().filter(|a| a.usage_key != key) {
                    lock().accounts.remove(&job_id);
                    budget.close(job_id, old);
                }
                if held.is_none_or(|a| a.usage_key != key) {
                    let refused = budget.with(|ledger| match handle {
                        // Resumed after a restart: already running, so its money is committed.
                        Some(_) => ledger.adopt(job_id, queued.estimate).err(),
                        // A reservation left by a charge that couldn't be recorded is taken over:
                        // it already counts against the caps.
                        None if ledger.is_reserved(job_id) => None,
                        None => ledger.reserve(job_id, queued.estimate).err(),
                    });
                    if let Some(refusal) = refused {
                        return Outcome::Failed { error: refusal.job_error(), transient: false };
                    }
                    let account = Account {
                        usage_key: key,
                        provider: queued.provider.id().to_owned(),
                        model: queued.job.model.clone(),
                        node: queued.node,
                        maybe_billed: handle.is_some(),
                    };
                    lock().accounts.insert(job_id, account);
                }
            }
            let signal = TokenSignal(cancel);
            let key = start.idempotency_key.clone();
            let record = |update: RunUpdate| {
                if let RunUpdate::Submitted(h) = &update {
                    let mut s = lock();
                    s.handles.insert(key.clone(), h.clone());
                    if let Some(account) = s.accounts.get_mut(&job_id) {
                        account.maybe_billed = true;
                    }
                }
                on_update(job_id, update);
            };
            let result = match handle {
                Some(handle) => nodecraft_providers::resume(queued.provider.as_ref(), &handle, &signal, &clock, limits, record).await,
                None => nodecraft_providers::run_to_completion(queued.provider.as_ref(), &queued.job, &signal, &clock, limits, record).await,
            };
            match result {
                Ok(done) => {
                    let account = {
                        let mut s = lock();
                        s.handles.remove(&start.idempotency_key);
                        s.provider_failures.remove(&start.idempotency_key);
                        s.jobs.remove(&job_id);
                        s.accounts.remove(&job_id)
                    };
                    // The provider's reported cost when it gives one, else the estimate.
                    if let (Some(budget), Some(account)) = (&budget, account) {
                        budget.charge(job_id, &account, done.usage.cost, false);
                    }
                    on_output(job_id, done);
                    Outcome::Done
                }
                Err(failure) => {
                    if failure.handle.is_none() || failure.finished || !failure.is_transient() {
                        // Nothing to resume: never accepted, ended by the provider, or over for good.
                        lock().handles.remove(&start.idempotency_key);
                    }
                    if failure.finished {
                        // The provider ended this generation: a retry is a new submission.
                        let mut s = lock();
                        let count = s.provider_failures.entry(start.idempotency_key.clone()).or_insert(0);
                        *count = count.saturating_add(1);
                    }
                    if let Some(budget) = &budget {
                        let closed = {
                            let mut s = lock();
                            let account = s.accounts.get_mut(&job_id);
                            match account {
                                Some(account) => {
                                    if failure.uncertain {
                                        // The submit got no answer: it may have been accepted.
                                        account.maybe_billed = true;
                                    }
                                    // Certainly not billed, or ended by the provider: close now.
                                    // Otherwise it stays open for a resume or retry.
                                    if !account.maybe_billed || failure.finished { s.accounts.remove(&job_id) } else { None }
                                }
                                None => None,
                            }
                        };
                        if let Some(account) = closed {
                            budget.close(job_id, &account);
                        }
                    }
                    outcome(&failure.error)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use nodecraft_exec::scheduler::Lane;
    use nodecraft_providers::Output;
    use nodecraft_providers::{JobProgress, Submission};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct InstantRuntime(AtomicU64);
    impl Runtime for InstantRuntime {
        fn spawn(&self, task: TaskFuture<()>) {
            block_on(task);
        }
        fn sleep(&self, ms: u64) -> TaskFuture<()> {
            self.0.fetch_add(ms, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Polls fail with a network error `fail_polls` times, then the job is done.
    struct Flaky {
        submits: AtomicU64,
        polls: AtomicU64,
        fail_polls: u64,
    }

    impl Provider for Flaky {
        fn id(&self) -> &str {
            "flaky"
        }
        fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            let id = job.idempotency_key.clone();
            Box::pin(async move { Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::Value::Null })) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            let n = self.polls.fetch_add(1, Ordering::SeqCst);
            let fail = n < self.fail_polls;
            Box::pin(async move {
                if fail {
                    Err(ProviderError::new(ErrorKind::Network, "reset"))
                } else {
                    Ok(JobProgress::Done(Completed::new(vec![Output::Text("done".into())])))
                }
            })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn start(attempt: u32) -> Start {
        Start { job: JobId(1), lane: Lane::new("flaky"), attempt, idempotency_key: "nodecraft:r:1".into() }
    }

    #[test]
    fn a_retry_resumes_the_accepted_job_instead_of_paying_twice() {
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 6 });
        type Collected = Arc<Mutex<Vec<(JobId, Vec<Output>)>>>;
        let outputs: Collected = Arc::default();
        let sink = Arc::clone(&outputs);
        let runner = ProviderRunner::new(
            Arc::new(InstantRuntime(AtomicU64::new(0))),
            RunLimits { max_poll_errors: 5, ..RunLimits::default() },
            Arc::new(move |id, out| sink.lock().unwrap().push((id, out.outputs))),
            Arc::new(|_, _| {}),
        );
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, estimate: None, node: NodeId::from_bytes([1; 16]) });
        // First attempt: submitted, then the connection keeps failing.
        let first = block_on(runner.run(start(1), CancelToken::default()));
        assert!(matches!(first, Outcome::Failed { transient: true, .. }), "{first:?}");
        // The scheduler retries: the runner resumes the same job.
        let second = block_on(runner.run(start(2), CancelToken::default()));
        assert_eq!(second, Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 1, "submitted once across both attempts");
        assert_eq!(outputs.lock().unwrap().as_slice(), &[(JobId(1), vec![Output::Text("done".into())])]);
    }

    /// Fails on the provider (transiently) the first time it is polled, then succeeds.
    struct FailsOnce {
        submits: AtomicU64,
        polls: AtomicU64,
    }

    impl Provider for FailsOnce {
        fn id(&self) -> &str {
            "fails-once"
        }
        fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            let id = job.idempotency_key.clone();
            Box::pin(async move { Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::Value::Null })) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            let first = self.polls.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                Ok(if first {
                    JobProgress::Failed(ProviderError::new(ErrorKind::Server, "OOM"))
                } else {
                    JobProgress::Done(Completed::new(vec![Output::Text("ok".into())]))
                })
            })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn a_job_the_provider_failed_is_submitted_again_on_retry() {
        let provider = Arc::new(FailsOnce { submits: AtomicU64::new(0), polls: AtomicU64::new(0) });
        let runner = ProviderRunner::new(Arc::new(InstantRuntime(AtomicU64::new(0))), RunLimits::default(), Arc::new(|_, _| {}), Arc::new(|_, _| {}));
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, estimate: None, node: NodeId::from_bytes([1; 16]) });
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 2, "a failed generation is retried as a new one");
        runner.forget(JobId(1));
    }

    #[test]
    fn unknown_jobs_and_cancellation() {
        let runner = ProviderRunner::new(Arc::new(InstantRuntime(AtomicU64::new(0))), RunLimits::default(), Arc::new(|_, _| {}), Arc::new(|_, _| {}));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, estimate: None, node: NodeId::from_bytes([1; 16]) });
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(start(1), token)), Outcome::Cancelled);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    }

    /// Finishes at submit, reporting `cost`, or fails the submit with `submit_error`.
    struct OneShot {
        submits: AtomicU64,
        submit_error: Option<ErrorKind>,
        cost: Option<Money>,
    }

    impl Provider for OneShot {
        fn id(&self) -> &str {
            "instant"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            let (error, cost) = (self.submit_error, self.cost);
            Box::pin(async move {
                match error {
                    Some(kind) => Err(ProviderError::new(kind, "submit failed")),
                    None => {
                        let mut done = Completed::new(vec![Output::Text("ok".into())]);
                        done.usage.cost = cost;
                        Ok(Submission::Finished(done))
                    }
                }
            })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "no queue")) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    use nodecraft_exec::budget::is_refusal;
    use nodecraft_exec::cost::{Budgets, Charge, UsageLog};
    use nodecraft_types::Currency;

    fn usd(micros: u64) -> Money {
        Money::new(Currency::USD, micros)
    }

    fn budgeted(budgets: Budgets, limits: RunLimits) -> (ProviderRunner, Arc<Mutex<Ledger>>) {
        let ledger = Arc::new(Mutex::new(Ledger::new(budgets, false, "s1", UsageLog::default())));
        let budget = Budget { ledger: Arc::clone(&ledger), wall_clock_ms: Arc::new(|| 1_700_000_000_000) };
        let runner = ProviderRunner::new(Arc::new(InstantRuntime(AtomicU64::new(0))), limits, Arc::new(|_, _| {}), Arc::new(|_, _| {})).with_budget(budget);
        (runner, ledger)
    }

    fn queue(runner: &ProviderRunner, provider: Arc<dyn Provider>, estimate: Option<Money>) {
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider, job, estimate, node: NodeId::from_bytes([1; 16]) });
    }

    fn charges(ledger: &Mutex<Ledger>) -> Vec<(Money, Charge)> {
        ledger.lock().unwrap().usage().entries().map(|e| (e.cost, e.status)).collect()
    }

    #[test]
    fn a_job_over_a_cap_is_refused_before_it_reaches_the_provider() {
        let (runner, ledger) = budgeted(Budgets { per_call: Some(usd(100)), ..Budgets::default() }, RunLimits::default());
        let provider = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: None, cost: None });
        queue(&runner, provider.clone(), Some(usd(101)));
        let Outcome::Failed { error, transient: false } = block_on(runner.run(start(1), CancelToken::default())) else { panic!("not refused") };
        assert!(is_refusal(&error), "{error}");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
        assert!(!ledger.lock().unwrap().is_reserved(JobId(1)));
        // An unpriced job is refused too while a cap is set.
        queue(&runner, provider.clone(), None);
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reservations_of_running_jobs_count_against_the_session_cap() {
        let (runner, ledger) = budgeted(Budgets { session: Some(usd(150)), ..Budgets::default() }, RunLimits::default());
        // Another job is running and holds 100.
        ledger.lock().unwrap().reserve(JobId(9), Some(usd(100))).unwrap();
        let provider = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: None, cost: None });
        queue(&runner, provider.clone(), Some(usd(60)));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
        // Once it settles for less, there is room.
        ledger.lock().unwrap().release(JobId(9));
        assert_eq!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Done);
        assert_eq!(charges(&ledger), vec![(usd(60), Charge::Actual)]);
    }

    #[test]
    fn a_finished_job_is_charged_its_reported_cost_once() {
        let (runner, ledger) = budgeted(Budgets::default(), RunLimits::default());
        let provider = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: None, cost: Some(usd(42)) });
        queue(&runner, provider.clone(), Some(usd(50)));
        assert_eq!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Done);
        assert_eq!(charges(&ledger), vec![(usd(42), Charge::Actual)]);
        assert!(ledger.lock().unwrap().reserved().values().all(|m| m.micros == 0));
        runner.forget(JobId(1));
        assert_eq!(charges(&ledger).len(), 1);
    }

    #[test]
    fn a_refused_submit_releases_and_an_interrupted_one_is_charged_when_given_up() {
        let (runner, ledger) = budgeted(Budgets::default(), RunLimits::default());
        // The provider said no (bad key): nothing was billed.
        let refused = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: Some(ErrorKind::Auth), cost: None });
        queue(&runner, refused, Some(usd(30)));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        assert!(!ledger.lock().unwrap().is_reserved(JobId(1)));
        assert!(charges(&ledger).is_empty());
        // The connection dropped mid-submit: it may have been accepted. The reservation stands for
        // the retry, and is charged when the scheduler gives up.
        let cut = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: Some(ErrorKind::Network), cost: None });
        queue(&runner, cut, Some(usd(30)));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert!(ledger.lock().unwrap().is_reserved(JobId(1)));
        assert!(matches!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        runner.forget(JobId(1));
        assert_eq!(charges(&ledger), vec![(usd(30), Charge::Unresolved)]);
        assert!(!ledger.lock().unwrap().is_reserved(JobId(1)));
    }

    #[test]
    fn a_job_cancelled_before_it_starts_is_not_charged() {
        let (runner, ledger) = budgeted(Budgets::default(), RunLimits::default());
        let provider = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: None, cost: None });
        queue(&runner, provider, Some(usd(30)));
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(start(1), token)), Outcome::Cancelled);
        runner.forget(JobId(1));
        assert!(charges(&ledger).is_empty());
    }

    #[test]
    fn a_resumed_job_keeps_one_reservation_and_is_charged_once() {
        let (runner, ledger) = budgeted(Budgets { session: Some(usd(100)), ..Budgets::default() }, RunLimits { max_poll_errors: 5, ..RunLimits::default() });
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 6 });
        queue(&runner, provider.clone(), Some(usd(80)));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        // The retry resumes under the reservation it holds (a second one would break the cap).
        assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
        assert_eq!(charges(&ledger), vec![(usd(80), Charge::Actual)]);
    }

    #[test]
    fn a_handle_restored_after_a_restart_is_adopted_past_the_caps() {
        let (runner, ledger) = budgeted(Budgets { per_call: Some(usd(10)), ..Budgets::default() }, RunLimits::default());
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        queue(&runner, provider.clone(), Some(usd(80)));
        runner.restore_handle("nodecraft:r:1".into(), JobHandle { remote_id: "remote".into(), data: serde_json::Value::Null });
        assert_eq!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
        assert_eq!(charges(&ledger), vec![(usd(80), Charge::Actual)]);
    }

    #[test]
    fn each_submission_the_provider_failed_is_charged_separately() {
        let (runner, ledger) = budgeted(Budgets::default(), RunLimits::default());
        let provider = Arc::new(FailsOnce { submits: AtomicU64::new(0), polls: AtomicU64::new(0) });
        queue(&runner, provider.clone(), Some(usd(25)));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 2);
        assert_eq!(charges(&ledger), vec![(usd(25), Charge::Unresolved), (usd(25), Charge::Actual)]);
        let keys: Vec<String> = ledger.lock().unwrap().usage().entries().map(|e| e.idempotency_key.clone()).collect();
        assert_eq!(keys, ["nodecraft:r:1", "nodecraft:r:1#2"]);
    }

    #[test]
    fn a_requeue_under_a_new_key_charges_the_old_submission_and_checks_the_cap_again() {
        let (runner, ledger) = budgeted(Budgets { session: Some(usd(50)), ..Budgets::default() }, RunLimits::default());
        let cut = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: Some(ErrorKind::Timeout), cost: None });
        queue(&runner, cut, Some(usd(30)));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        // Requeued: a fresh key. The lost submit is charged, and 30 more no longer fits under 50.
        let requeued = Start { idempotency_key: "nodecraft:r2:1".into(), ..start(1) };
        let Outcome::Failed { error, transient: false } = block_on(runner.run(requeued, CancelToken::default())) else { panic!("not refused") };
        assert!(is_refusal(&error), "{error}");
        assert_eq!(charges(&ledger), vec![(usd(30), Charge::Unresolved)]);
        assert!(!ledger.lock().unwrap().is_reserved(JobId(1)));
    }

    #[test]
    fn a_job_without_an_idempotency_key_is_refused() {
        let (runner, _ledger) = budgeted(Budgets::default(), RunLimits::default());
        let provider = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: None, cost: None });
        queue(&runner, provider.clone(), Some(usd(1)));
        let keyless = Start { idempotency_key: String::new(), ..start(1) };
        let Outcome::Failed { error, transient: false } = block_on(runner.run(keyless, CancelToken::default())) else { panic!("not refused") };
        assert!(is_refusal(&error), "{error}");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    }

    struct PoolRuntime(futures::executor::ThreadPool, AtomicU64);
    impl Runtime for PoolRuntime {
        fn spawn(&self, task: TaskFuture<()>) {
            self.0.spawn_ok(task);
        }
        fn sleep(&self, ms: u64) -> TaskFuture<()> {
            self.1.fetch_add(ms, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            self.1.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn a_maybe_billed_job_the_scheduler_gives_up_on_is_charged_through_observe() {
        use nodecraft_exec::scheduler::{JobSpec, Limits, RetryPolicy, Scheduler};
        let runtime = Arc::new(PoolRuntime(futures::executor::ThreadPool::new().unwrap(), AtomicU64::new(0)));
        let ledger = Arc::new(Mutex::new(Ledger::new(Budgets::default(), false, "s1", UsageLog::default())));
        let budget = Budget { ledger: Arc::clone(&ledger), wall_clock_ms: Arc::new(|| 0) };
        let runner = Arc::new(ProviderRunner::new(runtime.clone(), RunLimits::default(), Arc::new(|_, _| {}), Arc::new(|_, _| {})).with_budget(budget));
        let cut = Arc::new(OneShot { submits: AtomicU64::new(0), submit_error: Some(ErrorKind::Network), cost: None });
        queue(&runner, cut.clone(), Some(usd(30)));
        let mut scheduler = Scheduler::new(Limits::default());
        let retry = RetryPolicy { max_attempts: 2, base_delay_ms: 0, max_delay_ms: 0 };
        scheduler.add(vec![JobSpec { id: JobId(1), deps: vec![], lane: Lane::new("instant"), retry, idempotency_key: "nodecraft:r:1".into() }]).unwrap();
        let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
        drop(handle);
        let observer = Arc::clone(&runner);
        block_on(driver.run(runner.clone(), runtime, move |event| observer.observe(&event)));
        assert_eq!(cut.submits.load(Ordering::SeqCst), 2, "retried once under the same key");
        assert_eq!(charges(&ledger), vec![(usd(30), Charge::Unresolved)]);
        assert!(!ledger.lock().unwrap().is_reserved(JobId(1)));
    }
}
