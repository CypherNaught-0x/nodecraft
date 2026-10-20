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
//! Every queued job is also followed as a [`JobView`] ([`RunHost::jobs`]) for the queue panel,
//! and can be cancelled on its own ([`RunHost::cancel`]); see [`crate::run_jobs`].
//!
//! Not yet: the usage log and submitted handles aren't saved, so a run cut short by quitting is
//! not resumed and its open reservations are lost with the process; and media is read on the UI
//! thread when a job is queued.
use crate::Session;
use crate::action_exec::LocalActions;
use crate::commands::Services;
use crate::generate;
use crate::jobs::{CompletionAccounting, CompletionDelivery, JobBudget, ProviderRunner};
use crate::run_cmds::{AuthorizedRun, BudgetState, PreparedRun, UsageSnapshot};
use crate::run_jobs::{JobKey, JobState, JobView, MAX_RECENT, Reports, RunJobs};
use nodecraft_exec::cost::Budgets;
use nodecraft_exec::driver::{CancelToken, Handle, JobRunner, Runtime, TaskFuture, driver};
use nodecraft_exec::run::{Progress, RunOptions, SourceRunner, StepResult, take_outputs};
use nodecraft_exec::scheduler::{CancelScope, Event, JobId, Outcome, RetryPolicy, Scheduler, Start};
use nodecraft_nodes::JobSpec;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

/// Most runs the host holds at once, waiting or running; `run.start` beyond it is refused.
pub const MAX_RUNS: usize = 8;

/// Accepted runs not yet authorized, and how many are running. A waiting run is bound to the
/// document revision it was quoted at, so it must start on the next pump, before that pump records
/// anything: the bound counts running runs too, so nothing accepted ever waits behind one.
#[derive(Default)]
struct Inbox {
    waiting: VecDeque<PreparedRun>,
    running: usize,
}
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
    /// A paid result was kept but its charge couldn't be written to the usage log, so budgets
    /// under-count it. The rest of its run was stopped.
    Unaccounted { canvas: CanvasId, node: NodeId, reason: String },
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
    /// The provider's latest progress report for each job.
    reports: Reports,
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
    /// The run's number in start order, for [`JobKey`]s.
    seq: u64,
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
    /// Cancelled: nothing more is sent, and what the run unlocks fails instead.
    cancelled: bool,
    /// What the queue panel shows of its jobs.
    jobs: RunJobs,
}

/// See the module docs.
pub struct RunHost {
    inbox: Arc<Mutex<Inbox>>,
    runs: Vec<Active>,
    runtime: Arc<dyn Runtime>,
    budget: JobBudget,
    limits: nodecraft_providers::RunLimits,
    retry: RetryPolicy,
    /// Notices raised outside `pump`, delivered by the next one.
    held: Vec<RunNotice>,
    /// The next run's number.
    next_run: u64,
    /// Jobs that ended, oldest first, at most [`MAX_RECENT`].
    recent: VecDeque<JobView>,
}

impl RunHost {
    /// A host running jobs on `runtime`, charging paid jobs to `budget`.
    pub fn new(runtime: Arc<dyn Runtime>, budget: JobBudget) -> Self {
        Self {
            inbox: Arc::default(),
            runs: Vec::new(),
            runtime,
            budget,
            limits: nodecraft_providers::RunLimits::default(),
            retry: RetryPolicy::default(),
            held: Vec::new(),
            next_run: 1,
            recent: VecDeque::new(),
        }
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
            if inbox.waiting.len().saturating_add(inbox.running) >= MAX_RUNS {
                return Err(Error::new(ErrorKind::Busy, "too many runs are in progress; try again when one finishes"));
            }
            inbox.waiting.push_back(prepared);
            Ok(json!({"accepted": true}))
        })
    }

    /// Installs the budget resources quotes read, from the shared usage log.
    pub fn install(&self, session: &mut Session) {
        refresh(session, &self.budget);
    }

    /// Whether any run is waiting or running.
    pub fn is_busy(&self) -> bool {
        !self.runs.is_empty() || !lock(&self.inbox).waiting.is_empty()
    }

    /// The nodes with work queued or running, for "running" badges.
    pub fn running(&self) -> BTreeSet<(CanvasId, NodeId)> {
        self.runs.iter().flat_map(|active| active.live.values().map(move |node| (active.canvas, *node))).collect()
    }

    /// Every job: those that ended recently (oldest first), then those queued or running, in
    /// the order their runs started.
    pub fn jobs(&self) -> Vec<JobView> {
        self.recent.iter().cloned().chain(self.runs.iter().flat_map(|active| active.jobs.live.values().cloned())).collect()
    }

    /// Asks one job to stop. A queued job is dropped without being sent; a running one is asked to
    /// cancel, and may still finish (and be recorded) or be billed. What the run unlocks after it
    /// fails. Its end arrives through [`Self::pump`].
    pub fn cancel(&mut self, key: JobKey) -> Result<(), Error> {
        let gone = || Error::new(ErrorKind::NotFound, "that job has already ended");
        let active = self.runs.iter_mut().find(|active| active.seq == key.run).ok_or_else(gone)?;
        let view = active.jobs.live.get_mut(&key.job).ok_or_else(gone)?;
        if view.cancel_requested {
            return Ok(());
        }
        let handle = active.handle.as_ref().ok_or_else(gone)?;
        handle.cancel(CancelScope::Jobs(vec![key.job])).map_err(|_| gone())?;
        view.cancel_requested = true;
        Ok(())
    }

    /// Forgets the jobs that ended.
    pub fn clear_finished(&mut self) {
        self.recent.clear();
    }

    /// Stops every run: runs not yet started are dropped, queued jobs are cancelled, running ones
    /// are asked to cancel, and nothing they unlock is sent. A job that finishes anyway is still
    /// recorded. The notices arrive through [`Self::pump`].
    pub fn cancel_all(&mut self) {
        let waiting = std::mem::take(&mut lock(&self.inbox).waiting);
        self.held.extend(waiting.into_iter().map(|_| RunNotice::Refused { reason: "cancelled before it started".into() }));
        for active in &mut self.runs {
            stop(active);
            for view in active.jobs.live.values_mut() {
                view.cancel_requested = true;
            }
        }
    }

    /// Starts accepted runs and absorbs finished work. UI thread; never blocks on a provider.
    pub fn pump(&mut self, session: &mut Session) -> Vec<RunNotice> {
        let mut notices = std::mem::take(&mut self.held);
        // Every accepted run starts now, before this pump records anything (see `Inbox`).
        loop {
            let next = lock(&self.inbox).waiting.pop_front();
            let Some(prepared) = next else { break };
            if let Err(error) = self.start(session, prepared, &mut notices) {
                notices.push(RunNotice::Refused { reason: error.message });
            }
        }
        let mut settled = false;
        let recent = &mut self.recent;
        self.runs.retain_mut(|active| {
            // Read first: everything the driver reported before it returned is then absorbed.
            let ended = lock(&active.mailbox).ended;
            settled |= absorb(session, active, &mut notices, &self.runtime, &self.budget);
            keep_recent(recent, active.jobs.ended.drain(..));
            if ended {
                notices.push(RunNotice::Finished { canvas: active.canvas });
            }
            !ended
        });
        lock(&self.inbox).running = self.runs.len();
        if settled {
            refresh(session, &self.budget);
        }
        notices
    }

    fn start(&mut self, session: &mut Session, prepared: PreparedRun, notices: &mut Vec<RunNotice>) -> Result<(), Error> {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|_| Error::new(ErrorKind::Internal, "run identity entropy is unavailable"))?;
        let options = RunOptions { id, first_job: 1, limits: Default::default(), retry: self.retry };
        let (run, progress) = session.authorize_prepared_run(prepared, options)?;
        let canvas = run.canvas_id();
        let mailbox: Arc<Mutex<Mailbox>> = Arc::default();
        let sink = Arc::clone(&mailbox);
        let reports = Arc::clone(&mailbox);
        let provider = ProviderRunner::new(
            Arc::clone(&self.runtime),
            self.limits,
            Arc::new(move |job, delivery| {
                lock(&sink).deliveries.insert(job, delivery);
            }),
            Arc::new(move |job, _, update| lock(&reports).reports.note(job, &update)),
        )
        .with_budget(self.budget.clone());
        let actions = Arc::new(LocalActions::new(session.assets().clone()));
        let sources = SourceRunner::default().with_actions(actions.clone());
        let runner = Arc::new(Either { provider: Arc::new(provider), sources, local: Mutex::default() });
        let (driver, handle) = driver(Scheduler::new(Default::default()));
        let seq = self.next_run;
        self.next_run = self.next_run.saturating_add(1);
        let mut active = Active {
            seq,
            canvas,
            run,
            runner,
            actions,
            mailbox,
            handle: Some(handle),
            provider_jobs: BTreeSet::new(),
            live: BTreeMap::new(),
            cancelled: false,
            jobs: RunJobs::default(),
        };
        // Queue before the driver runs, so a job that can't be prepared is never sent.
        let mut started = Vec::new();
        advance(session, &mut active, progress, &mut started, &self.runtime);
        notices.push(RunNotice::Started { canvas, nodes: active.live.values().copied().collect::<BTreeSet<_>>().into_iter().collect() });
        notices.extend(started);
        keep_recent(&mut self.recent, active.jobs.ended.drain(..));
        let events = Arc::clone(&active.mailbox);
        let ended = Ended(Arc::clone(&active.mailbox));
        let runner: Arc<dyn JobRunner> = active.runner.clone();
        let runtime = Arc::clone(&self.runtime);
        let task = driver.run(runner, Arc::clone(&self.runtime), move |event| lock(&events).events.push_back(event));
        runtime.spawn(Box::pin(async move {
            let _ended = ended;
            let _scheduler = task.await;
        }));
        self.runs.push(active);
        Ok(())
    }
}

/// Marks the run's driver as returned when dropped: when the task ends, or when a runtime that is
/// shutting down drops it unfinished.
struct Ended(Arc<Mutex<Mailbox>>);
impl Drop for Ended {
    fn drop(&mut self) {
        lock(&self.0).ended = true;
    }
}

/// Adds jobs that ended to `recent`, dropping the oldest past [`MAX_RECENT`].
fn keep_recent(recent: &mut VecDeque<JobView>, ended: impl Iterator<Item = JobView>) {
    for view in ended {
        if recent.len() >= MAX_RECENT {
            recent.pop_front();
        }
        recent.push_back(view);
    }
}

/// Cancels everything `active` has queued and stops it sending more.
fn stop(active: &mut Active) {
    active.cancelled = true;
    if let Some(handle) = &active.handle {
        // Control commands have reserved capacity; a closed driver has nothing left to cancel.
        let _ = handle.cancel(CancelScope::All);
    }
}

/// Puts the budget the host charges against, and what it has spent, where quotes read them.
fn refresh(session: &mut Session, budget: &JobBudget) {
    let budgets: Budgets = lock(&budget.budgets).clone();
    let log = Arc::new(lock(&budget.log).clone());
    session.swap_resource(Some(BudgetState::from_usage(budgets, &log, &budget.session)));
    session.swap_resource(Some(UsageSnapshot(log)));
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
        if active.cancelled {
            for spec in progress.jobs {
                pending.push_back(active.run.job_failed(spec.id, "cancelled"));
            }
            continue;
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
                    active.jobs.end(spec.id, JobState::Failed { reason: reason.clone(), cause: None });
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
                Err(error) => format!("the run could not queue this job: {error}"),
            };
            lock(&mailbox).rejected.extend(ids.into_iter().map(|job| (job, reason.clone())));
        }));
    }
}

/// Registers one job with the runner that will run it.
fn prepare(session: &Session, active: &mut Active, spec: &nodecraft_exec::scheduler::JobSpec) -> Result<(), String> {
    let work = active.run.work(spec.id).ok_or("the job has no work")?;
    let (run, canvas) = (active.seq, active.canvas);
    let view = |node: NodeId| JobView {
        key: JobKey { run, job: spec.id },
        canvas,
        node,
        provider: None,
        model: None,
        estimate: None,
        state: JobState::Queued,
        sent: false,
        cancel_requested: false,
    };
    match work {
        JobSpec::Generate { node, .. } => {
            let node = *node;
            active.live.insert(spec.id, node);
            active.jobs.live.insert(spec.id, view(node));
            let queued = active.run.queued_job_with_media(spec.id, &spec.idempotency_key, session.assets()).map_err(|e| e.message)?;
            if let Some(view) = active.jobs.live.get_mut(&spec.id) {
                view.provider = Some(queued.provider.target().adapter().to_owned());
                view.model = Some(queued.job.model.clone());
                view.estimate = queued.charged_price().and_then(|price| price.estimate);
            }
            active.runner.provider.register(spec.id, queued);
            active.provider_jobs.insert(spec.id);
        }
        JobSpec::Source { node, .. } | JobSpec::Action { node, .. } => {
            let node = *node;
            active.live.insert(spec.id, node);
            active.jobs.live.insert(spec.id, view(node));
            active.runner.sources.register(spec.id, work.clone());
            lock(&active.runner.local).insert(spec.id);
        }
    }
    Ok(())
}

/// Absorbs one run's messages; returns whether a paid job settled.
fn absorb(session: &mut Session, active: &mut Active, notices: &mut Vec<RunNotice>, runtime: &Arc<dyn Runtime>, budget: &JobBudget) -> bool {
    let (events, rejected, reports) = {
        let mut mailbox = lock(&active.mailbox);
        (std::mem::take(&mut mailbox.events), std::mem::take(&mut mailbox.rejected), std::mem::take(&mut mailbox.reports))
    };
    // Before the events, so a job cancelled this frame knows whether the provider had it; again
    // after, so a job that started this frame shows its progress.
    active.jobs.absorb_reports(&reports);
    let mut settled = false;
    for (job, reason) in rejected {
        active.jobs.end(job, JobState::Failed { reason: reason.clone(), cause: None });
        settled |= finish(active, job);
        let progress = active.run.job_failed(job, &reason);
        advance(session, active, progress, notices, runtime);
    }
    for event in events {
        let progress = match event {
            Event::Done { job } => {
                let outputs = outputs(session, active, job);
                settled |= finish(active, job);
                active.jobs.end(job, ended(&outputs));
                match outputs {
                    Ok(outputs) => active.run.job_done(session.kinds(), job, outputs),
                    Err(reason) => active.run.job_failed(job, &reason),
                }
            }
            Event::Failed { job, error } => match unaccounted(active, job, budget) {
                // The provider finished and billed; only writing the charge failed.
                Some(charge) => {
                    let node = active.live.get(&job).copied();
                    let outputs = outputs(session, active, job);
                    settled |= finish(active, job);
                    active.jobs.end(job, ended(&outputs));
                    if let Err(reason) = charge {
                        notices.extend(node.map(|node| RunNotice::Unaccounted { canvas: active.canvas, node, reason }));
                        stop(active);
                    }
                    match outputs {
                        Ok(outputs) => active.run.job_done(session.kinds(), job, outputs),
                        Err(reason) => active.run.job_failed(job, &reason),
                    }
                }
                None => {
                    settled |= finish(active, job);
                    active.jobs.end(job, JobState::Failed { reason: error.clone(), cause: None });
                    active.run.job_failed(job, &error)
                }
            },
            Event::Cancelled { job } => {
                settled |= finish(active, job);
                active.jobs.end(job, JobState::Cancelled);
                active.run.job_failed(job, "cancelled")
            }
            Event::Skipped { job, .. } => {
                settled |= finish(active, job);
                let reason = "skipped because an earlier job failed";
                active.jobs.end(job, JobState::Failed { reason: reason.into(), cause: None });
                active.run.job_failed(job, reason)
            }
            Event::Started { job, attempt } => {
                active.jobs.started(job, attempt);
                continue;
            }
            Event::Retrying { job, next_attempt, error, .. } => {
                active.jobs.retrying(job, next_attempt, &error);
                continue;
            }
            Event::Queued { .. } => continue,
        };
        advance(session, active, progress, notices, runtime);
    }
    active.jobs.absorb_reports(&reports);
    if active.run.is_finished() {
        // Dropping the last handle lets the driver drain and return.
        active.handle = None;
    }
    settled
}

/// How a finished job ended for the panel: done, or failed when its outputs couldn't be kept.
fn ended<T>(outputs: &Result<T, String>) -> JobState {
    match outputs {
        Ok(_) => JobState::Done,
        Err(reason) => JobState::Failed { reason: reason.clone(), cause: None },
    }
}

/// For a model job whose output arrived but whose charge failed to write: retries the write once
/// and says whether it took. `None` for every other failure.
fn unaccounted(active: &Active, job: JobId, budget: &JobBudget) -> Option<Result<(), String>> {
    let mut mailbox = lock(&active.mailbox);
    let delivery = mailbox.deliveries.get_mut(&job)?;
    let CompletionAccounting::Failed(failure) = &mut delivery.accounting else { return None };
    let charged = match failure.retry.as_mut() {
        Some(retry) => retry.retry(budget).map_err(|error| format!("the charge couldn't be recorded: {error:?}")),
        None => Err(format!("the charge couldn't be recorded: {}", failure.message)),
    };
    Some(charged)
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
        // Staged images stay staged: a later action in the run may read them (chains).
        let staged = active.actions.staged();
        for (hash, bytes) in staged.iter() {
            if session.assets().contains(hash) {
                continue;
            }
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
