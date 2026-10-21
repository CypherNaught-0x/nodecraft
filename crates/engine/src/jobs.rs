//! Runs provider jobs on the exec scheduler: the bridge between `exec` and `providers`, which
//! share a layer and can't depend on each other.
//!
//! [`ProviderRunner`] implements `exec::driver::JobRunner`. For each scheduled job it uses the
//! [`ProviderJob`] and its captured typed endpoint binding, and runs it with `providers::run_to_completion`. When a run
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
mod restore;
pub use accounting::{AccountingFailure, AccountingRetry, AccountingRetryError, CompletionAccounting, CompletionDelivery};
use nodecraft_types::{Money, NodeId};
pub use restore::RestoreHandleError;

use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{JobId, Outcome, Start};
#[cfg(test)]
use nodecraft_providers::Provider;
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, Completed, ErrorKind, JobHandle, ProviderError, ProviderJob, RunLimits, RunUpdate};

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
    /// The scheduler's key `key` derives from, which a restored handle is keyed by.
    pub base: &'a str,
    pub target: &'a nodecraft_types::ProviderTarget,
    pub generation: u32,
    /// The scheduler's attempt, 1 for the first: what a streamed piece of text belongs to.
    pub attempt: u32,
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
    pub provider: crate::provider_routing::RegisteredProvider,
    pub job: ProviderJob,
    /// What the job is expected to cost, for budgets. `None` for local and free work, which isn't
    /// charged. For a job from an authorized run, budgets use the run's own bound price instead
    /// (see [`Self::charged_price`]), so changing this changes nothing.
    pub price: Option<JobPrice>,
    /// The authorized run's admitted price (`Some(None)` for free work), set only by
    /// [`crate::run_cmds::AuthorizedRun::queued_job`].
    pub(crate) bound: Option<Option<JobPrice>>,
    /// Only resumes from a restored handle, never submits (#508): set for a job restored after
    /// a restart, which the provider already has.
    pub(crate) resume_only: bool,
}

impl QueuedJob {
    /// A job outside an authorized run: only the budgets bound `price`, which is the trusted
    /// host's statement. Jobs of an authorized run come from
    /// [`crate::run_cmds::AuthorizedRun::queued_job`].
    pub fn new(provider: crate::provider_routing::RegisteredProvider, job: ProviderJob, price: Option<JobPrice>) -> Self {
        Self { provider, job, price, bound: None, resume_only: false }
    }
    /// A job restored after a restart: it runs only by resuming its restored handle, and fails
    /// rather than submitting again when there is none.
    pub(crate) fn resume(provider: crate::provider_routing::RegisteredProvider, job: ProviderJob, price: Option<JobPrice>) -> Self {
        Self { resume_only: true, ..Self::new(provider, job, price) }
    }
    /// The price budgets and the run ceiling use: an authorized run's own admitted price, or
    /// `price` for other work.
    pub fn charged_price(&self) -> Option<&JobPrice> {
        match &self.bound {
            Some(bound) => bound.as_ref(),
            None => self.price.as_ref(),
        }
    }
    /// Whether the job comes from an authorized run.
    pub fn is_authorized(&self) -> bool {
        self.bound.is_some()
    }
}

/// A paid job's quote: the node and model it charges, and one job's estimate (`None` when the
/// price isn't known, which any cap refuses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobPrice {
    pub node: NodeId,
    pub model: String,
    pub estimate: Option<Money>,
    /// The authorized run's accepted-amount ceiling (#366), which every new priced reservation
    /// is debited against. Only [`crate::run_cmds::AuthorizedRun::queued_job`] attaches one, so
    /// no caller can attach another run's ceiling or widen one.
    pub(crate) ceiling: Option<crate::run_ceiling::RunCeiling>,
    /// The model's per-token rates (#595), which charge a finished job its measured usage when
    /// the provider reports tokens but no cost. Only an authorized run attaches them.
    pub(crate) rates: Option<nodecraft_catalog::TokenRates>,
}

/// What a finished job is charged as reported: the provider's own cost, or else its measured
/// tokens at the model's catalog rates (#595). `None` (charge the estimate) when neither is known.
fn measured(price: Option<&JobPrice>, usage: &nodecraft_providers::Usage) -> Option<Money> {
    usage.cost.or_else(|| {
        let rates = price?.rates.as_ref()?;
        rates.cost(usage.input_tokens?, usage.output_tokens?).ok()
    })
}

impl JobPrice {
    /// A price for work outside an authorized run, so with no run ceiling: only the budgets
    /// apply. Like the estimate itself, this is the trusted host's statement; prices for an
    /// authorized run come from [`crate::run_cmds::AuthorizedRun::queued_job`].
    pub fn new(node: NodeId, model: String, estimate: Option<Money>) -> Self {
        Self { node, model, estimate, ceiling: None, rates: None }
    }
    /// The run ceiling this price is bound to, if it came from an authorized run.
    pub fn ceiling(&self) -> Option<&crate::run_ceiling::RunCeiling> {
        self.ceiling.as_ref()
    }
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
    provider: nodecraft_types::ProviderTarget,
    /// The key sent to the provider (and charged under).
    key: String,
    /// The scheduler's key, which the registration keeps (#223).
    base: String,
}

impl Attempt {
    fn matches_job(&self, job: &QueuedJob) -> bool {
        job.provider.target() == &self.provider && job.job.idempotency_key == self.base
    }
}

/// A job's reservation in the usage log.
#[derive(Clone, Debug)]
struct Reserved {
    identity: LedgerIdentity,
    invocation: u64,
    provider: nodecraft_types::ProviderTarget,
    key: String,
    /// The scheduler's key `key` derives from.
    base: String,
    estimate: Money,
    /// The job may have reached the provider: settle as unresolved, never release.
    maybe_billed: bool,
    /// Credited back when the reservation is released.
    ceiling: Option<crate::run_ceiling::RunCeiling>,
    /// This reservation took the run's debit for its key; only that owner's release credits it,
    /// never a later job that found the key already reserved.
    debited: bool,
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
    handles: HashMap<(nodecraft_types::ProviderTarget, String), JobHandle>,
    /// Open reservations of jobs that haven't settled.
    reserved: HashMap<JobId, Reserved>,
    /// Jobs inside `run` right now, and those asked to settle meanwhile (done when `run` ends).
    running: HashMap<JobId, Attempt>,
    settle_after: std::collections::HashSet<JobId>,
    forget_after: std::collections::HashSet<JobId>,
    /// Generations reported failed, keyed by provider and scheduler base (main297).
    failures: HashMap<(nodecraft_types::ProviderTarget, String), u32>,
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
    fn registered(&self, provider: &nodecraft_types::ProviderTarget, base: String) -> Attempt {
        let failures = self.failures.get(&(provider.to_owned(), base.clone())).copied().unwrap_or(0);
        Attempt { provider: provider.clone(), key: sent_key(&base, failures), base }
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
    write_ahead: Option<Arc<WriteAhead>>,
}

/// Called before a paid job is first sent, once its estimate is in the usage log: saves what a
/// restart needs so the charge isn't lost if the process dies mid-submit (#508). An error stops
/// the job before it is sent, and is its failure reason.
pub type WriteAhead = dyn Fn() -> Result<(), String> + Send + Sync;

fn lock_log(budget: &JobBudget) -> std::sync::MutexGuard<'_, UsageLog> {
    budget.log.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends a reservation: charged as unresolved if the job may have been billed, released if not.
fn close(budget: &JobBudget, r: &Reserved) {
    let mut log = lock_log(budget);
    if log.ledger_identity() != r.identity {
        return;
    }
    let open = log.charge_target(&r.provider, &r.key).is_some_and(|e| e.status == Charge::Estimate);
    if !open {
        return;
    }
    // Neither can fail for an open estimate in its own currency; if one did, the estimate stays
    // charged, which is the safe side.
    if r.maybe_billed {
        let _ = log.settle_target(&r.provider, &r.key, r.estimate, Charge::Unresolved);
    } else if log.release_target(&r.provider, &r.key).is_ok()
        && r.debited
        && let Some(ceiling) = &r.ceiling
    {
        // Released on evidence that nothing reached the provider: the run may use the room again.
        ceiling.credit(&r.provider, &r.key);
    }
}

/// Reserves (or adopts) a priced job's estimate. `Err` is the refusal message.
fn reserve(budget: &JobBudget, attempt: &Attempt, price: &JobPrice, resumed: bool, at_ms: u64) -> Result<Option<Reserved>, String> {
    if budget.session.is_empty() {
        return Err(format!("{REFUSAL_PREFIX}the budget has no session id, so charges can't be recorded (a host setup error)"));
    }
    let budgets = budget.budgets.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut log = lock_log(budget);
    let maybe_billed = resumed;
    let (provider, key) = (&attempt.provider, attempt.key.as_str());
    if log.ownership_target(provider, key).map_err(|e| format!("{REFUSAL_PREFIX}{}", e.message))? == Some(ChargeOwnership::LegacyUncertain) {
        return Err(format!("{REFUSAL_PREFIX}historical charge ownership is uncertain; inspect the usage log before retrying"));
    }
    if let Some(entry) = log.charge_target(provider, key) {
        // A retry of a job already reserved (or settled) under this key: nothing new to charge
        // in the log, which is shared across runs. This run still accounts the work once (a
        // same-run retry finds its own debit); the debit is never credited from here.
        account_in_run(price, provider, key, Some(entry.cost), resumed)?;
        return Ok((entry.status == Charge::Estimate).then(|| Reserved {
            identity: log.ledger_identity(),
            invocation: 0,
            provider: provider.clone(),
            key: key.into(),
            base: attempt.base.clone(),
            estimate: entry.cost,
            maybe_billed,
            ceiling: price.ceiling.clone(),
            debited: false,
        }));
    }
    if log.has_recorded_target(provider, key).map_err(|e| format!("{REFUSAL_PREFIX}{}", e.message))? {
        account_in_run(price, provider, key, price.estimate, resumed)?;
        return Ok(None);
    }
    let Some(estimate) = price.estimate else {
        // An unknown price: fine without caps (nothing to charge), refused with any.
        let capped = budgets.per_call.is_some() || budgets.session.is_some() || budgets.project.is_some();
        if capped && !resumed {
            return Err(format!("{REFUSAL_PREFIX}the price of {} isn't known, so the budget can't be checked", price.model));
        }
        // An authorized run admits only as many unknown-price jobs as were accepted.
        if let Some(ceiling) = &price.ceiling {
            ceiling.admit_unpriced(provider, key, resumed).map_err(|e| format!("{REFUSAL_PREFIX}{e}"))?;
        }
        return Ok(None);
    };
    if !resumed {
        let step = StepQuote { node: price.node, models: vec![price.model.clone()], per_job: Ok(Some(estimate)), jobs: 1, jobs_known: true };
        let quote = nodecraft_exec::cost::Quote { steps: vec![step], totals: [(estimate.currency, estimate)].into_iter().collect() };
        let blocks = check(&quote, &budgets, &log.spent(&budget.session)).map_err(|e| format!("{REFUSAL_PREFIX}{}", e.message))?.blocks;
        if let Some(block) = blocks.first() {
            return Err(format!("{REFUSAL_PREFIX}{}", block.message()));
        }
    }
    // The run's accepted-amount ceiling, under the log lock so parallel jobs can't both take
    // the last of it. An adopted job already ran: debited, never refused.
    let debited = match &price.ceiling {
        Some(ceiling) => ceiling.debit(provider, key, estimate, resumed).map_err(|e| format!("{REFUSAL_PREFIX}{e}"))?,
        None => false,
    };
    let entry = UsageEntry {
        at_ms,
        session: budget.session.clone(),
        node: price.node,
        provider: provider.adapter().to_owned(),
        instance: provider.instance(),
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
            provider: provider.clone(),
            key: key.into(),
            base: attempt.base.clone(),
            estimate,
            maybe_billed,
            ceiling: price.ceiling.clone(),
            debited,
        })),
        Err(e) if !resumed || matches!(e.kind, nodecraft_types::ErrorKind::Busy | nodecraft_types::ErrorKind::Conflict) => {
            Err(format!("{REFUSAL_PREFIX}{}", e.message))
        }
        Err(_) => Ok(None),
    }
    // Nothing was recorded for this key, so the debit just taken is undone, unless the work was
    // already submitted: failing to record its charge proves nothing about delivery.
    .inspect_err(|_| undo_debit(debited && !resumed, price, provider, key))
    .inspect(|reserved| {
        if reserved.is_none() {
            undo_debit(debited && !resumed, price, provider, key);
        }
    })
}

/// Accounts work whose key the usage log already holds (from this run or an earlier one) in this
/// run's ceiling. Money and the unknown-price count are independent: any retained `exposure` (the
/// row's amount, which for an open estimate or unresolved charge is not a known bill) is debited,
/// and a job the authorized run classifies as unknown-price also uses one of the accepted count.
/// Adopted work is accounted past both; new work is refused past either, and a refusal undoes
/// the debit this call took.
fn account_in_run(price: &JobPrice, provider: &nodecraft_types::ProviderTarget, key: &str, exposure: Option<Money>, resumed: bool) -> Result<(), String> {
    let Some(ceiling) = &price.ceiling else { return Ok(()) };
    let refusal = |e: String| format!("{REFUSAL_PREFIX}{e}");
    let debited = match exposure {
        Some(money) => ceiling.debit(provider, key, money, resumed).map_err(refusal)?,
        None => false,
    };
    if price.estimate.is_none()
        && let Err(e) = ceiling.admit_unpriced(provider, key, resumed)
    {
        if debited && !resumed {
            ceiling.credit(provider, key);
        }
        return Err(refusal(e));
    }
    Ok(())
}

/// Nothing was recorded for this key, so the debit just taken for it is undone.
fn undo_debit(debited: bool, price: &JobPrice, provider: &nodecraft_types::ProviderTarget, key: &str) {
    if let (true, Some(ceiling)) = (debited, &price.ceiling) {
        ceiling.credit(provider, key);
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
                let old = s.registered(q.queued.provider.target(), q.queued.job.idempotency_key);
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
        Self { state: Arc::default(), runtime, limits, on_output, on_update, budget: None, write_ahead: None }
    }

    /// Calls `write_ahead` before each paid job is first sent (see [`WriteAhead`]).
    pub fn with_write_ahead(mut self, write_ahead: Arc<WriteAhead>) -> Self {
        self.write_ahead = Some(write_ahead);
        self
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
            && let Some(old) = s.jobs.get(&id).filter(|old| old.provider.target() == job.provider.target())
        {
            job.job.idempotency_key.clone_from(&old.job.idempotency_key);
        }
        let incoming = s.registered(job.provider.target(), job.job.idempotency_key.clone());
        let scheduler = s.jobs.get_mut(&id).filter(|old| old.provider.target() == job.provider.target()).and_then(|old| old.scheduler.take());
        s.metadata.retain(|attempt, held| held.job != id || *attempt == incoming || scheduler.as_ref().is_some_and(|lease| lease.attempt == *attempt));
        if let Some(old) = s.jobs.insert(id, RegisteredJob { queued: job, identity: Arc::new(()), scheduler }) {
            let attempt = s.registered(old.queued.provider.target(), old.queued.job.idempotency_key);
            s.prune_handle(&attempt);
            if let Some(lease) = old.scheduler {
                s.prune_handle(&lease.attempt);
            }
        }
    }

    /// Restores a first-generation handle saved before a restart (from `RunUpdate::Submitted`),
    /// keyed by the provider that issued it and the scheduler's idempotency key, so the job is
    /// resumed rather than submitted again. See [`Self::restore_generation`].
    pub fn restore_handle(&self, provider: impl Into<String>, idempotency_key: String, handle: JobHandle) -> Result<(), RestoreHandleError> {
        self.restore_generation(provider, idempotency_key, 0, handle)
    }

    /// Restores a handle saved before a restart with the [`Sent::generation`] it was reported
    /// with: it is stored under the derived key it was sent under, and the job's failure count is
    /// brought back, so the retry resumes that generation and charges under its key.
    ///
    /// Only the legacy/default provider address is validated. Arbitrary adapter strings within
    /// the target bound and legacy base keys (including keys normalized by [`sent_key`]) remain
    /// supported; this does not apply the stricter named-restoration key policy.
    /// On refusal, runner and accounting state are unchanged and the error owns the original
    /// accepted handle. Keep the persisted provider/base/generation identity until restoration
    /// succeeds, recover the handle with [`RestoreHandleError::into_handle`], and explicitly retry
    /// after correcting the address. Never delete the saved identity or submit a replacement.
    pub fn restore_generation(
        &self,
        provider: impl Into<String>,
        idempotency_key: String,
        generation: u32,
        handle: JobHandle,
    ) -> Result<(), RestoreHandleError> {
        let provider = provider.into();
        let target = match nodecraft_types::ProviderTarget::default_instance(&provider) {
            Ok(target) => target,
            Err(cause) => return Err(RestoreHandleError::new(cause, handle)),
        };
        self.restore_identity(target, idempotency_key, generation, handle);
        Ok(())
    }
    /// Trusted host restoration requires the actual endpoint binding, never a parsed lane.
    pub fn restore_target_generation(
        &self,
        binding: &crate::provider_routing::RegisteredProvider,
        idempotency_key: String,
        generation: u32,
        handle: JobHandle,
    ) -> Result<(), nodecraft_types::Error> {
        if idempotency_key.is_empty() || idempotency_key.len() > 64 || !idempotency_key.is_ascii() {
            return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, "invalid restored scheduler identity"));
        }
        self.restore_identity(binding.target().try_clone()?, idempotency_key, generation, handle);
        Ok(())
    }
    fn restore_identity(&self, provider: nodecraft_types::ProviderTarget, idempotency_key: String, generation: u32, handle: JobHandle) {
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
            let old = s.registered(q.queued.provider.target(), q.queued.job.idempotency_key);
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
        let write_ahead = self.write_ahead.clone();
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
                if queued.provider.target().adapter().len() > nodecraft_exec::cost::MAX_TEXT
                    || queued.charged_price().is_some_and(|p| p.model.len() > nodecraft_exec::cost::MAX_TEXT)
                {
                    return Outcome::Failed { error: format!("{REFUSAL_PREFIX}accounting identity metadata is too long"), transient: false };
                }
                // An authorized run's paid work is admitted only against its budget and ceiling.
                if budget.is_none() && queued.is_authorized() && queued.charged_price().is_some() {
                    return Outcome::Failed {
                        error: format!("{REFUSAL_PREFIX}this runner has no budget, so the authorized run's paid job can't be accounted"),
                        transient: false,
                    };
                }
                (queued.queued.clone(), Arc::clone(&queued.identity))
            };
            let provider_id = queued.provider.target().clone();
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
            if handle.is_none() && queued.resume_only {
                return Outcome::Failed {
                    error: "the provider's handle for this job is gone, so it can't be followed without sending it again".into(),
                    transient: false,
                };
            }
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
                let at_ms = if queued.charged_price().is_some() { budget.as_ref().map_or(0, |b| (b.wall_clock_ms)()) } else { 0 };
                let identity = if let Some(budget) = &budget {
                    let log = lock_log(budget);
                    match log.ownership_target(&provider_id, &key) {
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
                    && let (Some(budget), Some(price)) = (&budget, queued.charged_price())
                {
                    match reserve(budget, &attempt, price, handle.is_some(), at_ms) {
                        Ok(Some(r)) => {
                            let mut s = lock();
                            let maybe = s.reserved.get(&job_id).is_some_and(|old| old.matches(&attempt) && old.maybe_billed) || r.maybe_billed;
                            // A retry of this job's own attempt keeps the debit it took.
                            let debited = s.reserved.get(&job_id).is_some_and(|old| old.matches(&attempt) && old.debited) || r.debited;
                            s.reserved.insert(job_id, Reserved { invocation, maybe_billed: maybe, debited, ..r });
                        }
                        Ok(None) => {}
                        Err(refusal) => return Outcome::Failed { error: refusal, transient: false },
                    }
                }
                if guard.metadata.is_none()
                    && let (Some(budget), Some(price)) = (&budget, queued.charged_price())
                {
                    let mut log = lock_log(budget);
                    match log.has_recorded_target(&provider_id, &key) {
                        Ok(false) => {
                            let m = UsageMetadata {
                                at_ms,
                                session: &budget.session,
                                node: price.node,
                                provider: provider_id.adapter(),
                                instance: provider_id.instance(),
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
                    on_update(job_id, Sent { key: &key, base: &start.idempotency_key, generation, target: &provider_id, attempt: start.attempt }, update);
                };
                if handle.is_none()
                    && queued.charged_price().is_some()
                    && let Some(write_ahead) = &write_ahead
                    && let Err(reason) = write_ahead()
                {
                    // Nothing was sent: the estimate is released.
                    let reserved = lock().take_reserved(job_id, &attempt);
                    if let (Some(budget), Some(r)) = (&budget, reserved) {
                        close(budget, &r);
                    }
                    return Outcome::Failed { error: reason, transient: false };
                }
                let result = match handle {
                    Some(handle) => nodecraft_providers::resume(queued.provider.provider().as_ref(), &handle, &signal, &clock, limits, record).await,
                    None => nodecraft_providers::run_to_completion(queued.provider.provider().as_ref(), &queued.job, &signal, &clock, limits, record).await,
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
                            price: queued.charged_price(),
                            reserved: reserved.as_ref(),
                            identity,
                            attempt: &attempt,
                            permit: guard.metadata.take(),
                            reported: measured(queued.charged_price(), &done.usage),
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
                        let will_retry = failure.error.is_transient()
                            && (failure.handle.is_some() || !failure.uncertain || queued.provider.provider().retry_ambiguous_submit());
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
        runner.register(JobId(1), QueuedJob::new(crate::provider_routing::RegisteredProvider::default_instance(provider.clone()).unwrap(), job, None));
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
        runner.register(JobId(1), QueuedJob::new(crate::provider_routing::RegisteredProvider::default_instance(provider.clone()).unwrap(), job, None));
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
        runner.register(JobId(1), QueuedJob::new(crate::provider_routing::RegisteredProvider::default_instance(provider.clone()).unwrap(), job, None));
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
        QueuedJob {
            provider: crate::provider_routing::RegisteredProvider::default_instance(provider).unwrap(),
            job,
            price: Some(JobPrice { node: NodeId::from_bytes([5; 16]), model: "test.img".into(), estimate: micros.map(usd), ceiling: None, rates: None }),
            bound: None,
            resume_only: false,
        }
    }

    fn job(n: u64) -> Start {
        Start { job: JobId(n), lane: Lane::new("x"), attempt: 1, idempotency_key: format!("nodecraft:r:{n}") }
    }

    fn spent(b: &JobBudget) -> u64 {
        b.log.lock().unwrap().spent("now").project.get(&nodecraft_types::Currency::USD).map_or(0, |m| m.micros)
    }

    #[test]
    fn the_run_ceiling_is_debited_once_and_credited_only_by_its_owner() {
        let (_, budget) = budgeted(None);
        let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 20)].into(), 0);
        let price =
            JobPrice { node: NodeId::from_bytes([5; 16]), model: "test.img".into(), estimate: Some(usd(20)), ceiling: Some(ceiling.clone()), rates: None };
        let target = nodecraft_types::ProviderTarget::default_instance("p").unwrap();
        let attempt = |key: &str| Attempt { provider: target.clone(), key: key.into(), base: key.into() };
        let usd_status = || ceiling.status(nodecraft_types::Currency::USD);
        // The owner's debit; a second job finding the key reserved takes none.
        let owner = reserve(&budget, &attempt("a"), &price, false, 1).unwrap().unwrap();
        assert!(owner.debited);
        let finder = reserve(&budget, &attempt("a"), &price, false, 1).unwrap().unwrap();
        assert!(!finder.debited);
        assert_eq!(usd_status(), (20, 20));
        assert!(reserve(&budget, &attempt("b"), &price, false, 1).unwrap_err().contains("accepted"), "one over the ceiling");
        assert_eq!(budget.log.lock().unwrap().entries().count(), 1, "the refused job recorded nothing");
        // A possibly billed release keeps the debit; a no-delivery release by the owner frees it.
        let billed = Reserved { maybe_billed: true, ..owner.clone() };
        close(&budget, &billed);
        assert_eq!(usd_status(), (20, 20));
        let (_, budget) = budgeted(None);
        let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 20)].into(), 0);
        let price = JobPrice { ceiling: Some(ceiling.clone()), ..price };
        let owner = reserve(&budget, &attempt("a"), &price, false, 1).unwrap().unwrap();
        close(&budget, &Reserved { debited: false, ..owner.clone() });
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (20, 20), "only the debit's owner credits it");
        let owner = reserve(&budget, &attempt("c"), &price, false, 1);
        assert!(owner.is_err(), "the non-owner close released the estimate but kept the debit");
        let (_, budget) = budgeted(None);
        let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 20)].into(), 0);
        let price = JobPrice { ceiling: Some(ceiling.clone()), ..price };
        let owner = reserve(&budget, &attempt("a"), &price, false, 1).unwrap().unwrap();
        close(&budget, &owner);
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (20, 0));
        assert!(reserve(&budget, &attempt("b"), &price, false, 1).unwrap().unwrap().debited, "the room is used again");
        // Adopted work is debited past the ceiling, never refused.
        assert!(reserve(&budget, &attempt("d"), &price, true, 1).unwrap().unwrap().debited);
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (20, 40));
    }

    #[test]
    fn adopted_work_keeps_its_debit_when_its_charge_cant_be_recorded() {
        // A log at its project limit refuses every new charge (Overflow).
        let mut file = nodecraft_exec::cost::UsageLogFile::default();
        let full = usd(nodecraft_exec::cost::MAX_TOTAL_MICROS);
        file.project = [(nodecraft_types::Currency::USD, full)].into_iter().collect();
        file.sessions = [("old".to_owned(), [(nodecraft_types::Currency::USD, full)].into_iter().collect())].into_iter().collect();
        let (_, budget) = budgeted(None);
        *budget.log.lock().unwrap() = UsageLog::try_from(file).unwrap();
        let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 100)].into(), 0);
        let price =
            JobPrice { node: NodeId::from_bytes([5; 16]), model: "test.img".into(), estimate: Some(usd(80)), ceiling: Some(ceiling.clone()), rates: None };
        let target = nodecraft_types::ProviderTarget::default_instance("p").unwrap();
        let attempt = |key: &str| Attempt { provider: target.clone(), key: key.into(), base: key.into() };
        // New work that can't be recorded is refused and leaves no debit.
        assert!(reserve(&budget, &attempt("fresh"), &price, false, 1).is_err());
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (100, 0));
        // Adopted work already ran: it goes ahead uncharged, but its exposure stays on the run.
        assert!(reserve(&budget, &attempt("adopted"), &price, true, 1).unwrap().is_none());
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (100, 80));
        assert!(reserve(&budget, &attempt("next"), &price, false, 1).is_err(), "80 + 80 would pass 100");
    }

    #[test]
    fn an_authorized_runs_bound_price_wins_and_needs_a_budget() {
        let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, 100)].into(), 0);
        let bound = JobPrice { node: NodeId::from_bytes([5; 16]), model: "test.img".into(), estimate: Some(usd(80)), ceiling: Some(ceiling), rates: None };
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        let mut queued = priced(provider.clone(), Some(80));
        queued.bound = Some(Some(bound.clone()));
        // A host that swaps the public price changes nothing the budget sees.
        queued.price = Some(JobPrice::new(NodeId::from_bytes([5; 16]), "test.img".into(), Some(usd(1))));
        assert_eq!(queued.charged_price(), Some(&bound));
        queued.price = None;
        assert_eq!(queued.charged_price(), Some(&bound));
        assert!(queued.is_authorized() && !QueuedJob::new(queued.provider.clone(), queued.job.clone(), None).is_authorized());
        // A runner without a budget refuses the authorized run's paid job before the provider.
        let runner = ProviderRunner::new(Arc::new(InstantRuntime(AtomicU64::new(0))), RunLimits::default(), Arc::new(|_, _| {}), Arc::new(|_, _, _| {}));
        runner.register(JobId(1), queued);
        let refused = block_on(runner.run(job(1), CancelToken::default()));
        assert!(matches!(&refused, Outcome::Failed { transient: false, error } if is_budget_refusal(error) && error.contains("no budget")), "{refused:?}");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn work_adopted_from_an_earlier_runs_row_counts_in_the_new_run() {
        // Run A reserved "old" (USD 80) in the shared log and it may have been billed. Run B,
        // accepting USD 100, adopts that submission: it accounts the 80 even though the log needs
        // no new row, so a fresh 80 doesn't fit.
        let (_, budget) = budgeted(None);
        let target = nodecraft_types::ProviderTarget::default_instance("p").unwrap();
        let attempt = |key: &str| Attempt { provider: target.clone(), key: key.into(), base: key.into() };
        let usd_ceiling = |limit: u64, unpriced: u64| crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, limit)].into(), unpriced);
        let price = |estimate: Option<u64>, ceiling: &crate::run_ceiling::RunCeiling| JobPrice {
            node: NodeId::from_bytes([5; 16]),
            model: "test.img".into(),
            estimate: estimate.map(usd),
            ceiling: Some(ceiling.clone()),
            rates: None,
        };
        let run_a = usd_ceiling(100, 0);
        reserve(&budget, &attempt("old"), &price(Some(80), &run_a), false, 1).unwrap().unwrap();
        let run_b = usd_ceiling(100, 0);
        let adopted = reserve(&budget, &attempt("old"), &price(Some(80), &run_b), true, 1).unwrap().unwrap();
        assert!(!adopted.debited, "never credited from the adoption path");
        assert_eq!(run_b.status(nodecraft_types::Currency::USD), (100, 80));
        assert_eq!(run_b.unpriced_status(), (0, 0), "priced work never touches the count");
        assert!(reserve(&budget, &attempt("fresh"), &price(Some(80), &run_b), false, 1).unwrap_err().contains("accepted"));
        // Re-adopting the same submission in run B accounts nothing more.
        reserve(&budget, &attempt("old"), &price(Some(80), &run_b), true, 1).unwrap();
        assert_eq!(run_b.status(nodecraft_types::Currency::USD), (100, 80));
        // Once settled, the row still counts in a run that adopts it, priced or not.
        budget.log.lock().unwrap().settle_target(&target, "old", usd(80), Charge::Actual).unwrap();
        let run_c = usd_ceiling(100, 0);
        assert!(reserve(&budget, &attempt("old"), &price(Some(80), &run_c), true, 1).unwrap().is_none());
        assert_eq!(run_c.status(nodecraft_types::Currency::USD), (100, 80));
        let run_d = usd_ceiling(100, 1);
        reserve(&budget, &attempt("old"), &price(None, &run_d), true, 1).unwrap();
        assert_eq!(run_d.status(nodecraft_types::Currency::USD), (100, 80), "the row's own amount is accounted");
        assert_eq!(run_d.unpriced_status(), (1, 1), "and the authorized unknown-price job is counted");
        // New work on a key another run holds is admitted only within this run's room.
        let run_e = usd_ceiling(50, 0);
        assert!(reserve(&budget, &attempt("old"), &price(Some(80), &run_e), false, 1).is_err());
    }

    #[test]
    fn adopting_an_estimated_or_unresolved_row_uses_the_unknown_price_count() {
        // An earlier priced run left an open estimate (or an unresolved charge) of USD 80 for
        // "old". A new run whose authorized step is unknown-price, accepting one such job,
        // adopts that submission: it keeps the money and uses the count, so a fresh-key
        // unknown-price job is refused before the provider.
        for unresolved in [false, true] {
            let (runner, budget) = budgeted(None);
            let target = crate::provider_routing::RegisteredProvider::default_instance(Arc::new(Flaky {
                submits: AtomicU64::new(0),
                polls: AtomicU64::new(0),
                fail_polls: 0,
            }))
            .unwrap()
            .target()
            .clone();
            let attempt = |key: &str| Attempt { provider: target.clone(), key: key.into(), base: key.into() };
            let earlier = JobPrice::new(NodeId::from_bytes([5; 16]), "test.img".into(), Some(usd(80)));
            reserve(&budget, &attempt("old"), &earlier, false, 1).unwrap().unwrap();
            if unresolved {
                budget.log.lock().unwrap().settle_target(&target, "old", usd(80), Charge::Unresolved).unwrap();
            }
            let ceiling = crate::run_ceiling::RunCeiling::new(std::collections::BTreeMap::new(), 1);
            let unknown =
                JobPrice { node: NodeId::from_bytes([5; 16]), model: "test.usage".into(), estimate: None, ceiling: Some(ceiling.clone()), rates: None };
            // (a) The adoption uses the count and (c) keeps the row's money.
            reserve(&budget, &attempt("old"), &unknown, true, 1).unwrap();
            assert_eq!(ceiling.unpriced_status(), (1, 1), "unresolved: {unresolved}");
            assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (0, 80));
            // (b) Adopting the same submission again uses nothing more.
            reserve(&budget, &attempt("old"), &unknown, true, 1).unwrap();
            assert_eq!(ceiling.unpriced_status(), (1, 1));
            assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (0, 80));
            // (d) A fresh-key unknown-price job of the run is refused by the runner before the
            // provider.
            let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
            let mut queued = priced(provider.clone(), None);
            queued.bound = Some(Some(unknown.clone()));
            runner.register(JobId(9), queued);
            let refused = block_on(runner.run(job(9), CancelToken::default()));
            assert!(matches!(&refused, Outcome::Failed { transient: false, error } if is_budget_refusal(error) && error.contains("used up")), "{refused:?}");
            assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
            assert_eq!(ceiling.unpriced_status(), (1, 1));
        }
    }

    #[test]
    fn credits_cannot_reopen_room_after_adopted_exposure_saturates() {
        // Admitted prices only: a legitimate USD 100 reservation, then two adopted submissions at
        // u64::MAX − 100 whose log rows are refused by the per-charge bound.
        let (_, budget) = budgeted(None);
        let target = nodecraft_types::ProviderTarget::default_instance("p").unwrap();
        let attempt = |key: &str| Attempt { provider: target.clone(), key: key.into(), base: key.into() };
        let ceiling = crate::run_ceiling::RunCeiling::new([(nodecraft_types::Currency::USD, u64::MAX)].into(), 0);
        let price = |micros: u64| JobPrice {
            node: NodeId::from_bytes([5; 16]),
            model: "test.img".into(),
            estimate: Some(usd(micros)),
            ceiling: Some(ceiling.clone()),
            rates: None,
        };
        let legit = reserve(&budget, &attempt("legit"), &price(100), false, 1).unwrap().unwrap();
        for key in ["h1", "h2"] {
            assert!(reserve(&budget, &attempt(key), &price(u64::MAX - 100), true, 1).unwrap().is_none(), "adopted, uncharged");
        }
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (u64::MAX, u64::MAX));
        // The legitimate reservation is released on no-delivery evidence: the room stays shut.
        close(&budget, &legit);
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "released");
        assert_eq!(ceiling.status(nodecraft_types::Currency::USD), (u64::MAX, u64::MAX));
        assert!(reserve(&budget, &attempt("fresh"), &price(100), false, 1).unwrap_err().contains("accepted"));
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

    /// Reports tokens, and a cost only when it has one.
    struct Tokens(Option<Money>);

    impl Provider for Tokens {
        fn id(&self) -> &str {
            "tokens"
        }
        fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            let mut done = Completed::new(vec![Output::Text("ok".into())]);
            done.usage.input_tokens = Some(1_000);
            done.usage.output_tokens = Some(2_000);
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

    #[test]
    fn measured_tokens_are_charged_at_the_models_rates() {
        // #595: 1,000 × 0.0001 + 2,000 × 0.0005 USD per 1K tokens.
        let rates = nodecraft_catalog::TokenRates { input: "0.0001 USD".parse().unwrap(), output: "0.0005 USD".parse().unwrap(), long: None };
        let with_rates = |provider: Arc<dyn Provider>| {
            let mut job = priced(provider, Some(40_000));
            if let Some(price) = job.price.as_mut() {
                price.rates = Some(rates.clone());
            }
            job
        };
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), with_rates(Arc::new(Tokens(None))));
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
        runner.register(JobId(2), with_rates(Arc::new(Tokens(Some(usd(7))))));
        assert_eq!(block_on(runner.run(job(2), CancelToken::default())), Outcome::Done);
        runner.register(JobId(3), priced(Arc::new(Tokens(None)), Some(40_000)));
        assert_eq!(block_on(runner.run(job(3), CancelToken::default())), Outcome::Done);
        let log = budget.log.lock().unwrap();
        let rows: Vec<(u64, Charge)> = log.entries().map(|e| (e.cost.micros, e.status)).collect();
        // Measured at the rates; a reported cost wins; with no rates, the estimate.
        assert_eq!(rows, [(1_100, Charge::Actual), (7, Charge::Actual), (40_000, Charge::Actual)]);
    }

    #[test]
    fn a_job_resumed_after_a_restart_is_charged_even_past_the_cap() {
        let (runner, budget) = budgeted(Some(100_000));
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        runner.register(JobId(1), priced(provider.clone(), Some(900_000)));
        runner.restore_handle("flaky", "nodecraft:r:1".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null }).unwrap();
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done, "already paid for");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0, "resumed, not resubmitted");
        assert_eq!(spent(&budget), 900_000);
    }

    #[test]
    fn a_resume_only_job_without_its_handle_fails_rather_than_submitting() {
        // #508: a job restored after a restart was already sent; with no handle it can't be
        // followed, and sending it again could bill it twice.
        let (runner, _budget) = budgeted(None);
        let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 0 });
        let QueuedJob { provider: binding, job: provider_job, price, .. } = priced(provider.clone(), None);
        runner.register(JobId(1), QueuedJob::resume(binding, provider_job, price));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0, "never sent again");
        runner.restore_handle("flaky", "nodecraft:r:1".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null }).unwrap();
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done, "followed from its handle");
        assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
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
        runner.restore_handle("flaky", "nodecraft:r:2".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null }).unwrap();
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
        runner.restore_handle("resumes", "nodecraft:r:2".into(), JobHandle { remote_id: "x".into(), data: serde_json::Value::Null }).unwrap();
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
    mod endpoint_tests;
    mod fresh_key_tests;
    #[cfg(not(target_arch = "wasm32"))]
    mod native_build_tests;
    mod paid_unpriced_tests;
    mod reregister_retry_tests;
    mod restore_tests;
    mod stale_handle_tests;
    mod strict257_tests;
}
