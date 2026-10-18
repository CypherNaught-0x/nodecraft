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
//!   provider provably never accepted (pre-invocation cancellation or audited whole-submit
//!   evidence) is released. Diagnostic kinds alone never supply that proof. A job
//!   that may have been billed (it got a handle, or its submit got no clear answer: a timeout, a
//!   network or server error, a garbled reply, or one saying it ran or expired;
//!   `RunFailure::uncertain`) keeps its reservation while the scheduler may retry it, and is charged as
//!   unresolved when it is forgotten or settled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use nodecraft_exec::cost::{
    Budgets, Charge, ChargeOwnership, LedgerIdentity, MAX_METADATA_SHELLS, MetadataPermit, StepQuote, UsageEntry, UsageLog, UsageMetadata, check,
};

mod accounting;
pub use accounting::{AccountingFailure, AccountingRetry, AccountingRetryError, CompletionAccounting, CompletionDelivery};
use nodecraft_types::{Money, NodeId};

use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{JobId, Outcome, Start};
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, Completed, ErrorKind, JobHandle, Provider, ProviderError, ProviderJob, RunLimits, RunUpdate};

/// Where a finished job's outputs and usage go (the engine turns the outputs into an
/// `AddGeneration` changeset; budgets charge the usage's reported cost).
pub type OutputSink = dyn Fn(JobId, CompletionDelivery) + Send + Sync;

/// Progress reports while a job runs (queue panel, status cards), with the key the attempt was
/// sent under. A host that persists `RunUpdate::Submitted` handles saves `Sent::generation`
/// with them and passes it back to [`ProviderRunner::restore_generation`] after a restart.
pub type UpdateSink = dyn Fn(JobId, Sent<'_>, RunUpdate) + Send + Sync;

/// What an attempt was sent as: its key and its generation (how many generations the provider
/// had reported failed before it, #223; the key is `sent_key(scheduler key, generation)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent<'a> {
    pub key: &'a str,
    pub generation: u32,
}

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

/// Accounting belongs to a captured provider/key, even if its JobId is re-registered.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Attempt {
    provider: String,
    /// The key sent to the provider (and charged under).
    key: String,
    /// The scheduler's key, which the registration keeps (#223).
    base: String,
}

impl Attempt {
    fn matches_job(&self, job: &QueuedJob) -> bool {
        job.provider.id() == self.provider && job.job.idempotency_key == self.base
    }
}

/// A job's reservation in the usage log.
#[derive(Clone, Debug)]
struct Reserved {
    identity: LedgerIdentity,
    invocation: u64,
    provider: String,
    key: String,
    /// The scheduler's key `key` derives from.
    base: String,
    estimate: Money,
    /// The job may have reached the provider: settle as unresolved, never release.
    maybe_billed: bool,
}

struct RetainedMetadata {
    job: JobId,
    permit: MetadataPermit,
}

#[derive(Clone)]
struct SchedulerLease {
    identity: Arc<()>,
    attempt: Attempt,
    invocation: u64,
}

// Incarnation proof belongs to the existing registration, rather than a second registry.
struct RegisteredJob {
    queued: QueuedJob,
    identity: Arc<()>,
    scheduler: Option<SchedulerLease>,
}

impl std::ops::Deref for RegisteredJob {
    type Target = QueuedJob;
    fn deref(&self) -> &Self::Target {
        &self.queued
    }
}

impl std::ops::DerefMut for RegisteredJob {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.queued
    }
}

#[derive(Default)]
struct State {
    jobs: HashMap<JobId, RegisteredJob>,
    /// Handles of jobs a provider accepted, by provider and idempotency key, so retries resume
    /// them, and a job moved to another provider never resumes the first one's handle (#228).
    handles: HashMap<(String, String), JobHandle>,
    /// Open reservations of jobs that haven't settled.
    reserved: HashMap<JobId, Reserved>,
    /// Jobs inside `run` right now, and those asked to settle meanwhile (done when `run` ends).
    running: HashMap<JobId, Attempt>,
    settle_after: std::collections::HashSet<JobId>,
    forget_after: std::collections::HashSet<JobId>,
    /// Generations reported failed, keyed by provider and scheduler base (main297).
    failures: HashMap<(String, String), u32>,
    running_serial: HashMap<JobId, u64>,
    invocations: HashMap<u64, (JobId, Attempt)>,
    next_invocation: u64,
    metadata: HashMap<Attempt, RetainedMetadata>,
}

/// Longest idempotency key sent to a provider.
pub const MAX_SENT_KEY: usize = 64;

/// The key a job's next submission is sent under: the scheduler's `base` key at first, then
/// `base#n` after the provider reported `n` generations failed, so a provider that dedupes on the
/// key starts a new generation instead of returning the failed one. Deterministic per attempt (a
/// lost reply resends the same key), ASCII, and at most [`MAX_SENT_KEY`] long: a key that
/// wouldn't fit, or isn't ASCII, keeps an ASCII prefix and a hash of the whole base key, even at
/// generation 0. Scheduler keys (`nodecraft:r…`) are short ASCII without `#`, so they go out
/// unchanged at first and never collide with another key's derived form.
pub fn sent_key(base: &str, failures: u32) -> String {
    if failures == 0 && base.len() <= MAX_SENT_KEY && base.is_ascii() {
        return base.to_owned();
    }
    let suffix = if failures == 0 { String::new() } else { format!("#{failures}") };
    let candidate = format!("{base}{suffix}");
    if candidate.len() <= MAX_SENT_KEY && candidate.is_ascii() {
        return candidate;
    }
    // FNV-1a: stable across runs and platforms, no dependency.
    let hash = base.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let tail = format!("~{hash:016x}{suffix}");
    let room = MAX_SENT_KEY.saturating_sub(tail.len());
    let prefix: String = base.chars().filter(char::is_ascii).filter(|c| !c.is_ascii_control()).take(room).collect();
    format!("{prefix}{tail}")
}

impl Reserved {
    fn matches(&self, attempt: &Attempt) -> bool {
        self.provider == attempt.provider && self.key == attempt.key
    }
}

impl State {
    /// Handles are global provider/key entries. Prune only the touched identity, after its
    /// registration/running transition, and retain retries still referenced by another JobId.
    /// Restored handles without registrations are not swept. Work scales with host job state.
    /// `attempt.key` is the key its handle is stored under (the scheduler's key, or one derived
    /// from it after reported failures, #223); the failure count goes with the last reference.
    fn prune_handle(&mut self, attempt: &Attempt) {
        let referenced = self.jobs.values().any(|job| {
            attempt.matches_job(job)
                || job.scheduler.as_ref().is_some_and(|lease| lease.attempt.provider == attempt.provider && lease.attempt.base == attempt.base)
        }) || self.running.values().any(|active| active.provider == attempt.provider && active.base == attempt.base)
            || self.invocations.values().any(|(_, active)| active.provider == attempt.provider && active.base == attempt.base);
        if !referenced {
            self.handles.remove(&(attempt.provider.clone(), attempt.key.clone()));
            self.failures.remove(&(attempt.provider.clone(), attempt.base.clone()));
        }
    }

    /// The attempt a registration's next run would send: under its derived key after failures.
    fn registered(&self, provider: &str, base: String) -> Attempt {
        let failures = self.failures.get(&(provider.to_owned(), base.clone())).copied().unwrap_or(0);
        Attempt { provider: provider.to_owned(), key: sent_key(&base, failures), base }
    }

    fn owns_scheduler(&self, id: JobId, lease: &SchedulerLease) -> bool {
        self.jobs
            .get(&id)
            .and_then(|job| job.scheduler.as_ref())
            .is_some_and(|current| Arc::ptr_eq(&current.identity, &lease.identity) && current.invocation == lease.invocation)
    }

    fn retire_scheduler(&mut self, id: JobId, lease: &SchedulerLease) {
        if self.owns_scheduler(id, lease) {
            if let Some(job) = self.jobs.get_mut(&id) {
                job.scheduler = None;
            }
            self.prune_handle(&lease.attempt);
        }
    }

    fn take_reserved(&mut self, id: JobId, attempt: &Attempt) -> Option<Reserved> {
        if self.reserved.get(&id).is_some_and(|reservation| reservation.matches(attempt)) { self.reserved.remove(&id) } else { None }
    }
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
    if log.ledger_identity() != r.identity {
        return;
    }
    let open = log.charge(&r.provider, &r.key).is_some_and(|e| e.status == Charge::Estimate);
    if !open {
        return;
    }
    // Neither can fail for an open estimate in its own currency; if one did, the estimate stays
    // charged, which is the safe side.
    let _ = if r.maybe_billed { log.settle(&r.provider, &r.key, r.estimate, Charge::Unresolved) } else { log.release(&r.provider, &r.key) };
}

/// Reserves (or adopts) a priced job's estimate. `Err` is the refusal message.
fn reserve(budget: &JobBudget, attempt: &Attempt, price: &JobPrice, resumed: bool, at_ms: u64) -> Result<Option<Reserved>, String> {
    if budget.session.is_empty() {
        return Err(format!("{REFUSAL_PREFIX}the budget has no session id, so charges can't be recorded (a host setup error)"));
    }
    let budgets = budget.budgets.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut log = lock_log(budget);
    let maybe_billed = resumed;
    let (provider, key) = (attempt.provider.as_str(), attempt.key.as_str());
    if log.ownership(provider, key).map_err(|e| format!("{REFUSAL_PREFIX}{}", e.message))? == Some(ChargeOwnership::LegacyUncertain) {
        return Err(format!("{REFUSAL_PREFIX}historical charge ownership is uncertain; inspect the usage log before retrying"));
    }
    if let Some(entry) = log.charge(provider, key) {
        // A retry of a job already reserved (or settled) under this key: nothing new to charge.
        return Ok((entry.status == Charge::Estimate).then(|| Reserved {
            identity: log.ledger_identity(),
            invocation: 0,
            provider: provider.into(),
            key: key.into(),
            base: attempt.base.clone(),
            estimate: entry.cost,
            maybe_billed,
        }));
    }
    if log.has_recorded(provider, key).map_err(|e| format!("{REFUSAL_PREFIX}{}", e.message))? {
        return Ok(None);
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
        at_ms,
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
        Ok(_) => Ok(Some(Reserved {
            identity: log.ledger_identity(),
            invocation: 0,
            provider: provider.into(),
            key: key.into(),
            base: attempt.base.clone(),
            estimate,
            maybe_billed,
        })),
        Err(e) if !resumed || matches!(e.kind, nodecraft_types::ErrorKind::Busy | nodecraft_types::ErrorKind::Conflict) => {
            Err(format!("{REFUSAL_PREFIX}{}", e.message))
        }
        Err(_) => Ok(None),
    }
}

/// See `run`: clears a job's running mark and performs settles and forgets that arrived while it
/// ran, however the run ends.
struct RunGuard {
    state: Arc<Mutex<State>>,
    budget: Option<JobBudget>,
    job: JobId,
    attempt: Attempt,
    /// Set when the run returned; a run dropped or unwound mid-flight may have reached the
    /// provider, so its reservation counts as maybe billed.
    returned: bool,
    invocation: u64,
    registration: Arc<()>,
    metadata: Option<MetadataPermit>,
    scheduler: Option<SchedulerLease>,
    retry: bool,
    published: bool,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let (settle, reserved, unstarted) = {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let owns_running = s.running_serial.get(&self.job) == Some(&self.invocation);
            s.invocations.remove(&self.invocation);
            if owns_running {
                s.running.remove(&self.job);
                s.running_serial.remove(&self.job);
            }
            if !self.returned
                && self.published
                && let Some(r) = s.reserved.get_mut(&self.job).filter(|r| r.matches(&self.attempt) && r.invocation == self.invocation)
            {
                r.maybe_billed = true;
            }
            // An older future must not consume a replacement attempt's deferred requests.
            let settle = owns_running && s.settle_after.remove(&self.job);
            let forget = owns_running && s.forget_after.remove(&self.job);
            let reserved = if settle { s.take_reserved(self.job, &self.attempt) } else { None };
            // Only a returned retryable failure keeps this captured lease for backoff. Drop/
            // unwind, success, terminal failure/cancellation or deferred settle/forget retires it;
            // retire_scheduler still checks Arc identity + invocation, so stale guards cannot
            // retire a replacement's lease.
            if (!self.returned || !self.retry || settle || forget)
                && let Some(lease) = &self.scheduler
            {
                s.retire_scheduler(self.job, lease);
            }
            if settle {
                s.metadata.retain(|_, held| held.job != self.job);
            }
            if forget
                && s.jobs.get(&self.job).is_some_and(|job| self.attempt.matches_job(job))
                && s.jobs.get(&self.job).is_some_and(|job| Arc::ptr_eq(&job.identity, &self.registration))
                && let Some(q) = s.jobs.remove(&self.job)
            {
                let old = s.registered(q.queued.provider.id(), q.queued.job.idempotency_key);
                s.prune_handle(&old);
            }
            // Also clean late Submitted handles after a re-registration, on every return/drop.
            // A different replacement's running mark and deferred requests remain untouched.
            s.prune_handle(&self.attempt);
            let unstarted = if !self.published && s.reserved.get(&self.job).is_some_and(|r| r.invocation == self.invocation) {
                s.take_reserved(self.job, &self.attempt)
            } else {
                None
            };
            (settle, reserved, unstarted)
        };
        if let (Some(budget), Some(r)) = (&self.budget, unstarted) {
            close(budget, &r);
        }
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
            if s.running.contains_key(&id) {
                s.settle_after.insert(id);
                return;
            }
            s.metadata.retain(|_, held| held.job != id);
            let lease = s.jobs.get_mut(&id).and_then(|job| job.scheduler.take());
            if let Some(lease) = lease {
                s.prune_handle(&lease.attempt);
            }
            s.reserved.remove(&id)
        };
        if let (Some(budget), Some(r)) = (&self.budget, reserved) {
            close(budget, &r);
        }
    }

    /// Registers the provider job behind a scheduler job; add it before scheduling the `JobId`.
    /// A same-provider replacement preserves the last scheduler identity until a new Start or
    /// terminal host notification retires it. A proposed key alone cannot end a scheduled retry.
    /// Other registrations and active invocations can also share its handle.
    /// A different `Provider::id` carries no old scheduler lease or inactive retry permit, so a
    /// later retry uses the new provider. Active captured invocation/output ownership remains
    /// protected; a different provider object with the same id is still the same identity here.
    /// An empty key on the same provider leaves the previous scheduler-assigned key in place;
    /// `run` supplies the authoritative key and cleans up if it changes.
    pub fn register(&self, id: JobId, mut job: QueuedJob) {
        let mut s = self.lock();
        if job.job.idempotency_key.is_empty()
            && let Some(old) = s.jobs.get(&id).filter(|old| old.provider.id() == job.provider.id())
        {
            job.job.idempotency_key.clone_from(&old.job.idempotency_key);
        }
        let incoming = s.registered(job.provider.id(), job.job.idempotency_key.clone());
        let scheduler = s.jobs.get_mut(&id).filter(|old| old.provider.id() == job.provider.id()).and_then(|old| old.scheduler.take());
        s.metadata.retain(|attempt, held| held.job != id || *attempt == incoming || scheduler.as_ref().is_some_and(|lease| lease.attempt == *attempt));
        if let Some(old) = s.jobs.insert(id, RegisteredJob { queued: job, identity: Arc::new(()), scheduler }) {
            let attempt = s.registered(old.queued.provider.id(), old.queued.job.idempotency_key);
            s.prune_handle(&attempt);
            if let Some(lease) = old.scheduler {
                s.prune_handle(&lease.attempt);
            }
        }
    }

    /// Restores a first-generation handle saved before a restart (from `RunUpdate::Submitted`),
    /// keyed by the provider that issued it and the scheduler's idempotency key, so the job is
    /// resumed rather than submitted again. See [`Self::restore_generation`].
    pub fn restore_handle(&self, provider: impl Into<String>, idempotency_key: String, handle: JobHandle) {
        self.restore_generation(provider, idempotency_key, 0, handle);
    }

    /// Restores a handle saved before a restart with the [`Sent::generation`] it was reported
    /// with: it is stored under the derived key it was sent under, and the job's failure count is
    /// brought back, so the retry resumes that generation and charges under its key.
    pub fn restore_generation(&self, provider: impl Into<String>, idempotency_key: String, generation: u32, handle: JobHandle) {
        let provider = provider.into();
        let mut s = self.lock();
        let key = sent_key(&idempotency_key, generation);
        if generation > 0 {
            let n = s.failures.entry((provider.clone(), idempotency_key)).or_insert(0);
            *n = (*n).max(generation);
        }
        s.handles.insert((provider, key), handle);
    }

    /// Drops a finished job's registration and handle (call when the scheduler forgets it).
    pub fn forget(&self, id: JobId) {
        self.settle(id);
        let mut s = self.lock();
        if s.running.contains_key(&id) {
            // Dropped when the run ends (with its reservation settled).
            s.forget_after.insert(id);
            return;
        }
        if let Some(q) = s.jobs.remove(&id) {
            let old = s.registered(q.queued.provider.id(), q.queued.job.idempotency_key);
            s.prune_handle(&old);
            if let Some(lease) = q.scheduler {
                s.prune_handle(&lease.attempt);
            }
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

fn outcome(error: &ProviderError, will_retry: bool) -> Outcome {
    match error.kind {
        ErrorKind::Cancelled => Outcome::Cancelled,
        _ => Outcome::Failed { error: error.to_string(), transient: will_retry },
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
            if start.idempotency_key.is_empty() || start.idempotency_key.len() > 64 || !start.idempotency_key.is_ascii() {
                return Outcome::Failed { error: format!("{REFUSAL_PREFIX}invalid scheduler idempotency identity"), transient: false };
            }
            let (mut queued, registration) = {
                let s = lock();
                let Some(queued) = s.jobs.get(&start.job) else {
                    return Outcome::Failed { error: format!("{} has no provider job registered", start.job), transient: false };
                };
                if queued.provider.id().len() > nodecraft_exec::cost::MAX_TEXT
                    || queued.price.as_ref().is_some_and(|p| p.model.len() > nodecraft_exec::cost::MAX_TEXT)
                {
                    return Outcome::Failed { error: format!("{REFUSAL_PREFIX}accounting identity metadata is too long"), transient: false };
                }
                (queued.queued.clone(), Arc::clone(&queued.identity))
            };
            let provider_id = queued.provider.id().to_owned();
            // The scheduler's key wins: requeued jobs get a fresh one. Keep the registration in
            // step so `forget` finds the handle stored under it.
            let (key, generation) = {
                let mut s = lock();
                if !s.jobs.get(&start.job).is_some_and(|job| Arc::ptr_eq(&job.identity, &registration)) {
                    return Outcome::Failed { error: "provider job registration changed before admission".into(), transient: false };
                }
                let old = s.jobs.get_mut(&start.job).map(|registered| std::mem::replace(&mut registered.job.idempotency_key, start.idempotency_key.clone()));
                // Requeued under a new key: the old key's handle is done with.
                if let Some(old) = old.filter(|old| *old != start.idempotency_key) {
                    let old = s.registered(&provider_id, old);
                    s.prune_handle(&old);
                }
                // After provider-reported failures, a fresh generation goes under a derived key.
                let generation = s.failures.get(&(provider_id.clone(), start.idempotency_key.clone())).copied().unwrap_or(0);
                let key = sent_key(&start.idempotency_key, generation);
                (key, generation)
            };
            // Only a handle this job's current provider issued, under the key now sent, resumes.
            let handle = lock().handles.get(&(provider_id.clone(), key.clone())).cloned();
            queued.job.idempotency_key = key.clone();
            let signal = TokenSignal(cancel);
            let job_id = start.job;
            let attempt = Attempt { provider: provider_id.clone(), key: key.clone(), base: start.idempotency_key.clone() };
            // Close a prior identity even when this replacement is unpriced or already cancelled.
            if let Some(budget) = &budget {
                let stale = {
                    let mut s = lock();
                    if s.reserved.get(&job_id).is_some_and(|r| !r.matches(&attempt)) {
                        let mut old = s.reserved.remove(&job_id);
                        if let Some(old) = &mut old {
                            // An in-flight submit may have reached its provider before Submitted.
                            old.maybe_billed |= s.running.get(&job_id).is_some_and(|running| old.matches(running));
                            s.prune_handle(&Attempt { provider: old.provider.clone(), key: old.key.clone(), base: old.base.clone() });
                        }
                        old
                    } else {
                        None
                    }
                };
                if let Some(old) = stale {
                    close(budget, &old);
                }
            }
            // A separate cancelled Start owns no active invocation's lease or metadata.
            if signal.is_cancelled() {
                let mut s = lock();
                let active = s.invocations.values().any(|(id, active)| *id == job_id && active.provider == attempt.provider && active.base == attempt.base);
                if !active {
                    if s.metadata.get(&attempt).is_some_and(|held| held.job == job_id) {
                        s.metadata.remove(&attempt);
                    }
                    let lease = s
                        .jobs
                        .get(&job_id)
                        .and_then(|job| job.scheduler.as_ref())
                        .filter(|lease| lease.attempt.provider == attempt.provider && lease.attempt.base == attempt.base)
                        .cloned();
                    if let Some(lease) = lease {
                        s.retire_scheduler(job_id, &lease);
                    }
                }
                return Outcome::Cancelled;
            }
            let invocation = {
                let mut s = lock();
                if s.invocations.values().any(|(_, active)| active.provider == attempt.provider && active.key == attempt.key) {
                    return Outcome::Failed { error: format!("{REFUSAL_PREFIX}that provider identity already has an active invocation"), transient: false };
                }
                let reuse = s.metadata.get(&attempt).is_some_and(|held| held.job == job_id);
                // An invocation may have claimed an inactive identity before transferring its
                // permit to its guard. That is one retained attempt, not two table entries.
                let transfers = s.invocations.values().filter(|(id, active)| s.metadata.get(active).is_some_and(|held| held.job == *id)).count();
                let retained = s.metadata.len().saturating_add(s.invocations.len()).saturating_sub(transfers);
                if s.invocations.len() >= MAX_METADATA_SHELLS || retained.saturating_add(usize::from(!reuse)) > MAX_METADATA_SHELLS {
                    return Outcome::Failed { error: format!("{REFUSAL_PREFIX}accounting attempt capacity is full"), transient: false };
                }
                let Some(serial) = s.next_invocation.checked_add(1) else {
                    return Outcome::Failed { error: format!("{REFUSAL_PREFIX}accounting attempt sequence exhausted"), transient: false };
                };
                let needed = s.invocations.len().saturating_sub(transfers).saturating_add(usize::from(!reuse));
                if s.invocations.try_reserve(1).is_err()
                    || s.running.try_reserve(1).is_err()
                    || s.running_serial.try_reserve(1).is_err()
                    || s.metadata.try_reserve(needed).is_err()
                {
                    return Outcome::Failed { error: format!("{REFUSAL_PREFIX}cannot allocate accounting attempt metadata"), transient: false };
                }
                s.next_invocation = serial;
                s.invocations.insert(serial, (job_id, attempt.clone()));
                serial
            };
            let mut guard = RunGuard {
                state: Arc::clone(&state),
                budget: budget.clone(),
                job: job_id,
                attempt: attempt.clone(),
                returned: false,
                invocation,
                registration: Arc::clone(&registration),
                metadata: None,
                scheduler: None,
                retry: false,
                published: false,
            };
            let outcome = async {
                let at_ms = if queued.price.is_some() { budget.as_ref().map_or(0, |b| (b.wall_clock_ms)()) } else { 0 };
                let identity = if let Some(budget) = &budget {
                    let log = lock_log(budget);
                    match log.ownership(&provider_id, &key) {
                        Ok(Some(ChargeOwnership::LegacyUncertain)) => {
                            return Outcome::Failed {
                                error: format!("{REFUSAL_PREFIX}historical charge ownership is uncertain; inspect the usage log"),
                                transient: false,
                            };
                        }
                        Err(error) => return Outcome::Failed { error: format!("{REFUSAL_PREFIX}{}", error.message), transient: false },
                        _ => {}
                    }
                    Some(log.ledger_identity())
                } else {
                    None
                };
                if let Some(budget) = &budget {
                    let old_identity = lock().reserved.get(&job_id).filter(|r| r.matches(&attempt)).map(|r| r.identity.clone());
                    if old_identity.is_some_and(|old| old != lock_log(budget).ledger_identity()) {
                        return Outcome::Failed {
                            error: format!("{REFUSAL_PREFIX}accounting reservation belongs to a replaced live ledger"),
                            transient: false,
                        };
                    }
                }
                if let Some(budget) = &budget {
                    let retained = {
                        let mut s = lock();
                        if s.metadata.get(&attempt).is_some_and(|held| held.job != job_id) {
                            return Outcome::Failed { error: format!("{REFUSAL_PREFIX}another job owns this accounting attempt"), transient: false };
                        }
                        s.metadata.remove(&attempt)
                    };
                    if let Some(held) = retained {
                        if handle.is_none() {
                            let caps = budget.budgets.lock().unwrap_or_else(PoisonError::into_inner);
                            if caps.per_call.is_some() || caps.session.is_some() || caps.project.is_some() {
                                drop(caps);
                                lock().metadata.insert(attempt.clone(), held);
                                return Outcome::Failed {
                                    error: format!("{REFUSAL_PREFIX}the captured attempt has unknown cost; capped retries require a resumable handle"),
                                    transient: false,
                                };
                            }
                        }
                        if lock_log(budget).validate_metadata(&held.permit).is_err() {
                            lock().metadata.insert(attempt.clone(), held);
                            return Outcome::Failed {
                                error: format!("{REFUSAL_PREFIX}accounting reservation no longer belongs to this live ledger"),
                                transient: false,
                            };
                        }
                        guard.metadata = Some(held.permit);
                    }
                }
                if guard.metadata.is_none()
                    && let (Some(budget), Some(price)) = (&budget, &queued.price)
                {
                    match reserve(budget, &attempt, price, handle.is_some(), at_ms) {
                        Ok(Some(r)) => {
                            let mut s = lock();
                            let maybe = s.reserved.get(&job_id).is_some_and(|old| old.matches(&attempt) && old.maybe_billed) || r.maybe_billed;
                            s.reserved.insert(job_id, Reserved { invocation, maybe_billed: maybe, ..r });
                        }
                        Ok(None) => {}
                        Err(refusal) => return Outcome::Failed { error: refusal, transient: false },
                    }
                }
                if guard.metadata.is_none()
                    && let (Some(budget), Some(price)) = (&budget, &queued.price)
                {
                    let mut log = lock_log(budget);
                    match log.has_recorded(&provider_id, &key) {
                        Ok(false) => {
                            let m = UsageMetadata {
                                at_ms,
                                session: &budget.session,
                                node: price.node,
                                provider: &provider_id,
                                model: &price.model,
                                idempotency_key: &key,
                            };
                            match log.reserve_metadata(m) {
                                Ok(permit) => {
                                    guard.metadata = Some(permit);
                                }
                                Err(error) => return Outcome::Failed { error: format!("{REFUSAL_PREFIX}{}", error.message), transient: false },
                            }
                        }
                        Ok(true) => {}
                        Err(error) => return Outcome::Failed { error: format!("{REFUSAL_PREFIX}{}", error.message), transient: false },
                    }
                }
                {
                    let mut s = lock();
                    if !s.jobs.get(&job_id).is_some_and(|job| Arc::ptr_eq(&job.identity, &registration)) {
                        return Outcome::Failed { error: format!("{REFUSAL_PREFIX}job registration changed during accounting admission"), transient: false };
                    }
                    // Only admitted replacement work retires inactive old capabilities. A refused
                    // or pre-cancelled different-key Start leaves the older retry permit/lease
                    // until an admitted Start or host settle/forget; it still counts at the cap.
                    s.metadata.retain(|old, held| held.job != job_id || *old == attempt);
                    // Accounting is fully admitted before replacing the scheduler lifetime.
                    let previous = s.jobs.get_mut(&job_id).and_then(|job| job.scheduler.take());
                    let scheduler = SchedulerLease {
                        identity: previous
                            .as_ref()
                            .filter(|old| old.attempt.provider == attempt.provider && old.attempt.base == attempt.base)
                            .map_or_else(|| Arc::new(()), |old| Arc::clone(&old.identity)),
                        attempt: attempt.clone(),
                        invocation,
                    };
                    if let Some(job) = s.jobs.get_mut(&job_id) {
                        job.scheduler = Some(scheduler.clone());
                    }
                    guard.scheduler = Some(scheduler);
                    if let Some(old) = previous {
                        s.prune_handle(&old.attempt);
                    }
                    s.running_serial.insert(job_id, invocation);
                    s.running.insert(job_id, attempt.clone());
                    s.settle_after.remove(&job_id);
                    s.forget_after.remove(&job_id);
                    guard.published = true;
                }
                let record = |update: RunUpdate| {
                    if let RunUpdate::Submitted(h) = &update {
                        let mut s = lock();
                        s.handles.insert((provider_id.clone(), key.clone()), h.clone());
                        if let Some(r) = s.reserved.get_mut(&job_id).filter(|r| r.matches(&attempt)) {
                            r.maybe_billed = true;
                        }
                    }
                    on_update(job_id, Sent { key: &key, generation }, update);
                };
                let result = match handle {
                    Some(handle) => nodecraft_providers::resume(queued.provider.as_ref(), &handle, &signal, &clock, limits, record).await,
                    None => nodecraft_providers::run_to_completion(queued.provider.as_ref(), &queued.job, &signal, &clock, limits, record).await,
                };
                match result {
                    Ok(done) => {
                        let reserved = {
                            let mut s = lock();
                            if s.jobs.get(&job_id).is_some_and(|job| attempt.matches_job(job))
                                && s.jobs.get(&job_id).is_some_and(|job| Arc::ptr_eq(&job.identity, &registration))
                                && guard.scheduler.as_ref().is_some_and(|lease| s.owns_scheduler(job_id, lease))
                            {
                                s.jobs.remove(&job_id);
                            }
                            s.take_reserved(job_id, &attempt)
                        };
                        let accounting = accounting::finish(accounting::Finish {
                            budget: budget.as_ref(),
                            price: queued.price.as_ref(),
                            reserved: reserved.as_ref(),
                            identity,
                            attempt: &attempt,
                            permit: guard.metadata.take(),
                            reported: done.usage.cost,
                        });
                        let failed = matches!(accounting, CompletionAccounting::Failed(_));
                        on_output(job_id, CompletionDelivery { completed: done, accounting });
                        if failed {
                            Outcome::Failed { error: "accounting: completed output requires accounting repair".into(), transient: false }
                        } else {
                            Outcome::Done
                        }
                    }
                    Err(failure) => {
                        if failure.handle.is_some() && (failure.finished || !failure.is_transient()) {
                            // Only an actual remote handle can invalidate that shared generation.
                            // A local no-handle refusal says nothing about another owner's handle.
                            let mut s = lock();
                            s.handles.remove(&(provider_id.clone(), key.clone()));
                            // The provider ran and failed this generation: a retry is a new one,
                            // under the next derived key. Two runs reporting the same failed
                            // generation advance the count once.
                            if failure.finished && failure.handle.is_some() {
                                let n = s.failures.entry((provider_id.clone(), start.idempotency_key.clone())).or_insert(0);
                                *n = (*n).max(generation.saturating_add(1));
                            }
                        }
                        // A submit lost in transit (or answered with a garbled or "it ran" reply) may
                        // still have been accepted and billed. Only affirmative whole-submit
                        // evidence proves nonacceptance; generic refusal kinds do not (#250).
                        let lost = failure.uncertain;
                        // Use the captured provider that executed this attempt, even if its job
                        // registration was replaced while awaiting the response.
                        let will_retry =
                            failure.error.is_transient() && (failure.handle.is_some() || !failure.uncertain || queued.provider.retry_ambiguous_submit());
                        let reserved = {
                            let mut s = lock();
                            if let Some(r) = s.reserved.get_mut(&job_id).filter(|r| r.matches(&attempt)) {
                                r.maybe_billed |= failure.handle.is_some() || lost;
                            }
                            // A retry may still come for transient failures: keep the reservation.
                            if will_retry { None } else { s.take_reserved(job_id, &attempt) }
                        };
                        if let (Some(budget), Some(r)) = (&budget, reserved) {
                            close(budget, &r);
                        }
                        guard.retry = will_retry;
                        if will_retry && let Some(permit) = guard.metadata.take() {
                            let mut s = lock();
                            if s.running_serial.get(&job_id) == Some(&invocation)
                                && guard.scheduler.as_ref().is_some_and(|lease| s.owns_scheduler(job_id, lease))
                            {
                                s.metadata.insert(attempt.clone(), RetainedMetadata { job: job_id, permit });
                            }
                        }
                        outcome(&failure.error, will_retry)
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
            Arc::new(move |id, out| {
                assert!(matches!(out.accounting, CompletionAccounting::NotBudgeted));
                sink.lock().unwrap().push((id, out.completed.outputs));
            }),
            Arc::new(|_, _, _| {}),
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
        let runner = ProviderRunner::new(
            Arc::new(InstantRuntime(AtomicU64::new(0))),
            RunLimits::default(),
            Arc::new(|_, delivery| {
                assert!(matches!(delivery.accounting, CompletionAccounting::NotBudgeted));
            }),
            Arc::new(|_, _, _| {}),
        );
        let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: String::new() };
        runner.register(JobId(1), QueuedJob { provider: provider.clone(), job, price: None });
        assert!(matches!(block_on(runner.run(start(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(block_on(runner.run(start(2), CancelToken::default())), Outcome::Done);
        assert_eq!(provider.submits.load(Ordering::SeqCst), 2, "a failed generation is retried as a new one");
        runner.forget(JobId(1));
    }

    #[test]
    fn unknown_jobs_and_cancellation() {
        let runner = ProviderRunner::new(
            Arc::new(InstantRuntime(AtomicU64::new(0))),
            RunLimits::default(),
            Arc::new(|_, delivery| {
                assert!(matches!(delivery.accounting, CompletionAccounting::NotBudgeted));
            }),
            Arc::new(|_, _, _| {}),
        );
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
            Arc::new(|_, delivery| {
                assert!(!matches!(delivery.accounting, CompletionAccounting::Failed(_)), "unexpected accounting failure: {:?}", delivery.accounting);
            }),
            Arc::new(|_, _, _| {}),
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
        let (mut runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(Arc::new(Reports(Some(usd(12_345)))), Some(40_000)));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        runner.register(JobId(2), priced(Arc::new(Reports(None)), Some(40_000)));
        assert_eq!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Done);
        // Strict accounting retains the estimate, but no longer fabricates an Actual USD
        // report when the provider actually reported EUR. The original output is still delivered.
        let deliveries = Arc::new(Mutex::new(Vec::new()));
        let saved = Arc::clone(&deliveries);
        runner.on_output = Arc::new(move |_, delivery| saved.lock().unwrap().push(delivery));
        runner.register(JobId(3), priced(Arc::new(Reports(Some(Money::new(nodecraft_types::Currency::EUR, 1)))), Some(40_000)));
        assert!(matches!(block_on(runner.run(job(3), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        let mut delivered = deliveries.lock().unwrap();
        assert_eq!(delivered.len(), 1);
        let completion = delivered.pop().unwrap();
        assert_eq!(completion.completed.outputs, vec![Output::Text("ok".into())]);
        let CompletionAccounting::Failed(mut failure) = completion.accounting else { panic!("missing accounting failure") };
        assert_eq!(failure.kind, nodecraft_types::ErrorKind::CurrencyMismatch);
        assert_eq!(failure.retry.as_mut().unwrap().retry(&budget), Err(AccountingRetryError::CurrencyMismatch));
        let log = budget.log.lock().unwrap();
        let rows: Vec<(u64, Charge)> = log.entries().map(|e| (e.cost.micros, e.status)).collect();
        assert_eq!(rows, [(12_345, Charge::Actual), (40_000, Charge::Actual), (40_000, Charge::Estimate)]);
        assert_eq!(log.entries().next().map(|e| (e.provider.as_str(), e.model.as_str(), e.at_ms)), Some(("reports", "test.img", 7)));
    }

    #[test]
    fn a_job_resumed_after_a_restart_is_charged_even_past_the_cap() {
        let (runner, budget) = budgeted(Some(100_000));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        runner.register(JobId(1), priced(provider.clone(), Some(900_000)));
        runner.restore_handle("flaky", "nodecraft:r:1".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null });
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done, "already paid for");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0, "resumed, not resubmitted");
        assert_eq!(spent(&budget), 900_000);
    }

    #[test]
    fn a_job_moved_to_another_provider_never_resumes_the_old_handle() {
        // #228: A accepts the job, then polling fails transiently: its handle is kept for a retry.
        let (runner, budget) = budgeted(Some(500_000));
        let a = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
        runner.register(JobId(1), priced(a.clone(), Some(300_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        let polls = a.polls.load(Ordering::SeqCst);
        // The same job and key, now on B (which can't resume anything): B submits afresh and is
        // held to the cap, A's possibly billed attempt having been charged.
        runner.register(JobId(1), priced(Arc::new(Reports(None)), Some(300_000)));
        let Outcome::Failed { error, transient: false } = block_on(runner.run(job(1), CancelToken::default())) else { panic!("not refused") };
        assert!(is_budget_refusal(&error), "{error}");
        assert_eq!(a.polls.load(Ordering::SeqCst), polls, "A's handle wasn't polled by B");
        assert_eq!(spent(&budget), 300_000, "A's attempt charged as unresolved");
        // With room, B runs its own job.
        *budget.budgets.lock().unwrap() = Budgets { session: Some(usd(1_000_000)), ..Budgets::default() };
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        // A handle restored for one provider is never offered to another.
        runner.register(JobId(2), priced(Arc::new(Reports(None)), Some(1)));
        runner.restore_handle("flaky", "nodecraft:r:2".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null });
        assert_eq!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Done, "submitted afresh, not resumed");
    }

    #[test]
    fn a_reported_cost_is_recorded_once_for_a_job_with_no_estimate() {
        // #228: no caps and no estimate, so nothing is reserved; the actual cost still counts.
        let (runner, budget) = budgeted(None);
        runner.register(JobId(1), priced(Arc::new(Reports(Some(usd(4_200)))), None));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        runner.register(JobId(1), priced(Arc::new(Reports(Some(usd(4_200)))), None));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done, "a rerun under the same key");
        let rows: Vec<(u64, Charge)> = budget.log.lock().unwrap().entries().map(|e| (e.cost.micros, e.status)).collect();
        assert_eq!(rows, [(4_200, Charge::Actual)]);
        // Resumed after a restart, with no estimate, reporting a cost: recorded too.
        let resumed = Arc::new(ResumesWith(usd(900)));
        runner.register(JobId(2), priced(resumed, None));
        runner.restore_handle("resumes", "nodecraft:r:2".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null });
        assert_eq!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Done);
        assert_eq!(spent(&budget), 4_200 + 900);
    }

    #[test]
    fn handles_left_by_a_provider_change_or_requeue_are_dropped_without_a_budget() {
        let runner = ProviderRunner::new(
            Arc::new(InstantRuntime(AtomicU64::new(0))),
            RunLimits::default(),
            Arc::new(|_, delivery| {
                assert!(matches!(delivery.accounting, CompletionAccounting::NotBudgeted));
            }),
            Arc::new(|_, _, _| {}),
        );
        let a = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
        runner.register(JobId(1), priced(a.clone(), None));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(runner.lock().handles.len(), 1, "kept for a retry");
        runner.register(JobId(1), priced(Arc::new(Reports(None)), None));
        assert!(runner.lock().handles.is_empty(), "another provider can't resume it");
        // A requeue under a new key drops the old key's handle too.
        runner.register(JobId(2), priced(a, None));
        assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        let requeued = Start { idempotency_key: "nodecraft:r2:2".into(), ..job(2) };
        assert!(matches!(block_on(runner.run(requeued, CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(runner.lock().handles.keys().map(|(_, k)| k.as_str()).collect::<Vec<_>>(), vec!["nodecraft:r2:2"]);
    }

    #[test]
    fn a_later_reported_cost_settles_an_unresolved_charge() {
        let (runner, budget) = budgeted(None);
        let flip = Arc::new(FailsThenReports(AtomicU64::new(0), usd(120_000)));
        runner.register(JobId(1), priced(flip.clone(), Some(300_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        runner.settle(JobId(1));
        assert_eq!(budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), vec![Charge::Unresolved]);
        // A later run under the same provider and key gets through and reports what it cost.
        runner.register(JobId(1), priced(flip, Some(300_000)));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        let rows: Vec<(u64, Charge)> = budget.log.lock().unwrap().entries().map(|e| (e.cost.micros, e.status)).collect();
        assert_eq!(rows, [(120_000, Charge::Actual)]);
    }

    /// Its first submit is lost (network); later ones finish at once, reporting its configured cost.
    struct FailsThenReports(AtomicU64, Money);

    impl Provider for FailsThenReports {
        fn id(&self) -> &str {
            "flip"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
            let cost = self.1;
            Box::pin(async move {
                if first {
                    Err(ProviderError::new(ErrorKind::Network, "reset"))
                } else {
                    let mut done = Completed::new(vec![Output::Text("ok".into())]);
                    done.usage.cost = Some(cost);
                    Ok(Submission::Finished(done))
                }
            })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "none")) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// Finishes when polled (resumed), reporting the cost it holds.
    struct ResumesWith(Money);

    impl Provider for ResumesWith {
        fn id(&self) -> &str {
            "resumes"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::Auth, "only resumes")) })
        }
        fn poll<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            let mut done = Completed::new(vec![Output::Text("ok".into())]);
            done.usage.cost = Some(self.0);
            Box::pin(async move { Ok(JobProgress::Done(done)) })
        }
        fn cancel<'a>(&'a self, _h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn legacy_submit_errors_are_charged_and_preinvocation_cancel_is_free() {
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(Arc::new(SubmitFails(ErrorKind::Auth)), Some(300_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        assert_eq!(spent(&budget), 300_000, "legacy Auth does not prove absence of acceptance");
        assert_eq!(budget.log.lock().unwrap().entries().count(), 1);
        runner.register(JobId(2), priced(Arc::new(SubmitFails(ErrorKind::Network)), Some(300_000)));
        assert!(matches!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(spent(&budget), 600_000, "a lost submit may have been accepted: held");
        runner.settle(JobId(2));
        assert_eq!(budget.log.lock().unwrap().entries().last().map(|e| e.status), Some(Charge::Unresolved));
        assert_eq!(spent(&budget), 600_000);
        // Cancelled before it started: nothing reserved.
        let provider = Arc::new(Reports(None));
        runner.register(JobId(3), priced(provider, Some(5)));
        let token = CancelToken::default();
        token.cancel();
        assert_eq!(block_on(runner.run(job(3), token)), Outcome::Cancelled);
        assert_eq!(spent(&budget), 600_000);
    }

    #[test]
    fn legacy_submit_kinds_cannot_release_reservations_without_evidence() {
        // Generic diagnostics alone cannot prove whole-submit nonacceptance, even for a
        // legacy adapter returning a refusal-looking error. All remain unresolved.
        let (runner, budget) = budgeted(Some(10_000_000));
        let mut expected = 0;
        for (n, (kind, charged)) in [
            (ErrorKind::Malformed, true),
            (ErrorKind::JobFailed, true),
            (ErrorKind::Expired, true),
            (ErrorKind::UnsupportedInput, true),
            (ErrorKind::BlockedContent, true),
            (ErrorKind::Budget, true),
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

    mod ambiguous_retry_tests;
    mod delivery_evidence_tests;
    mod fresh_key_tests;
    #[cfg(not(target_arch = "wasm32"))]
    mod native_build_tests;
    mod paid_unpriced_tests;
    mod reregister_retry_tests;
    mod stale_handle_tests;
    mod strict257_tests;
}
