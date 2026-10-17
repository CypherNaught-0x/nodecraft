//! Runs provider jobs on the exec scheduler: the bridge between `exec` and `providers`, which
//! share a layer and can't depend on each other.
//!
//! [`ProviderRunner`] implements `exec::driver::JobRunner`. For each scheduled job it looks up the
//! [`ProviderJob`] and its provider, and runs it with `providers::run_to_completion`. When a run
//! fails after the provider accepted the job, the handle is kept: the scheduler's retry then
//! *resumes* that job instead of submitting it again, so a retry never pays twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use nodecraft_exec::budget::Ledger;
use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{JobId, Outcome, Start};
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, ErrorKind, JobHandle, Output, Provider, ProviderError, ProviderJob, RunLimits, RunUpdate};
use nodecraft_types::Money;

/// Where a finished job's outputs go (the engine turns them into an `AddGeneration` changeset).
pub type OutputSink = dyn Fn(JobId, Vec<Output>) + Send + Sync;

/// Progress reports while a job runs (queue panel, status cards).
pub type UpdateSink = dyn Fn(JobId, RunUpdate) + Send + Sync;

/// A provider job the scheduler will run, and the provider to run it on.
#[derive(Clone)]
pub struct QueuedJob {
    pub provider: Arc<dyn Provider>,
    pub job: ProviderJob,
    /// The quoted price, reserved against the budget before the job starts. `None` = unknown.
    pub estimate: Option<Money>,
}

#[derive(Default)]
struct State {
    jobs: HashMap<JobId, QueuedJob>,
    /// Handles of jobs the provider accepted, by idempotency key, so retries resume them.
    handles: HashMap<String, JobHandle>,
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

impl ProviderRunner {
    pub fn new(runtime: Arc<dyn Runtime>, limits: RunLimits, on_output: Arc<OutputSink>, on_update: Arc<UpdateSink>) -> Self {
        Self { state: Arc::default(), runtime, limits, on_output, on_update, budget: None }
    }

    /// Holds every job to the budget: each reserves its estimate before it is submitted (and is
    /// refused, without reaching the provider, if that would break a cap), and is charged when it
    /// ends.
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

    /// Drops a finished job's registration and handle (call when the scheduler forgets it).
    pub fn forget(&self, id: JobId) {
        let mut s = self.lock();
        if let Some(q) = s.jobs.remove(&id) {
            s.handles.remove(&q.job.idempotency_key);
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
            // A new submission reserves its estimate first; a resumed one already holds it.
            let ledger = |f: &mut dyn FnMut(&mut Ledger)| {
                if let Some(b) = &budget {
                    f(&mut b.ledger.lock().unwrap_or_else(PoisonError::into_inner));
                }
            };
            if handle.is_none()
                && let Some(b) = &budget
            {
                let reserved = b.ledger.lock().unwrap_or_else(PoisonError::into_inner).reserve(job_id, queued.estimate);
                if let Err(e) = reserved {
                    return Outcome::Failed { error: format!("not started: {e}"), transient: false };
                }
            }
            let provider_id = queued.provider.id().to_owned();
            let model = queued.job.model.clone();
            let now = || budget.as_ref().map_or(0, |b| (b.wall_clock_ms)());
            let signal = TokenSignal(cancel);
            let key = start.idempotency_key.clone();
            let record = |update: RunUpdate| {
                if let RunUpdate::Submitted(h) = &update {
                    lock().handles.insert(key.clone(), h.clone());
                }
                on_update(job_id, update);
            };
            let result = match handle {
                Some(handle) => nodecraft_providers::resume(queued.provider.as_ref(), &handle, &signal, &clock, limits, record).await,
                None => nodecraft_providers::run_to_completion(queued.provider.as_ref(), &queued.job, &signal, &clock, limits, record).await,
            };
            match result {
                Ok(outputs) => {
                    {
                        let mut s = lock();
                        s.handles.remove(&start.idempotency_key);
                        s.jobs.remove(&job_id);
                    }
                    // Providers don't report actual cost yet (#195): the estimate is charged.
                    ledger(&mut |l| {
                        let _ = l.commit(job_id, None, &provider_id, &model, now());
                    });
                    on_output(job_id, outputs);
                    Outcome::Done
                }
                Err(failure) => {
                    let over = failure.handle.is_none() || failure.finished || !failure.is_transient();
                    if over {
                        // Nothing to resume: never accepted, ended by the provider, or over for good.
                        lock().handles.remove(&start.idempotency_key);
                    }
                    match (&failure.handle, over) {
                        // Never reached the provider: nothing was billed.
                        (None, _) => ledger(&mut |l| {
                            l.release(job_id);
                        }),
                        // Reached it and over: it may have been billed, so the estimate is charged.
                        (Some(_), true) => ledger(&mut |l| {
                            let _ = l.commit(job_id, None, &provider_id, &model, now());
                        }),
                        // Will be resumed: the reservation stays.
                        (Some(_), false) => {}
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
                if fail { Err(ProviderError::new(ErrorKind::Network, "reset")) } else { Ok(JobProgress::Done(vec![Output::Text("done".into())])) }
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
            Arc::new(move |id, out| sink.lock().unwrap().push((id, out))),
            Arc::new(|_, _| {}),
        );
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, estimate: None });
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
                Ok(if first { JobProgress::Failed(ProviderError::new(ErrorKind::Server, "OOM")) } else { JobProgress::Done(vec![Output::Text("ok".into())]) })
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
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, estimate: None });
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 2, "a failed generation is retried as a new one");
        runner.forget(JobId(1));
    }

    #[test]
    fn budgets_gate_and_charge_jobs() {
        use nodecraft_exec::budget::BudgetLimits;
        use nodecraft_types::Currency;
        let usd = |m| Money::new(Currency::USD, m);
        let ledger =
            Arc::new(Mutex::new(Ledger::new(Currency::USD, BudgetLimits { per_session: Some(usd(1_000_000)), ..BudgetLimits::default() }, usd(0)).unwrap()));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        let runner = ProviderRunner::new(Arc::new(InstantRuntime(AtomicU64::new(0))), RunLimits::default(), Arc::new(|_, _| {}), Arc::new(|_, _| {}))
            .with_budget(Budget { ledger: Arc::clone(&ledger), wall_clock_ms: Arc::new(|| 42) });
        let job = ProviderJob { model: "flux".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job: job.clone(), estimate: Some(usd(600_000)) });
        assert_eq!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Done);
        // A second 0.6 job would bring the session to 1.2, over the 1.0 cap: refused before submit.
        runner.register(JobId(2), QueuedJob { provider: provider.clone(), job, estimate: Some(usd(600_000)) });
        let second = Start { job: JobId(2), lane: Lane::new("flaky"), attempt: 1, idempotency_key: "nodecraft:r:2".into() };
        let refused = block_on(runner.run(second, CancelToken::default()));
        assert!(matches!(&refused, Outcome::Failed { transient: false, error } if error.contains("over its cap")), "{refused:?}");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 1, "the refused job never reached the provider");
        let l = ledger.lock().unwrap();
        assert_eq!(l.session_spent(), usd(600_000));
        assert_eq!(l.log().len(), 1);
        assert_eq!((l.log()[0].provider.as_str(), l.log()[0].model.as_str(), l.log()[0].at_ms), ("flaky", "flux", 42));
    }

    #[test]
    fn unknown_jobs_and_cancellation() {
        let runner = ProviderRunner::new(Arc::new(InstantRuntime(AtomicU64::new(0))), RunLimits::default(), Arc::new(|_, _| {}), Arc::new(|_, _| {}));
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, estimate: None });
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(start(1), token)), Outcome::Cancelled);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    }
}
