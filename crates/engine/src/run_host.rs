//! Drives authorized runs in a host (#409): the production counterpart of the driver goldens.
//!
//! [`RunHost::register`] provides the session's `run.start` service and [`RunHost::install`] its
//! budget resources. `run.start` hands each accepted run to the host's inbox; [`RunHost::pump`],
//! called on the UI thread every frame, does the rest:
//!
//! 1. **Authorize** the prepared run against the session as it is now.
//! 2. **Queue** its jobs: model jobs through [`AuthorizedRun::queued_job_with_media`] (media read
//!    from the session's assets, #502) on a [`ProviderRunner`] held to the shared [`JobBudget`],
//!    sources and local actions on a [`SourceRunner`]. The driver runs on the host's [`Runtime`].
//! 3. **Absorb** what the driver reports. A finished model job's outputs are converted
//!    ([`generate::produced`]), its assets imported, and the step advanced. Every model job that
//!    ends, however it ends, is let go with `ProviderRunner::forget`, which settles its
//!    reservation: charged as unresolved when it may have been billed, released when it provably
//!    wasn't.
//! 4. **Record** each finished step with `run.record` as soon as it finishes, one step per call,
//!    so one large result can't lose the others (the 1 MiB command limit). A step whose record is
//!    refused is reported as failed on its card.
//!
//! Callbacks from the driver and the runners only move messages into a queue; the session is
//! touched only in `pump`. After every settlement the budget resources are refreshed from the
//! usage log, so the next quote counts what was spent.
//!
//! Not yet: the usage log and submitted handles aren't saved, so a run cut short by quitting is
//! not resumed and its open reservations are lost with the process; and media is read on the UI
//! thread when a job is queued.
use crate::Session;
use crate::action_exec::LocalActions;
use crate::commands::Services;
use crate::generate;
use crate::jobs::{CompletionDelivery, JobBudget, ProviderRunner};
use crate::run_cmds::{AuthorizedRun, BudgetState, PreparedRun, UsageSnapshot};
use nodecraft_exec::cost::Budgets;
use nodecraft_exec::driver::{CancelToken, Handle, JobRunner, Runtime, TaskFuture, driver};
use nodecraft_exec::run::{Progress, RunOptions, SourceRunner, StepResult, take_outputs};
use nodecraft_exec::scheduler::{CancelScope, Event, JobId, Outcome, RetryPolicy, Scheduler, Start};
use nodecraft_nodes::JobSpec;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

/// Most runs the host holds at once; `run.start` beyond it is refused.
pub const MAX_RUNS: usize = 8;
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a run did, for the host to show on cards and in the status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunNotice {
    /// A run was authorized and its first jobs queued.
    Started { canvas: CanvasId, nodes: Vec<NodeId> },
    /// A step's result was recorded on its card.
    Recorded { canvas: CanvasId, node: NodeId },
    /// A step produced nothing, and why.
    Failed { canvas: CanvasId, node: NodeId, reason: String },
    /// A run could not start (authorization or queueing refused it); nothing was sent.
    Refused { reason: String },
    /// Every step of a run has finished or failed.
    Finished { canvas: CanvasId },
}

/// What the driver and the runner report, waiting for `pump`. Bounded by the run's jobs, which
/// admission bounds.
#[derive(Default)]
struct Mailbox {
    events: VecDeque<Event>,
    deliveries: BTreeMap<JobId, CompletionDelivery>,
    /// Jobs the scheduler refused to queue; they never start.
    rejected: Vec<(JobId, String)>,
    /// The driver returned: every job is finished and the handle is gone.
    ended: bool,
}

/// Runs a job on the provider runner or the source runner, whichever it was registered with.
struct Either {
    provider: Arc<ProviderRunner>,
    sources: SourceRunner,
    local: Mutex<BTreeSet<JobId>>,
}
impl JobRunner for Either {
    fn run(&self, start: Start, cancel: CancelToken) -> TaskFuture<Outcome> {
        if lock(&self.local).contains(&start.job) { self.sources.run(start, cancel) } else { self.provider.run(start, cancel) }
    }
}

struct Active {
    canvas: CanvasId,
    run: AuthorizedRun,
    runner: Arc<Either>,
    actions: Arc<LocalActions>,
    mailbox: Arc<Mutex<Mailbox>>,
    handle: Option<Handle>,
    /// Model jobs, which settle when they end without a result.
    provider_jobs: BTreeSet<JobId>,
    /// Jobs queued and not yet ended, and the node each works for.
    live: BTreeMap<JobId, NodeId>,
}

/// See the module docs.
pub struct RunHost {
    inbox: Arc<Mutex<VecDeque<PreparedRun>>>,
    runs: Vec<Active>,
    runtime: Arc<dyn Runtime>,
    budget: JobBudget,
    limits: nodecraft_providers::RunLimits,
    retry: RetryPolicy,
}

impl RunHost {
    /// A host running jobs on `runtime`, charging paid jobs to `budget`.
    pub fn new(runtime: Arc<dyn Runtime>, budget: JobBudget) -> Self {
        Self { inbox: Arc::default(), runs: Vec::new(), runtime, budget, limits: nodecraft_providers::RunLimits::default(), retry: RetryPolicy::default() }
    }

    /// Retries jobs with `retry` instead of the scheduler's default.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Registers the `run.start` service on `services`; call once, before the session is built.
    pub fn register(&self, services: &mut Services) -> Result<(), Error> {
        let inbox = Arc::clone(&self.inbox);
        services.register_run_host(move |prepared| {
            let mut inbox = lock(&inbox);
            if inbox.len() >= MAX_RUNS {
                return Err(Error::new(ErrorKind::Busy, "too many runs are waiting to start; try again when one finishes"));
            }
            inbox.push_back(prepared);
            Ok(json!({"accepted": true}))
        })
    }

    /// Installs the budget resources quotes read, from the shared usage log.
    pub fn install(&self, session: &mut Session) {
        refresh(session, &self.budget);
    }

    /// Whether any run is waiting or running.
    pub fn is_busy(&self) -> bool {
        !self.runs.is_empty() || !lock(&self.inbox).is_empty()
    }

    /// The nodes with work queued or running, for "running" badges.
    pub fn running(&self) -> BTreeSet<(CanvasId, NodeId)> {
        self.runs.iter().flat_map(|active| active.live.values().map(move |node| (active.canvas, *node))).collect()
    }

    /// Asks every run to stop: queued jobs are cancelled and running ones are asked to cancel.
    /// Their results still arrive through [`Self::pump`].
    pub fn cancel_all(&mut self) {
        for active in &self.runs {
            if let Some(handle) = &active.handle {
                let _ = handle.cancel(CancelScope::All);
            }
        }
    }

    /// Starts accepted runs and absorbs finished work. UI thread; never blocks on a provider.
    pub fn pump(&mut self, session: &mut Session) -> Vec<RunNotice> {
        let mut notices = Vec::new();
        loop {
            let next = lock(&self.inbox).pop_front();
            let Some(prepared) = next else { break };
            if let Err(error) = self.start(session, prepared, &mut notices) {
                notices.push(RunNotice::Refused { reason: error.message });
            }
        }
        let mut settled = false;
        self.runs.retain_mut(|active| {
            // Read first: everything the driver reported before it returned is then absorbed.
            let ended = lock(&active.mailbox).ended;
            settled |= absorb(session, active, &mut notices, &self.runtime);
            if ended {
                notices.push(RunNotice::Finished { canvas: active.canvas });
            }
            !ended
        });
        if settled {
            refresh(session, &self.budget);
        }
        notices
    }

    fn start(&mut self, session: &mut Session, prepared: PreparedRun, notices: &mut Vec<RunNotice>) -> Result<(), Error> {
        if self.runs.len() >= MAX_RUNS {
            return Err(Error::new(ErrorKind::Busy, "too many runs are in flight; try again when one finishes"));
        }
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|_| Error::new(ErrorKind::Internal, "run identity entropy is unavailable"))?;
        let options = RunOptions { id, first_job: 1, limits: Default::default(), retry: self.retry };
        let (run, progress) = session.authorize_prepared_run(prepared, options)?;
        let canvas = run.canvas_id();
        let mailbox: Arc<Mutex<Mailbox>> = Arc::default();
        let sink = Arc::clone(&mailbox);
        let provider = ProviderRunner::new(
            Arc::clone(&self.runtime),
            self.limits,
            Arc::new(move |job, delivery| {
                lock(&sink).deliveries.insert(job, delivery);
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(self.budget.clone());
        let actions = Arc::new(LocalActions::new(session.assets().clone()));
        let sources = SourceRunner::default().with_actions(actions.clone());
        let runner = Arc::new(Either { provider: Arc::new(provider), sources, local: Mutex::default() });
        let (driver, handle) = driver(Scheduler::new(Default::default()));
        let mut active = Active { canvas, run, runner, actions, mailbox, handle: Some(handle), provider_jobs: BTreeSet::new(), live: BTreeMap::new() };
        // Queue before the driver runs, so a job that can't be prepared is never sent.
        let mut started = Vec::new();
        advance(session, &mut active, progress, &mut started, &self.runtime);
        notices.push(RunNotice::Started { canvas, nodes: active.live.values().copied().collect::<BTreeSet<_>>().into_iter().collect() });
        notices.extend(started);
        let events = Arc::clone(&active.mailbox);
        let ended = Arc::clone(&active.mailbox);
        let runner: Arc<dyn JobRunner> = active.runner.clone();
        let runtime = Arc::clone(&self.runtime);
        let task = driver.run(runner, Arc::clone(&self.runtime), move |event| lock(&events).events.push_back(event));
        runtime.spawn(Box::pin(async move {
            let _scheduler = task.await;
            lock(&ended).ended = true;
        }));
        self.runs.push(active);
        Ok(())
    }
}

/// Puts the budget the host charges against, and what it has spent, where quotes read them.
fn refresh(session: &mut Session, budget: &JobBudget) {
    let budgets: Budgets = lock(&budget.budgets).clone();
    let log = lock(&budget.log).clone();
    session.swap_resource(Some(BudgetState::from_usage(budgets, &log, &budget.session)));
    session.swap_resource(Some(UsageSnapshot(Arc::new(log))));
}

/// Reports what `progress` finished or failed (recording each finished step), then registers and
/// schedules its new jobs and cancels what it cancels. A job that can't be prepared (its media is
/// missing or too large) fails on its own before anything is sent, and what that unlocks is
/// handled in turn.
fn advance(session: &mut Session, active: &mut Active, progress: Progress, notices: &mut Vec<RunNotice>, runtime: &Arc<dyn Runtime>) {
    let canvas = active.canvas;
    let mut pending = VecDeque::from([progress]);
    while let Some(progress) = pending.pop_front() {
        for failure in progress.failed {
            notices.push(RunNotice::Failed { canvas, node: failure.node, reason: failure.reason });
        }
        for result in progress.finished {
            notices.push(record(session, canvas, result));
        }
        let Some(handle) = active.handle.as_ref() else { continue };
        if !progress.cancel.is_empty() {
            let _ = handle.cancel(CancelScope::Jobs(progress.cancel));
        }
        let handle = handle.clone();
        let mut ready = Vec::new();
        for spec in progress.jobs {
            match prepare(session, active, &spec) {
                Ok(()) => ready.push(spec),
                Err(reason) => {
                    active.live.remove(&spec.id);
                    pending.push_back(active.run.job_failed(spec.id, &reason));
                }
            }
        }
        if ready.is_empty() {
            continue;
        }
        let ids: Vec<JobId> = ready.iter().map(|spec| spec.id).collect();
        // `add` enqueues on construction; a refusal arrives later and fails those jobs in `pump`.
        let verdict = handle.add(ready);
        let mailbox = Arc::clone(&active.mailbox);
        runtime.spawn(Box::pin(async move {
            let reason = match verdict.await {
                Ok(Ok(())) => return,
                Ok(Err(error)) => format!("the run could not queue this job: {error}"),
                Err(_) => "the run stopped before this job was queued".to_owned(),
            };
            lock(&mailbox).rejected.extend(ids.into_iter().map(|job| (job, reason.clone())));
        }));
    }
}

/// Registers one job with the runner that will run it.
fn prepare(session: &Session, active: &mut Active, spec: &nodecraft_exec::scheduler::JobSpec) -> Result<(), String> {
    let work = active.run.work(spec.id).ok_or("the job has no work")?;
    match work {
        JobSpec::Generate { node, .. } => {
            active.live.insert(spec.id, *node);
            let queued = active.run.queued_job_with_media(spec.id, &spec.idempotency_key, session.assets()).map_err(|e| e.message)?;
            active.runner.provider.register(spec.id, queued);
            active.provider_jobs.insert(spec.id);
        }
        JobSpec::Source { node, .. } | JobSpec::Action { node, .. } => {
            active.live.insert(spec.id, *node);
            active.runner.sources.register(spec.id, work.clone());
            lock(&active.runner.local).insert(spec.id);
        }
    }
    Ok(())
}

/// Absorbs one run's messages; returns whether a paid job settled.
fn absorb(session: &mut Session, active: &mut Active, notices: &mut Vec<RunNotice>, runtime: &Arc<dyn Runtime>) -> bool {
    let (events, rejected) = {
        let mut mailbox = lock(&active.mailbox);
        (std::mem::take(&mut mailbox.events), std::mem::take(&mut mailbox.rejected))
    };
    let mut settled = false;
    for (job, reason) in rejected {
        settled |= finish(active, job);
        let progress = active.run.job_failed(job, &reason);
        advance(session, active, progress, notices, runtime);
    }
    for event in events {
        let progress = match event {
            Event::Done { job } => {
                let outputs = outputs(session, active, job);
                settled |= finish(active, job);
                match outputs {
                    Ok(outputs) => active.run.job_done(session.kinds(), job, outputs),
                    Err(reason) => active.run.job_failed(job, &reason),
                }
            }
            Event::Failed { job, error } => {
                settled |= finish(active, job);
                active.run.job_failed(job, &error)
            }
            Event::Cancelled { job } => {
                settled |= finish(active, job);
                active.run.job_failed(job, "cancelled")
            }
            Event::Skipped { job, .. } => {
                settled |= finish(active, job);
                active.run.job_failed(job, "skipped because an earlier job failed")
            }
            Event::Queued { .. } | Event::Started { .. } | Event::Retrying { .. } => continue,
        };
        advance(session, active, progress, notices, runtime);
    }
    if active.run.is_finished() {
        // Dropping the last handle lets the driver drain and return.
        active.handle = None;
    }
    settled
}

/// Lets go of a job that ended. A model job's reservation is settled (charged as unresolved when
/// it may have been billed, released when it provably wasn't); returns whether it was one.
fn finish(active: &mut Active, job: JobId) -> bool {
    active.live.remove(&job);
    lock(&active.runner.local).remove(&job);
    let provider = active.provider_jobs.contains(&job);
    if provider {
        active.runner.provider.forget(job);
    }
    provider
}

/// A finished job's outputs, with any assets it made imported into the project.
fn outputs(session: &mut Session, active: &Active, job: JobId) -> Result<Vec<(String, nodecraft_types::Value)>, String> {
    if !active.provider_jobs.contains(&job) {
        let staged = active.actions.take_staged();
        for (hash, bytes) in staged.iter() {
            let imported = session.import_asset(bytes).map_err(|e| e.message)?;
            if imported != *hash {
                return Err("an action's image changed while it was imported".into());
            }
        }
        return Ok(take_outputs(&active.runner.sources.outputs, job));
    }
    let delivery = lock(&active.mailbox).deliveries.remove(&job).ok_or("the model's result went missing")?;
    let Some(JobSpec::Generate { outputs: routes, .. }) = active.run.work(job) else { return Err("the job is not a model call".into()) };
    let produced = generate::produced(routes, &delivery.completed).map_err(|e| e.message)?;
    for (hash, bytes) in produced.assets {
        let imported = session.import_asset(&bytes).map_err(|e| e.message)?;
        if imported != hash {
            return Err("a generated asset changed while it was imported".into());
        }
    }
    Ok(produced.outputs)
}

/// Records one finished step on its card.
fn record(session: &mut Session, canvas: CanvasId, result: StepResult) -> RunNotice {
    let node = result.node;
    let params = json!({"canvas": canvas, "results": [{"node": node, "generation": result.generation}]});
    match session.dispatch("run.record", &params) {
        Ok(_) => RunNotice::Recorded { canvas, node },
        Err(error) => RunNotice::Failed { canvas, node, reason: format!("the result could not be saved: {}", error.message) },
    }
}
