//! Runs provider jobs on the exec scheduler: the bridge between `exec` and `providers`, which
//! share a layer and can't depend on each other.
//!
//! [`ProviderRunner`] implements `exec::driver::JobRunner`. For each scheduled job it looks up the
//! [`ProviderJob`] and its provider, and runs it with `providers::run_to_completion`. When a run
//! fails after the provider accepted the job, the handle is kept: the scheduler's retry then
//! *resumes* that job instead of submitting it again, so a retry never pays twice.
//!
//! With a [`JobBudget`], paid jobs are also held to the budgets while they run (#207):
//! - **Reserve before submit.** A priced job's estimate is checked against the caps, counting
//!   every charge already in the usage log (including other jobs' reservations, so jobs running
//!   in parallel can't together overshoot), and recorded as an estimate. A job that would break a
//!   cap fails with a non-transient [`REFUSAL_PREFIX`] error and never reaches the provider.
//! - **Adopt resumed jobs.** A job resumed from a stored handle was already submitted: it is
//!   recorded without a cap check, since its money is already spent.
//! - **Settle.** A finished job is charged what the provider reported, or its estimate. A job the
//!   provider certainly never billed (refused at submit: bad key, invalid input) is released. A job
//!   that may have been billed (it got a handle, or its submit got no clear answer: a timeout, a
//!   network or server error, a garbled reply, or one saying it ran or expired;
//!   `RunFailure::uncertain`) keeps its reservation while the scheduler may retry it, and is charged as
//!   unresolved when it is forgotten or settled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use nodecraft_exec::cost::{Budgets, Charge, StepQuote, UsageEntry, UsageLog, check};
use nodecraft_types::{Money, NodeId};

use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{JobId, Outcome, Start};
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, Completed, ErrorKind, JobHandle, Provider, ProviderError, ProviderJob, RunLimits, RunUpdate};

/// Where a finished job's outputs and usage go (the engine turns the outputs into an
/// `AddGeneration` changeset; budgets charge the usage's reported cost).
pub type OutputSink = dyn Fn(JobId, Completed) + Send + Sync;

/// Progress reports while a job runs (queue panel, status cards).
pub type UpdateSink = dyn Fn(JobId, RunUpdate) + Send + Sync;

/// Start of every error for a job the budgets refused; such jobs are never retried.
pub const REFUSAL_PREFIX: &str = "budget: ";

/// Whether a failed job's error is a budget refusal.
pub fn is_budget_refusal(error: &str) -> bool {
    error.starts_with(REFUSAL_PREFIX)
}

/// A provider job the scheduler will run, and the provider to run it on.
#[derive(Clone)]
pub struct QueuedJob {
    pub provider: Arc<dyn Provider>,
    pub job: ProviderJob,
    /// What the job is expected to cost, for budgets. `None` for local and free work, which isn't
    /// charged.
    pub price: Option<JobPrice>,
}

/// A paid job's quote: the node and model it charges, and one job's estimate (`None` when the
/// price isn't known, which any cap refuses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobPrice {
    pub node: NodeId,
    pub model: String,
    pub estimate: Option<Money>,
}

/// The budgets paid jobs are held to, and the log they are charged in. The host shares the log
/// with the quote panel and saves it with the project.
#[derive(Clone)]
pub struct JobBudget {
    pub log: Arc<Mutex<UsageLog>>,
    pub budgets: Arc<Mutex<Budgets>>,
    /// The session id charges are recorded under.
    pub session: String,
    /// Wall-clock milliseconds since the Unix epoch, for the log.
    pub wall_clock_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

/// A job's reservation in the usage log.
#[derive(Clone, Debug)]
struct Reserved {
    provider: String,
    key: String,
    estimate: Money,
    /// The job may have reached the provider: settle as unresolved, never release.
    maybe_billed: bool,
}

#[derive(Default)]
struct State {
    jobs: HashMap<JobId, QueuedJob>,
    /// Handles of jobs the provider accepted, by idempotency key, so retries resume them.
    handles: HashMap<String, JobHandle>,
    /// Open reservations of jobs that haven't settled.
    reserved: HashMap<JobId, Reserved>,
    /// Jobs inside `run` right now, and those asked to settle meanwhile (done when `run` ends).
    running: std::collections::HashSet<JobId>,
    settle_after: std::collections::HashSet<JobId>,
    forget_after: std::collections::HashSet<JobId>,
}

/// See the module docs.
pub struct ProviderRunner {
    state: Arc<Mutex<State>>,
    runtime: Arc<dyn Runtime>,
    limits: RunLimits,
    on_output: Arc<OutputSink>,
    on_update: Arc<UpdateSink>,
    budget: Option<JobBudget>,
}

fn lock_log(budget: &JobBudget) -> std::sync::MutexGuard<'_, UsageLog> {
    budget.log.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends a reservation: charged as unresolved if the job may have been billed, released if not.
fn close(budget: &JobBudget, r: &Reserved) {
    let mut log = lock_log(budget);
    let open = log.charge(&r.provider, &r.key).is_some_and(|e| e.status == Charge::Estimate);
    if !open {
        return;
    }
    // Neither can fail for an open estimate in its own currency; if one did, the estimate stays
    // charged, which is the safe side.
    let _ = if r.maybe_billed { log.settle(&r.provider, &r.key, r.estimate, Charge::Unresolved) } else { log.release(&r.provider, &r.key) };
}

/// Reserves (or adopts) a priced job's estimate. `Err` is the refusal message.
fn reserve(budget: &JobBudget, provider: &str, key: &str, price: &JobPrice, resumed: bool) -> Result<Option<Reserved>, String> {
    if budget.session.is_empty() {
        return Err(format!("{REFUSAL_PREFIX}the budget has no session id, so charges can't be recorded (a host setup error)"));
    }
    let budgets = budget.budgets.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut log = lock_log(budget);
    let maybe_billed = resumed;
    if let Some(entry) = log.charge(provider, key) {
        // A retry of a job already reserved (or settled) under this key: nothing new to charge.
        return Ok((entry.status == Charge::Estimate).then(|| Reserved { provider: provider.into(), key: key.into(), estimate: entry.cost, maybe_billed }));
    }
    let Some(estimate) = price.estimate else {
        // An unknown price: fine without caps (nothing to charge), refused with any.
        let capped = budgets.per_call.is_some() || budgets.session.is_some() || budgets.project.is_some();
        return if capped && !resumed {
            Err(format!("{REFUSAL_PREFIX}the price of {} isn't known, so the budget can't be checked", price.model))
        } else {
            Ok(None)
        };
    };
    if !resumed {
        let step = StepQuote { node: price.node, models: vec![price.model.clone()], per_job: Ok(Some(estimate)), jobs: 1, jobs_known: true };
        let quote = nodecraft_exec::cost::Quote { steps: vec![step], totals: [(estimate.currency, estimate)].into_iter().collect() };
        let blocks = check(&quote, &budgets, &log.spent(&budget.session)).map_err(|e| format!("{REFUSAL_PREFIX}{}", e.message))?.blocks;
        if let Some(block) = blocks.first() {
            return Err(format!("{REFUSAL_PREFIX}{}", block.message()));
        }
    }
    let entry = UsageEntry {
        at_ms: (budget.wall_clock_ms)(),
        session: budget.session.clone(),
        node: price.node,
        provider: provider.into(),
        model: price.model.clone(),
        cost: estimate,
        idempotency_key: key.into(),
        status: Charge::Estimate,
    };
    // A log that can't take the charge (it is at its ceiling) refuses new work; an adopted job
    // already ran, so it goes ahead uncharged rather than being abandoned.
    match log.record(entry) {
        Ok(_) => Ok(Some(Reserved { provider: provider.into(), key: key.into(), estimate, maybe_billed })),
        Err(e) if !resumed => Err(format!("{REFUSAL_PREFIX}{}", e.message)),
        Err(_) => Ok(None),
    }
}

/// See `run`: clears a job's running mark and performs settles and forgets that arrived while it
/// ran, however the run ends.
struct RunGuard {
    state: Arc<Mutex<State>>,
    budget: Option<JobBudget>,
    job: JobId,
    /// Set when the run returned; a run dropped or unwound mid-flight may have reached the
    /// provider, so its reservation counts as maybe billed.
    returned: bool,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let (settle, reserved) = {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            s.running.remove(&self.job);
            if !self.returned
                && let Some(r) = s.reserved.get_mut(&self.job)
            {
                r.maybe_billed = true;
            }
            let settle = s.settle_after.remove(&self.job);
            let forget = s.forget_after.remove(&self.job);
            let reserved = if settle { s.reserved.remove(&self.job) } else { None };
            if forget && let Some(q) = s.jobs.remove(&self.job) {
                s.handles.remove(&q.job.idempotency_key);
            }
            (settle, reserved)
        };
        if settle && let (Some(budget), Some(r)) = (&self.budget, reserved) {
            close(budget, &r);
        }
    }
}

impl ProviderRunner {
    pub fn new(runtime: Arc<dyn Runtime>, limits: RunLimits, on_output: Arc<OutputSink>, on_update: Arc<UpdateSink>) -> Self {
        Self { state: Arc::default(), runtime, limits, on_output, on_update, budget: None }
    }

    /// Holds paid jobs to `budget` (see the module docs).
    pub fn with_budget(mut self, budget: JobBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Ends a job's open reservation, if any: charged as unresolved when the job may have been
    /// billed, released otherwise. If the job is running, this happens when its run ends.
    ///
    /// **Host contract.** The driver never tells the runner that the scheduler gave up, so the
    /// host must:
    /// - call `settle(job)` on every terminal `Event::Failed` and `Event::Cancelled` (attempts
    ///   used up, a job cancelled in backoff, a dropped run);
    /// - call [`Self::forget`] for each job it drops with `Handle::forget_finished`;
    /// - not call either for a job in backoff that will be retried;
    /// - when reopening a project, settle any estimate left open without a stored handle as
    ///   unresolved (its job can't be resumed).
    ///
    /// No call is needed before a requeue: `run` closes the old key's reservation itself.
    pub fn settle(&self, id: JobId) {
        let reserved = {
            let mut s = self.lock();
            if s.running.contains(&id) {
                s.settle_after.insert(id);
                return;
            }
            s.reserved.remove(&id)
        };
        if let (Some(budget), Some(r)) = (&self.budget, reserved) {
            close(budget, &r);
        }
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
        self.settle(id);
        let mut s = self.lock();
        if s.running.contains(&id) {
            // Dropped when the run ends (with its reservation settled).
            s.forget_after.insert(id);
            return;
        }
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
            let signal = TokenSignal(cancel);
            let job_id = start.job;
            let key = start.idempotency_key.clone();
            let provider_id = queued.provider.id().to_owned();
            // Cancelled before anything was reserved or sent: nothing to charge.
            if signal.is_cancelled() {
                return Outcome::Cancelled;
            }
            {
                let mut s = lock();
                s.running.insert(job_id);
                // Flags left by an earlier, abandoned run don't apply to this one.
                s.settle_after.remove(&job_id);
                s.forget_after.remove(&job_id);
            }
            // Ends the running mark and does deferred settles even if this future is dropped
            // mid-await or unwinds.
            let mut guard = RunGuard { state: Arc::clone(&state), budget: budget.clone(), job: job_id, returned: false };
            let outcome = async {
                if let (Some(budget), Some(price)) = (&budget, &queued.price) {
                    // A requeued job has a fresh key: end the old attempt's reservation first.
                    let stale = lock().reserved.get(&job_id).filter(|r| r.key != key || r.provider != provider_id).cloned();
                    if let Some(old) = stale {
                        close(budget, &old);
                        let mut s = lock();
                        s.reserved.remove(&job_id);
                        if old.key != key {
                            s.handles.remove(&old.key);
                        }
                    }
                    match reserve(budget, &provider_id, &key, price, handle.is_some()) {
                        Ok(Some(r)) => {
                            let mut s = lock();
                            let maybe = s.reserved.get(&job_id).is_some_and(|old| old.maybe_billed) || r.maybe_billed;
                            s.reserved.insert(job_id, Reserved { maybe_billed: maybe, ..r });
                        }
                        Ok(None) => {}
                        Err(refusal) => return Outcome::Failed { error: refusal, transient: false },
                    }
                }
                let record = |update: RunUpdate| {
                    if let RunUpdate::Submitted(h) = &update {
                        let mut s = lock();
                        s.handles.insert(key.clone(), h.clone());
                        if let Some(r) = s.reserved.get_mut(&job_id) {
                            r.maybe_billed = true;
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
                        let reserved = {
                            let mut s = lock();
                            s.handles.remove(&start.idempotency_key);
                            s.jobs.remove(&job_id);
                            s.reserved.remove(&job_id)
                        };
                        if let (Some(budget), Some(r)) = (&budget, reserved) {
                            // What the provider reported, else the estimate; a report in another
                            // currency can't settle it, so the estimate stands.
                            let mut log = lock_log(budget);
                            let reported = done.usage.cost.filter(|c| c.currency == r.estimate.currency).unwrap_or(r.estimate);
                            if log.settle(&r.provider, &r.key, reported, Charge::Actual).is_err() {
                                let _ = log.settle(&r.provider, &r.key, r.estimate, Charge::Actual);
                            }
                        }
                        on_output(job_id, done);
                        Outcome::Done
                    }
                    Err(failure) => {
                        if failure.handle.is_none() || failure.finished || !failure.is_transient() {
                            // Nothing to resume: never accepted, ended by the provider, or over for good.
                            lock().handles.remove(&start.idempotency_key);
                        }
                        // A submit lost in transit (or answered with a garbled or "it ran" reply) may
                        // still have been accepted and billed; one cancelled before it was sent,
                        // or refused, certainly wasn't (#227).
                        let lost = failure.uncertain;
                        let reserved = {
                            let mut s = lock();
                            if let Some(r) = s.reserved.get_mut(&job_id) {
                                r.maybe_billed |= failure.handle.is_some() || lost;
                            }
                            // A retry may still come for transient failures: keep the reservation.
                            if failure.is_transient() { None } else { s.reserved.remove(&job_id) }
                        };
                        if let (Some(budget), Some(r)) = (&budget, reserved) {
                            close(budget, &r);
                        }
                        outcome(&failure.error)
                    }
                }
            }
            .await;
            guard.returned = true;
            drop(guard);
            outcome
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
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, price: None });
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
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, price: None });
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
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, price: None });
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(start(1), token)), Outcome::Cancelled);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    }

    /// Submit always fails with this kind (no handle is ever issued).
    struct SubmitFails(ErrorKind);

    impl Provider for SubmitFails {
        fn id(&self) -> &str {
            "fails"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            let kind = self.0;
            Box::pin(async move { Err(ProviderError::new(kind, "submit failed")) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "none")) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// Finishes at submit and reports `cost`.
    struct Reports(Option<Money>);

    impl Provider for Reports {
        fn id(&self) -> &str {
            "reports"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            let mut done = Completed::new(vec![Output::Text("ok".into())]);
            done.usage.cost = self.0;
            Box::pin(async move { Ok(Submission::Finished(done)) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "none")) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn usd(micros: u64) -> Money {
        Money::new(nodecraft_types::Currency::USD, micros)
    }

    fn budgeted(session_cap: Option<u64>) -> (ProviderRunner, JobBudget) {
        let budget = JobBudget {
            log: Arc::default(),
            budgets: Arc::new(Mutex::new(Budgets { session: session_cap.map(usd), ..Budgets::default() })),
            session: "now".into(),
            wall_clock_ms: Arc::new(|| 7),
        };
        let runner = ProviderRunner::new(
            Arc::new(InstantRuntime(AtomicU64::new(0))),
            RunLimits { max_poll_errors: 1, ..RunLimits::default() },
            Arc::new(|_, _| {}),
            Arc::new(|_, _| {}),
        )
        .with_budget(budget.clone());
        (runner, budget)
    }

    fn priced(provider: Arc<dyn Provider>, micros: Option<u64>) -> QueuedJob {
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        QueuedJob { provider, job, price: Some(JobPrice { node: NodeId::from_bytes([5; 16]), model: "test.img".into(), estimate: micros.map(usd) }) }
    }

    fn job(n: u64) -> Start {
        Start { job: JobId(n), lane: Lane::new("x"), attempt: 1, idempotency_key: format!("nodecraft:r:{n}") }
    }

    fn spent(b: &JobBudget) -> u64 {
        b.log.lock().unwrap().spent("now").project.get(&nodecraft_types::Currency::USD).map_or(0, |m| m.micros)
    }

    #[test]
    fn caps_refuse_jobs_before_they_reach_the_provider() {
        let (runner, budget) = budgeted(Some(100_000));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        runner.register(JobId(1), priced(provider.clone(), Some(150_000)));
        let refused = block_on(runner.run(job(1), CancelToken::default()));
        assert!(matches!(&refused, Outcome::Failed { transient: false, error } if is_budget_refusal(error) && error.contains("session budget")), "{refused:?}");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
        assert_eq!(spent(&budget), 0);
        // An unknown price can't be checked against a cap.
        runner.register(JobId(2), priced(provider.clone(), None));
        assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { error, .. } if is_budget_refusal(&error)));
        // Without caps it runs, uncharged.
        let (free, budget) = budgeted(None);
        free.register(JobId(2), priced(provider.clone(), None));
        assert_eq!(block_on(free.run(job(2), CancelToken::default())), Outcome::Done);
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
    }

    #[test]
    fn jobs_in_flight_count_against_the_cap() {
        let (runner, budget) = budgeted(Some(100_000));
        // The first job is accepted, then its polls fail: still running as far as the budget knows.
        let slow = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
        runner.register(JobId(1), priced(slow, Some(60_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(spent(&budget), 60_000, "reserved");
        let other = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        runner.register(JobId(2), priced(other.clone(), Some(60_000)));
        assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { error, .. } if is_budget_refusal(&error)));
        assert_eq!(other.submits.load(Ordering::SeqCst), 0, "together they'd overshoot");
        // The first job is given up on: it reached the provider, so it stays charged.
        runner.forget(JobId(1));
        let log = budget.log.lock().unwrap();
        assert_eq!(log.entries().next().map(|e| e.status), Some(Charge::Unresolved));
        assert_eq!(log.spent("now").session.get(&nodecraft_types::Currency::USD), Some(&usd(60_000)));
    }

    #[test]
    fn finished_jobs_are_charged_what_the_provider_reported() {
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(Arc::new(Reports(Some(usd(12_345)))), Some(40_000)));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        runner.register(JobId(2), priced(Arc::new(Reports(None)), Some(40_000)));
        assert_eq!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Done);
        // A report in another currency can't settle it: the estimate stands.
        runner.register(JobId(3), priced(Arc::new(Reports(Some(Money::new(nodecraft_types::Currency::EUR, 1)))), Some(40_000)));
        assert_eq!(block_on(runner.run(job(3), CancelToken::default())), Outcome::Done);
        let log = budget.log.lock().unwrap();
        let rows: Vec<(u64, Charge)> = log.entries().map(|e| (e.cost.micros, e.status)).collect();
        assert_eq!(rows, [(12_345, Charge::Actual), (40_000, Charge::Actual), (40_000, Charge::Actual)]);
        assert_eq!(log.entries().next().map(|e| (e.provider.as_str(), e.model.as_str(), e.at_ms)), Some(("reports", "test.img", 7)));
    }

    #[test]
    fn a_job_resumed_after_a_restart_is_charged_even_past_the_cap() {
        let (runner, budget) = budgeted(Some(100_000));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        runner.register(JobId(1), priced(provider.clone(), Some(900_000)));
        runner.restore_handle("nodecraft:r:1".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null });
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done, "already paid for");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0, "resumed, not resubmitted");
        assert_eq!(spent(&budget), 900_000);
    }

    #[test]
    fn certain_rejections_are_released_and_lost_submits_are_charged() {
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(Arc::new(SubmitFails(ErrorKind::Auth)), Some(300_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        assert_eq!(spent(&budget), 0, "a rejected key never reached a model");
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
        runner.register(JobId(2), priced(Arc::new(SubmitFails(ErrorKind::Network)), Some(300_000)));
        assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(spent(&budget), 300_000, "a lost submit may have been accepted: held");
        runner.settle(JobId(2));
        assert_eq!(budget.log.lock().unwrap().entries().last().map(|e| e.status), Some(Charge::Unresolved));
        assert_eq!(spent(&budget), 300_000);
        // Cancelled before it started: nothing reserved.
        let provider = Arc::new(Reports(None));
        runner.register(JobId(3), priced(provider, Some(5)));
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(job(3), token)), Outcome::Cancelled);
        assert_eq!(spent(&budget), 300_000);
    }

    #[test]
    fn garbled_or_it_ran_submit_replies_are_charged_and_refusals_released() {
        // #227: a reply that can't be read, or one saying the job ran (and failed or expired),
        // may follow an accepted submission: charged as unresolved. Refusals are released.
        let (runner, budget) = budgeted(Some(10_000_000));
        let mut expected = 0;
        for (n, (kind, charged)) in [
            (ErrorKind::Malformed, true),
            (ErrorKind::JobFailed, true),
            (ErrorKind::Expired, true),
            (ErrorKind::UnsupportedInput, false),
            (ErrorKind::BlockedContent, false),
            (ErrorKind::Budget, false),
        ]
        .into_iter()
        .enumerate()
        {
            let id = JobId(10 + u64::try_from(n).unwrap());
            runner.register(id, priced(Arc::new(SubmitFails(kind)), Some(100_000)));
            assert!(matches!(block_on(runner.run(job(id.0), CancelToken::default())), Outcome::Failed { transient: false, .. }), "{kind:?}");
            if charged {
                expected += 100_000;
            }
            assert_eq!(spent(&budget), expected, "{kind:?}");
        }
        assert!(budget.log.lock().unwrap().entries().all(|e| e.status == Charge::Unresolved));
    }

    #[test]
    fn a_retry_reuses_its_reservation_and_a_requeue_closes_the_old_one() {
        let (runner, budget) = budgeted(Some(1_000_000));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 2 });
        runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        // Retry under the same key: same reservation, then done.
        assert_eq!(block_on(runner.run(Start { attempt: 2, ..job(1) }, CancelToken::default())), Outcome::Done);
        assert_eq!(spent(&budget), 100_000, "charged once");
        // A job that failed for good and is requeued with a fresh key: the old one is closed.
        let lost = Arc::new(SubmitFails(ErrorKind::Timeout));
        runner.register(JobId(4), priced(lost, Some(50_000)));
        assert!(matches!(block_on(runner.run(job(4), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        let fresh = Start { idempotency_key: "nodecraft:r:4b".into(), ..job(4) };
        let _ = block_on(runner.run(fresh, CancelToken::default()));
        let log = budget.log.lock().unwrap();
        let old = log.charge("fails", "nodecraft:r:4").map(|e| e.status);
        assert_eq!(old, Some(Charge::Unresolved), "the lost submit stays charged when its key is replaced");
    }

    /// Calls `settle` and `forget` on its own job while that job is being submitted.
    struct SettlesMidRun(Mutex<Option<Arc<ProviderRunner>>>);

    impl Provider for SettlesMidRun {
        fn id(&self) -> &str {
            "mid"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            if let Some(runner) = self.0.lock().unwrap().take() {
                runner.settle(JobId(1));
                runner.forget(JobId(1));
            }
            let mut done = Completed::new(vec![]);
            done.usage.cost = Some(usd(30_000));
            Box::pin(async move { Ok(Submission::Finished(done)) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "none")) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn settling_a_running_job_waits_for_its_run() {
        let (runner, budget) = budgeted(Some(1_000_000));
        let runner = Arc::new(runner);
        let provider = Arc::new(SettlesMidRun(Mutex::new(Some(Arc::clone(&runner)))));
        runner.register(JobId(1), priced(provider, Some(40_000)));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        let log = budget.log.lock().unwrap();
        assert_eq!(log.entries().map(|e| (e.cost.micros, e.status)).collect::<Vec<_>>(), [(30_000, Charge::Actual)], "the billed job is still charged");
        drop(log);
        // The deferred forget happened: the job is gone.
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    }

    #[test]
    fn a_new_provider_for_the_same_key_closes_the_old_reservation() {
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(4), priced(Arc::new(SubmitFails(ErrorKind::Timeout)), Some(100_000)));
        assert!(matches!(block_on(runner.run(job(4), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        // Re-registered on another provider (settings changed), same key.
        runner.register(JobId(4), priced(Arc::new(Reports(None)), Some(100_000)));
        assert_eq!(block_on(runner.run(job(4), CancelToken::default())), Outcome::Done);
        let log = budget.log.lock().unwrap();
        let rows: Vec<(&str, Charge)> = log.entries().map(|e| (e.provider.as_str(), e.status)).collect();
        assert_eq!(rows, [("fails", Charge::Unresolved), ("reports", Charge::Actual)], "no estimate left open");
    }

    /// Submit is polled but never returns a handle; acceptance is unknown.
    #[derive(Default)]
    struct Never {
        invoked: AtomicU64,
        polled: AtomicU64,
    }

    impl Provider for Never {
        fn id(&self) -> &str {
            "never"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            self.invoked.fetch_add(1, Ordering::SeqCst);
            Box::pin(futures::future::poll_fn(|_| {
                self.polled.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Pending
            }))
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(futures::future::pending())
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn cancel_after_pending_submit_settles_its_reservation_as_unresolved_once() {
        let (runner, budget) = budgeted(Some(1_000_000));
        let provider = Arc::new(Never::default());
        runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
        let token = CancelToken::default();
        let mut run = runner.run(job(1), token.clone());
        let waker = futures::task::noop_waker();
        assert!(run.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        assert_eq!(provider.invoked.load(Ordering::SeqCst), 1);
        assert_eq!(provider.polled.load(Ordering::SeqCst), 1);
        assert_eq!(spent(&budget), 100_000);
        assert_eq!(budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Estimate]);
        assert!(runner.lock().handles.is_empty());
        assert!(runner.lock().reserved.contains_key(&JobId(1)));
        token.cancel();
        assert_eq!(block_on(run), Outcome::Cancelled);
        assert_eq!(spent(&budget), 100_000, "pending submit may already have been accepted");
        assert_eq!(budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved]);
        assert!(runner.lock().handles.is_empty() && runner.lock().reserved.is_empty() && runner.lock().running.is_empty());
        runner.settle(JobId(1));
        runner.settle(JobId(1));
        runner.forget(JobId(1));
        assert_eq!(budget.log.lock().unwrap().entries().count(), 1, "terminal settlement is exactly once");
        let before = Arc::new(Never::default());
        runner.register(JobId(2), priced(before.clone(), Some(100_000)));
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(job(2), token)), Outcome::Cancelled);
        assert_eq!(before.invoked.load(Ordering::SeqCst), 0);
        assert_eq!(spent(&budget), 100_000, "pre-invocation cancellation adds no reservation");
        assert_eq!(budget.log.lock().unwrap().entries().count(), 1);
    }

    #[test]
    fn a_dropped_run_can_still_be_settled() {
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(Arc::new(Never::default()), Some(100_000)));
        let mut run = runner.run(job(1), CancelToken::default());
        let waker = futures::task::noop_waker();
        assert!(run.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        drop(run);
        // The submit was in flight when the run was dropped: it may have been accepted.
        runner.settle(JobId(1));
        assert_eq!(budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved], "dropped mid-submit: kept charged");
        assert!(runner.lock().reserved.is_empty() && runner.lock().running.is_empty());
        // A later run isn't hit by flags from the dropped one.
        runner.register(JobId(1), priced(Arc::new(Reports(None)), Some(100_000)));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    }

    #[test]
    fn a_budget_without_a_session_id_refuses_paid_jobs() {
        let (runner, budget) = budgeted(Some(1_000_000));
        let runner = runner.with_budget(JobBudget { session: String::new(), ..budget });
        runner.register(JobId(1), priced(Arc::new(Reports(None)), Some(10)));
        let refused = block_on(runner.run(job(1), CancelToken::default()));
        assert!(matches!(&refused, Outcome::Failed { transient: false, error } if is_budget_refusal(error) && error.contains("no session id")), "{refused:?}");
    }
}
