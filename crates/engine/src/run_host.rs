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
//! and can be cancelled on its own ([`RunHost::cancel`]); see [`crate::run_jobs`]. A text job's
//! answer streams into its view as it arrives (#504); what a cancelled or failed one streamed can
//! be kept on its card ([`RunHost::keep_partial`]).
//!
//! With a journal ([`RunHost::open_journal`], #508) the usage log and the handle of every paid
//! job the provider accepted are saved at the end of each pump that changed them. After a
//! restart the saved jobs are followed again ([`RunHost::resume`]), polled and never sent again;
//! each one's charge settles when it ends, and a finished one is recorded on its card when its
//! step was its alone. A reservation left open with no handle (its submit may or may not have
//! reached the provider) is charged as unresolved. Quitting with a journal suspends what is
//! running rather than cancelling it ([`RunHost::shut_down`]).
//!
//! Not yet: media is read on the UI thread when a job is queued.
use crate::Session;
use crate::action_exec::LocalActions;
use crate::commands::Services;
use crate::generate;
use crate::jobs::{CompletionAccounting, CompletionDelivery, JobBudget, ProviderRunner, Sent, Stage, WriteAhead};
use crate::ownership_cmds::{self, OWNERSHIP_SERVICE};
use crate::provider_routing::RegisteredProvider;
use crate::resolve_cmds::{self, RESOLUTION_SERVICE};
use crate::run_cmds::{AuthorizedRun, BudgetState, PreparedRun, UsageSnapshot};
use crate::run_jobs::{JobKey, JobState, JobView, MAX_RECENT, Reports, RunJobs};
use crate::run_journal::{JOURNAL_VERSION, Journal, JournalStore, MAX_PARKED_LAUNCHES, Submitted};
use nodecraft_exec::cost::{Budgets, Charge, UsageLog};
use nodecraft_exec::driver::{CancelToken, Handle, JobRunner, Runtime, TaskFuture, driver};
use nodecraft_exec::run::{Progress, RunOptions, SourceRunner, StepResult, take_outputs};
use nodecraft_exec::scheduler::{CancelScope, Event, JobId, Outcome, RetryPolicy, Scheduler, Start};
use nodecraft_nodes::{GenerateOutput, JobSpec};
use nodecraft_providers::{Completed, JobHandle, RunUpdate, StagedSubmit};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

mod estimates;
mod held;
mod partial;
mod resume;

pub use crate::held_cmds::{HELD_SERVICE, MAX_HELD};
pub use held::{HeldAction, HeldView};

/// Added to the reason a paid result couldn't be kept, once it is held (#369).
const HELD_NOTE: &str = "the paid result is held: retry it, put it on the canvas or discard it in the queue panel";

/// How long after a failed journal save the next one is tried.
const SAVE_RETRY_MS: u64 = 5_000;
/// How often a pump with work in flight compares the usage log with the saved one, for changes
/// no job's end announced. Ends and new handles are saved at once.
const SAVE_CHECK_MS: u64 = 2_000;

/// The reason a cancelled step's job fails with; reported as [`RunNotice::Cancelled`].
const CANCELLED: &str = "cancelled";

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

/// Applies a person's decision to the usage log: tried on a copy and saved in the journal
/// first, then applied to the live log in place, never replaced, since running jobs hold
/// reservations tied to its identity. A failed save leaves both as they were. `guard` refuses
/// the decision under the journal lock, before anything is tried.
fn keep_decision(
    shared: &Mutex<Shared>,
    log: &Mutex<UsageLog>,
    at_ms: u64,
    guard: impl FnOnce(&Shared) -> Result<(), Error>,
    apply: impl Fn(&mut UsageLog, u64) -> Result<serde_json::Value, Error>,
) -> Result<serde_json::Value, Error> {
    // The write-ahead's order: the journal, then the log.
    let mut shared = lock(shared);
    guard(&shared)?;
    let mut live = lock(log);
    let mut next = live.clone();
    apply(&mut next, at_ms)?;
    if let Some(store) = &shared.store {
        let journal = Journal { version: JOURNAL_VERSION, usage: next, submitted: shared.submitted.clone() };
        store
            .save(&journal)
            .map_err(|error| Error::new(error.kind, format!("the decision wasn't kept, because the usage log couldn't be saved: {}", error.message)))?;
    }
    let kept = apply(&mut live, at_ms)
        .map_err(|error| Error::new(ErrorKind::Internal, format!("the saved decision couldn't be applied to the live usage log: {}", error.message)))?;
    shared.reconciled = true;
    Ok(kept)
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
    /// A step was cancelled before it produced anything.
    Cancelled { canvas: CanvasId, node: NodeId },
    /// A run could not start (authorization or queueing refused it); nothing was sent.
    Refused { reason: String },
    /// A paid result was kept but its charge couldn't be written to the usage log, so budgets
    /// under-count it. The rest of its run was stopped.
    Unaccounted { canvas: CanvasId, node: NodeId, reason: String },
    /// A held result (#369) was put on its canvas as source nodes.
    Placed { canvas: CanvasId, node: NodeId },
    /// Every step of a run has finished or failed.
    Finished { canvas: CanvasId },
    /// The run journal couldn't be read or saved, so what is running may not be followed again
    /// after a restart. Raised once until a save succeeds again.
    Unsaved { reason: String },
    /// Providers answered the `run.estimate` that returned `request` (#663): `quoted` jobs have a
    /// price, `unquoted` ones stay priced only while they run. Quote the run again to see it.
    Estimated { request: u64, canvas: CanvasId, quoted: usize, unquoted: usize },
    /// The `run.estimate` that returned `request` couldn't ask anything, and why.
    NotEstimated { request: u64, reason: String },
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
    /// Handles the provider issued: (job, scheduler key, generation, handle).
    submitted: Vec<(JobId, String, u32, JobHandle)>,
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
    /// The granted plugins' actions the run started with (#173).
    plugins: Option<Arc<crate::plugin_runtime::PluginSnapshot>>,
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
    /// What each queued model job is saved as once the provider accepts it, less its handle;
    /// shared with the runner's stage hook, which saves it with its staged request (#664).
    templates: Arc<Mutex<BTreeMap<JobId, Submitted>>>,
    /// The host's journal, where this run's staged requests are kept until they get a handle.
    shared: Arc<Mutex<Shared>>,
    /// Model jobs the provider accepted and that haven't ended, as saved.
    journal: BTreeMap<JobId, Submitted>,
    /// The paid result of each model job converted and not yet used, until its step is recorded
    /// or ends: held if recording it is refused or its step can't use it (#369, #611).
    awaiting: BTreeMap<JobId, Awaiting>,
    /// `journal` changed since the last save.
    journal_changed: bool,
}

/// A model job's paid result waiting for its step.
struct Awaiting {
    step: NodeId,
    result: held::HeldResult,
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
    /// Where the journal is saved, shared with the runners' write-ahead.
    journal: Arc<Mutex<Shared>>,
    /// Restored jobs waiting for their provider, or for their project to be open.
    parked: Vec<Submitted>,
    /// Restored jobs whose provider couldn't be reached this session; followed next launch.
    kept: Vec<Submitted>,
    /// Restored jobs being followed.
    resumes: resume::Resumes,
    /// The usage log as last saved.
    saved_log: Option<UsageLog>,
    /// The saved journal is out of date.
    journal_changed: bool,
    /// When the last save failed, and whether that was reported.
    save_failed: Option<(u64, bool)>,
    /// When the usage log was last compared with the saved one.
    checked_at: u64,
    /// Handles of jobs that may have been billed though they failed, for a later run that sends
    /// them again (#517).
    kept_handles: Arc<Mutex<crate::jobs::KeptHandles>>,
    /// Paid results that couldn't be kept, waiting for a decision (#369).
    results: Arc<Mutex<held::Held>>,
    /// ffmpeg and ffprobe for timeline renders (#109), from the settings.
    video_tools: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// `run.estimate` requests (#663).
    estimates: estimates::Estimates,
}

/// The journal's store and the submitted jobs as last saved, for the write-ahead: a runner saves
/// the usage log with its new estimate before sending a paid job, keeping the jobs already saved.
#[derive(Default)]
struct Shared {
    /// `None` saves nothing.
    store: Option<Box<dyn JournalStore>>,
    submitted: Vec<Submitted>,
    /// Model jobs whose request was staged and may have been sent, with no handle back yet, by
    /// run and job (#664). Saved with `submitted`, and saved at once each time one is staged.
    staging: BTreeMap<(u64, JobId), Submitted>,
    /// The host is shutting down: nothing new is sent.
    closing: bool,
    /// An ownership decision changed the usage log since the budget resources were refreshed.
    reconciled: bool,
}

impl Shared {
    /// What a save made now writes: the jobs as last saved, and every staged one.
    fn saved(&self) -> Vec<Submitted> {
        self.submitted.iter().chain(self.staging.values()).cloned().collect()
    }
}

impl RunHost {
    /// A host running jobs on `runtime`, charging paid jobs to `budget`.
    pub fn new(runtime: Arc<dyn Runtime>, budget: JobBudget) -> Self {
        Self {
            inbox: Arc::default(),
            runs: Vec::new(),
            runtime,
            budget,
            // Text answers stream into their cards as they generate (#504).
            limits: nodecraft_providers::RunLimits { stream_text: true, ..Default::default() },
            retry: RetryPolicy::default(),
            held: Vec::new(),
            next_run: 1,
            recent: VecDeque::new(),
            journal: Arc::default(),
            parked: Vec::new(),
            kept: Vec::new(),
            resumes: resume::Resumes::default(),
            saved_log: None,
            journal_changed: false,
            save_failed: None,
            checked_at: 0,
            kept_handles: Arc::default(),
            results: Arc::default(),
            video_tools: None,
            estimates: estimates::Estimates::default(),
        }
    }

    /// The ffmpeg and ffprobe later runs render timelines with (`[media]` in the settings,
    /// through `MediaSettings::tools`); `None` makes timeline renders fail with a message
    /// saying so. Runs already going keep the tools they started with.
    pub fn set_video_tools(&mut self, tools: Option<(&std::path::Path, &std::path::Path)>) {
        self.video_tools = tools.map(|(ffmpeg, ffprobe)| (ffmpeg.to_path_buf(), ffprobe.to_path_buf()));
    }

    /// Keeps the usage log and submitted jobs in `store` (see the module docs), restoring what it
    /// holds: its usage log replaces the budget's, an estimate left open without a saved handle
    /// is charged as unresolved, and its submitted jobs wait for [`Self::resume`]. Call once,
    /// before [`Self::install`] and the first run. A journal that can't be read is moved aside
    /// and the error returned; the host then starts a new one in `store`.
    pub fn open_journal(&mut self, store: Box<dyn JournalStore>) -> Result<(), Error> {
        let loaded = store.load();
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                let aside = store.set_aside();
                self.journal_changed = true;
                return Err(match aside {
                    Ok(()) => {
                        lock(&self.journal).store = Some(store);
                        error
                    }
                    // Saving would replace it; keep it, and save nothing this session.
                    Err(aside) => Error::new(error.kind, format!("{}; {}", error.message, aside.message)),
                });
            }
        };
        lock(&self.journal).store = Some(store);
        let Some(journal) = loaded else { return Ok(()) };
        let mut log = journal.usage;
        let handled: BTreeSet<(String, Option<nodecraft_types::ProviderInstanceId>, String)> =
            journal.submitted.iter().map(|entry| (entry.target.adapter().to_owned(), entry.target.instance(), entry.sent_key())).collect();
        let orphans: Vec<_> = log
            .entries()
            .chain(log.pending())
            .filter(|entry| entry.status == Charge::Estimate)
            .filter(|entry| !handled.contains(&(entry.provider.clone(), entry.instance, entry.idempotency_key.clone())))
            .map(|entry| (entry.provider.clone(), entry.instance, entry.idempotency_key.clone(), entry.cost))
            .collect();
        for (provider, instance, key, cost) in orphans {
            let settled =
                nodecraft_types::ProviderTarget::new(&provider, instance).and_then(|target| log.settle_target(&target, &key, cost, Charge::Unresolved));
            // An entry that can't be settled stays an open estimate, which caps still count.
            let _ = settled;
        }
        *lock(&self.budget.log) = log;
        for mut entry in journal.submitted {
            entry.launches = entry.launches.saturating_add(1);
            if entry.launches <= MAX_PARKED_LAUNCHES {
                self.parked.push(entry);
                continue;
            }
            // Its project hasn't been open for many launches: stop holding its estimate open.
            settle_unresolved(&self.budget, &entry);
            self.held.push(RunNotice::Failed {
                canvas: entry.canvas,
                node: entry.node,
                reason: "it was given up on after a restart, because its project wasn't opened again; it may have been charged".into(),
            });
        }
        // What the journal follows until the next save: a decision kept before then saves these
        // runs again rather than dropping them.
        lock(&self.journal).submitted = self.parked.clone();
        self.journal_changed = true;
        Ok(())
    }

    /// Keeps held results (#369) in `store` across restarts (#614), holding again every result
    /// it saved. A record that can't be read is set aside and reported; the store is used anyway.
    /// An error that leaves the store unused says so. Call before any run starts.
    pub fn open_held(&mut self, store: Box<dyn crate::held_store::HeldStore>) -> Result<(), Error> {
        lock(&self.results).open(store)
    }

    /// The adapters restored jobs of `project` are waiting for (see [`Self::resume`]).
    pub fn waiting_adapters(&self, project: ProjectId) -> BTreeSet<String> {
        self.parked.iter().filter(|entry| entry.project == project).map(|entry| entry.target.adapter().to_owned()).collect()
    }

    /// Follows the restored jobs of the open project that ran on `binding`'s endpoint. A job that
    /// can't be followed is reported and its charge settled as unresolved.
    pub fn resume(&mut self, session: &Session, binding: &RegisteredProvider) {
        let project = session.document().id();
        let (ready, waiting): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.parked).into_iter().partition(|entry| entry.project == project && entry.target == *binding.target());
        self.parked = waiting;
        for entry in ready {
            if let Err(refused) = self.resumes.start(entry, binding, &self.runtime, &self.budget) {
                let (entry, reason) = *refused;
                settle_unresolved(&self.budget, &entry);
                self.held.push(RunNotice::Failed {
                    canvas: entry.canvas,
                    node: entry.node,
                    reason: format!("it couldn't be followed after a restart: {reason}"),
                });
                self.journal_changed = true;
            }
        }
    }

    /// Whether restored jobs are waiting for [`Self::resume`].
    pub fn has_waiting(&self) -> bool {
        !self.parked.is_empty()
    }

    /// Lets go of the host when the app quits. With a journal, what is running is saved and
    /// left running at the provider, to be followed next launch; without one, it is cancelled.
    /// Jobs not yet sent are refused from here on, so none can be sent after the last save.
    pub fn shut_down(&mut self) {
        for run in &self.runs {
            run.actions.cancel_captures();
        }
        let journaled = {
            let mut shared = lock(&self.journal);
            shared.closing = true;
            shared.store.is_some()
        };
        if journaled {
            self.runs.iter_mut().for_each(note_submitted);
            self.save(true);
        } else {
            self.cancel_all();
        }
    }

    /// Saves the journal if it changed (or `force`), no more often than [`SAVE_RETRY_MS`] after a
    /// failure. Returns a notice the first time a save fails.
    fn save(&mut self, force: bool) -> Option<RunNotice> {
        let mut shared = lock(&self.journal);
        let store = shared.store.as_ref()?;
        let now = self.runtime.now_ms();
        if !force
            && let Some((at, _)) = self.save_failed
            && now.saturating_sub(at) < SAVE_RETRY_MS
        {
            return None;
        }
        let usage = lock(&self.budget.log).clone();
        if !force && !self.journal_changed && self.saved_log.as_ref() == Some(&usage) {
            return None;
        }
        let submitted: Vec<Submitted> = self
            .parked
            .iter()
            .chain(&self.kept)
            .chain(self.resumes.live.values().map(|(entry, _)| entry))
            .chain(self.runs.iter().flat_map(|active| active.journal.values()))
            .cloned()
            .collect();
        let handled = submitted.len();
        // A staged job that got its handle is saved by its handle, and its staged copy dropped
        // only once that is on disk: a save in between still has it staged.
        let followed: Vec<(u64, JobId)> = self.runs.iter().flat_map(|active| active.journal.keys().map(|job| (active.seq, *job))).collect();
        let staged = shared.staging.iter().filter(|(at, _)| !followed.contains(at)).map(|(_, entry)| entry.clone());
        let submitted = submitted.into_iter().chain(staged).collect();
        let journal = Journal { version: JOURNAL_VERSION, usage, submitted };
        match store.save(&journal) {
            Ok(()) => {
                let mut submitted = journal.submitted;
                submitted.truncate(handled);
                shared.submitted = submitted;
                for at in &followed {
                    shared.staging.remove(at);
                }
                self.saved_log = Some(journal.usage);
                self.journal_changed = false;
                self.save_failed = None;
                None
            }
            Err(error) => {
                let reported = self.save_failed.is_some_and(|(_, reported)| reported);
                self.save_failed = Some((now, true));
                (!reported).then_some(RunNotice::Unsaved { reason: error.message })
            }
        }
    }

    /// The runners' write-ahead: saves the usage log, with the jobs as last saved, before a paid
    /// job is sent; refuses to send any job once the host is shutting down, or while
    /// [`MAX_HELD`] results are held (#369), so a held result is never dropped for room.
    fn write_ahead(&self) -> Arc<WriteAhead> {
        let shared = Arc::clone(&self.journal);
        let log = Arc::clone(&self.budget.log);
        let results = Arc::clone(&self.results);
        Arc::new(move |priced| {
            if lock(&results).is_full() {
                return Err(format!("it wasn't sent: {MAX_HELD} paid results are waiting in the queue panel; retry, place or discard them first"));
            }
            let shared = lock(&shared);
            if shared.closing {
                return Err("NodeCraft was quitting, so it wasn't sent".into());
            }
            if !priced {
                return Ok(());
            }
            let Some(store) = &shared.store else { return Ok(()) };
            let journal = Journal { version: JOURNAL_VERSION, usage: lock(&log).clone(), submitted: shared.saved() };
            store.save(&journal).map_err(|error| format!("it wasn't sent, because its charge couldn't be saved: {}", error.message))
        })
    }

    /// Run `seq`'s stage hook (#664): saves a model job with the request its adapter staged,
    /// before the request is sent, so a restart sends the same bytes again under its key.
    fn stage(&self, seq: u64, templates: &Arc<Mutex<BTreeMap<JobId, Submitted>>>) -> Arc<Stage> {
        let shared = Arc::clone(&self.journal);
        let log = Arc::clone(&self.budget.log);
        let templates = Arc::clone(templates);
        Arc::new(move |job, sent: Sent<'_>, staged: &StagedSubmit| {
            let Some(template) = lock(&templates).get(&job).cloned() else {
                return Err("it wasn't sent, because NodeCraft couldn't save it first".into());
            };
            let mut shared = lock(&shared);
            if shared.closing {
                return Err("NodeCraft was quitting, so it wasn't sent".into());
            }
            let entry = Submitted { key: sent.base.to_owned(), generation: sent.generation, staged: Some(staged.clone()), ..template };
            shared.staging.insert((seq, job), entry);
            let Some(store) = &shared.store else { return Ok(()) };
            let journal = Journal { version: JOURNAL_VERSION, usage: lock(&log).clone(), submitted: shared.saved() };
            if let Err(error) = store.save(&journal) {
                shared.staging.remove(&(seq, job));
                return Err(format!("it wasn't sent, because it couldn't be saved first: {}", error.message));
            }
            Ok(())
        })
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
        })?;
        held::register(&self.results, services)
    }

    /// Registers the `run.estimate` service (#663); call once, before the session is built, on a
    /// host that pumps while no run is going (the next [`Self::pump`] asks the providers).
    pub fn register_estimates(&self, services: &mut Services) -> Result<(), Error> {
        self.estimates.register(services)
    }

    /// The paid results waiting for a decision (#369), oldest first.
    pub fn held_results(&self) -> Vec<HeldView> {
        lock(&self.results).views()
    }

    /// Registers the `usage.reconcile_ownership` service (#362) over the shared usage log. A
    /// decision is applied to a copy of the log and saved in the journal before the live log
    /// takes it, so a save that fails leaves both as they were and the decision can be confirmed
    /// again. The next [`Self::pump`] (or [`Self::sync_usage`]) refreshes the snapshot reviews
    /// read; until then a review of the changed group is refused as stale.
    pub fn register_ownership(&self, services: &mut Services) -> Result<(), Error> {
        let shared = Arc::clone(&self.journal);
        let log = Arc::clone(&self.budget.log);
        let wall_clock_ms = Arc::clone(&self.budget.wall_clock_ms);
        services.register(OWNERSHIP_SERVICE, move |request| {
            keep_decision(&shared, &log, wall_clock_ms(), |_| Ok(()), |usage, at_ms| ownership_cmds::reconcile(usage, request, at_ms))
        })
    }

    /// Provides `usage.resolve_charge` over the shared usage log, saved and kept exactly as
    /// [`Self::register_ownership`] keeps an ownership decision. A charge a saved run still
    /// follows is refused: that run settles it when it resumes.
    pub fn register_resolution(&self, services: &mut Services) -> Result<(), Error> {
        let shared = Arc::clone(&self.journal);
        let log = Arc::clone(&self.budget.log);
        let wall_clock_ms = Arc::clone(&self.budget.wall_clock_ms);
        services.register(RESOLUTION_SERVICE, move |request| {
            let (provider, key) = resolve_cmds::requested_charge(request)?;
            // Checked under the same journal lock the decision is saved under.
            let unfollowed = |shared: &Shared| {
                if shared.submitted.iter().any(|s| s.target.adapter() == provider && s.sent_key() == key) {
                    return Err(Error::new(ErrorKind::Conflict, "a saved run still follows this charge; it settles when the run resumes"));
                }
                Ok(())
            };
            keep_decision(&shared, &log, wall_clock_ms(), unfollowed, |usage, at_ms| resolve_cmds::resolve(usage, request, at_ms))
        })
    }

    /// Refreshes the budget resources after an ownership decision changed the usage log.
    pub fn sync_usage(&self, session: &mut Session) {
        if std::mem::take(&mut lock(&self.journal).reconciled) {
            refresh(session, &self.budget);
        }
    }

    /// Installs the budget resources quotes read, from the shared usage log.
    pub fn install(&self, session: &mut Session) {
        refresh(session, &self.budget);
    }

    /// Whether any run is waiting or running.
    pub fn is_busy(&self) -> bool {
        !self.runs.is_empty() || !self.resumes.live.is_empty() || !lock(&self.inbox).waiting.is_empty()
    }

    /// Whether a `run.estimate` is waiting or being answered.
    pub fn is_estimating(&self) -> bool {
        self.estimates.is_busy()
    }

    /// The nodes with work queued or running, for "running" badges.
    pub fn running(&self) -> BTreeSet<(CanvasId, NodeId)> {
        let resumed = self.resumes.live.values().map(|(entry, _)| (entry.canvas, entry.node));
        self.runs.iter().flat_map(|active| active.live.values().map(move |node| (active.canvas, *node))).chain(resumed).collect()
    }

    /// Every job: those that ended recently (oldest first), then those queued or running, in
    /// the order their runs started.
    pub fn jobs(&self) -> Vec<JobView> {
        let resumed = self.resumes.jobs.live.values().cloned();
        self.recent.iter().cloned().chain(resumed).chain(self.runs.iter().flat_map(|active| active.jobs.live.values().cloned())).collect()
    }

    /// Asks one job to stop. A queued job is dropped without being sent; a running one is asked to
    /// cancel, and may still finish (and be recorded) or be billed. What the run unlocks after it
    /// fails. Its end arrives through [`Self::pump`].
    pub fn cancel(&mut self, key: JobKey) -> Result<(), Error> {
        let gone = || Error::new(ErrorKind::NotFound, "that job has already ended");
        if key.run == resume::RESUMED_RUN {
            return if self.resumes.cancel(key.job) { Ok(()) } else { Err(gone()) };
        }
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
        let resumed: Vec<JobId> = self.resumes.live.keys().copied().collect();
        for job in resumed {
            self.resumes.cancel(job);
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
        let mut journal_changed = false;
        let recent = &mut self.recent;
        self.runs.retain_mut(|active| {
            // Read first: everything the driver reported before it returned is then absorbed.
            let ended = lock(&active.mailbox).ended;
            settled |= absorb(session, active, &mut notices, &self.runtime, &self.budget, &self.results);
            journal_changed |= std::mem::take(&mut active.journal_changed);
            keep_recent(recent, active.jobs.ended.drain(..));
            if ended {
                // A run torn down before its steps ended (its runtime stopped) holds what it had.
                let rest = take_by_step(&mut active.awaiting, |_, _| true);
                hold_all(&self.results, rest, |_| "the run stopped before it was recorded".into());
                notices.push(RunNotice::Finished { canvas: active.canvas });
            }
            !ended
        });
        lock(&self.inbox).running = self.runs.len();
        let (resumed, handled) = self.resumes.absorb(session, &self.budget, &self.results, &mut notices);
        journal_changed |= handled;
        for ended in resumed {
            settled = true;
            journal_changed = true;
            match ended {
                resume::Ended::Kept(entry) => self.kept.push(*entry),
                resume::Ended::Parked(entry) => self.parked.push(*entry),
                resume::Ended::Settled => {}
            }
        }
        keep_recent(&mut self.recent, self.resumes.jobs.ended.drain(..));
        act_on_held(session, &self.results, &mut notices);
        self.estimates.pump(session, &self.runtime, &mut notices);
        if settled {
            lock(&self.journal).reconciled = false;
            refresh(session, &self.budget);
        } else {
            self.sync_usage(session);
        }
        self.journal_changed |= journal_changed;
        let now = self.runtime.now_ms();
        let check = (!self.runs.is_empty() || !self.resumes.live.is_empty()) && now.saturating_sub(self.checked_at) >= SAVE_CHECK_MS;
        if self.journal_changed || settled || check {
            self.checked_at = now;
            notices.extend(self.save(false));
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
            Arc::new(move |job, sent: Sent<'_>, update: RunUpdate| {
                let mut mailbox = lock(&reports);
                if let RunUpdate::Submitted(handle) = &update {
                    mailbox.submitted.push((job, sent.base.to_owned(), sent.generation, handle.clone()));
                }
                mailbox.reports.note(job, sent.attempt, &update);
            }),
        )
        .with_budget(self.budget.clone())
        .with_write_ahead(self.write_ahead())
        .with_kept_handles(Arc::clone(&self.kept_handles));
        let seq = self.next_run;
        self.next_run = self.next_run.saturating_add(1);
        let templates: Arc<Mutex<BTreeMap<JobId, Submitted>>> = Arc::default();
        let provider = provider.with_stage(self.stage(seq, &templates));
        let actions = Arc::new(LocalActions::new(session.assets().clone()).with_video_tools(self.video_tools.clone()));
        let plugins = (!session.plugins().is_empty()).then(|| Arc::clone(session.plugins()));
        let routed = crate::plugin_runtime::Routed { local: actions.clone(), plugins: plugins.clone() };
        let sources = SourceRunner::default().with_actions(Arc::new(routed));
        let runner = Arc::new(Either { provider: Arc::new(provider), sources, local: Mutex::default() });
        let (driver, handle) = driver(Scheduler::new(Default::default()));
        let mut active = Active {
            seq,
            canvas,
            run,
            runner,
            actions,
            plugins,
            mailbox,
            handle: Some(handle),
            provider_jobs: BTreeSet::new(),
            live: BTreeMap::new(),
            cancelled: false,
            jobs: RunJobs::default(),
            templates,
            shared: Arc::clone(&self.journal),
            journal: BTreeMap::new(),
            awaiting: BTreeMap::new(),
            journal_changed: false,
        };
        // Queue before the driver runs, so a job that can't be prepared is never sent.
        let mut started = Vec::new();
        advance(session, &mut active, progress, &mut started, &self.runtime, &self.results);
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

/// Charges a restored job that can't be followed as unresolved, at what it was reserved at. A
/// free job has no charge; one that can't be settled stays an open estimate, which caps count.
fn settle_unresolved(budget: &JobBudget, entry: &Submitted) {
    let key = entry.sent_key();
    let mut log = lock(&budget.log);
    let Some(cost) = log.charge_target(&entry.target, &key).filter(|charge| charge.status == Charge::Estimate).map(|charge| charge.cost) else { return };
    let _ = log.settle_target(&entry.target, &key, cost, Charge::Unresolved);
}

/// Marks the run's driver as returned when dropped: when the task ends, or when a runtime that is
/// shutting down drops it unfinished.
struct Ended(Arc<Mutex<Mailbox>>);
impl Drop for Ended {
    fn drop(&mut self) {
        lock(&self.0).ended = true;
    }
}

/// Most bytes of stopped text answers kept across the recent jobs, waiting for Keep (#504).
const MAX_KEPT_PARTIALS: usize = 4 * nodecraft_providers::MAX_STREAMED_TEXT;

/// Adds jobs that ended to `recent`, dropping the oldest past [`MAX_RECENT`]. A card shows only
/// its newest job's stopped answer, so older ones of that node are dropped, and the oldest go
/// once they add up to more than [`MAX_KEPT_PARTIALS`].
fn keep_recent(recent: &mut VecDeque<JobView>, ended: impl Iterator<Item = JobView>) {
    for view in ended {
        if recent.len() >= MAX_RECENT {
            recent.pop_front();
        }
        if view.partial.is_some() {
            recent.iter_mut().filter(|old| old.canvas == view.canvas && old.node == view.node).for_each(|old| old.partial = None);
        }
        recent.push_back(view);
    }
    let mut kept = 0usize;
    for view in recent.iter_mut().rev() {
        let len = view.partial.as_ref().map_or(0, |text| text.len());
        kept = kept.saturating_add(len);
        if kept > MAX_KEPT_PARTIALS {
            view.partial = None;
        }
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
fn advance(session: &mut Session, active: &mut Active, progress: Progress, notices: &mut Vec<RunNotice>, runtime: &Arc<dyn Runtime>, held: &Mutex<held::Held>) {
    let canvas = active.canvas;
    let mut pending = VecDeque::from([progress]);
    while let Some(progress) = pending.pop_front() {
        // Results the step let go first (a sibling in a failed fanned item, or a job that finished
        // after its step had ended), so a partial record can't take them along. The step's card
        // reports its own outcome.
        hold_dropped(active, &progress.dropped, held);
        // Then results: a partial one (#480) is recorded, and its step then reports why it is
        // partial. A technique node's record takes its inner steps' results along (#615).
        for result in progress.finished {
            let run = &active.run;
            let node = result.node;
            let awaiting = take_by_step(&mut active.awaiting, |_, a| run.owner(a.step) == node && (a.step == node || !run.is_running(a.step)));
            let notice = record(session, canvas, result);
            notices.push(match notice {
                RunNotice::Failed { canvas, node, reason } => {
                    let kept = hold_all(held, awaiting, |_| reason.clone());
                    RunNotice::Failed { canvas, node, reason: if kept { format!("{reason}; {HELD_NOTE}") } else { reason } }
                }
                // Recorded: the step's results, and its inner steps', were used.
                notice => notice,
            });
        }
        // A step can still fail after its paid result was admitted (assembling or checking it):
        // that result is held too, and so are those of a failed technique node's inner steps.
        for failure in progress.failed {
            let run = &active.run;
            let failed = |step: NodeId| run.owner(step) == failure.node && !run.is_running(step) && (run.has_failed(step) || run.has_failed(failure.node));
            let awaiting = take_by_step(&mut active.awaiting, |_, a| failed(a.step));
            // An inner step that finished says its technique failed, and why.
            let reason = |step: NodeId| if run.has_failed(step) { failure.reason.clone() } else { format!("its technique failed: {}", failure.reason) };
            let kept = hold_all(held, awaiting, reason);
            notices.push(match failure.reason == CANCELLED {
                // Cancelled with paid results already in: they are held, and the card shows the cancel.
                true => RunNotice::Cancelled { canvas, node: failure.node },
                false if kept => RunNotice::Failed { canvas, node: failure.node, reason: format!("{}; {HELD_NOTE}", failure.reason) },
                false => RunNotice::Failed { canvas, node: failure.node, reason: failure.reason },
            });
        }
        if active.cancelled {
            for spec in progress.jobs {
                pending.push_back(active.run.job_cancelled(session.kinds(), spec.id, CANCELLED));
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
                    pending.push_back(active.run.job_failed(session.kinds(), spec.id, &reason));
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
    let work = active.run.work(spec.id).ok_or("the job has no work")?.clone();
    let work = &work;
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
        cancel_refused: false,
        reported_sent: false,
        partial: None,
        partial_attempt: 0,
    };
    match work {
        JobSpec::Generate { node, outputs, .. } => {
            // An inlined step's jobs show, and report, as its technique node's (#94).
            let node = active.run.owner(*node);
            active.live.insert(spec.id, node);
            active.jobs.live.insert(spec.id, view(node));
            let queued = active.run.queued_job_with_media(spec.id, &spec.idempotency_key, session.assets()).map_err(|e| e.message)?;
            let price = queued.charged_price();
            let estimate = price.and_then(|price| price.estimate);
            if let Some(view) = active.jobs.live.get_mut(&spec.id) {
                view.provider = Some(queued.provider.target().adapter().to_owned());
                view.model = Some(queued.job.model.clone());
                view.estimate = estimate;
            }
            let template = Submitted {
                project: session.document().id(),
                canvas,
                node,
                target: queued.provider.target().clone(),
                key: spec.idempotency_key.clone(),
                generation: 0,
                handle: JobHandle { remote_id: String::new(), data: serde_json::Value::Null },
                model: queued.job.model.clone(),
                estimate,
                priced: price.is_some(),
                routes: outputs.clone(),
                recipe: active.run.resume_recipe(session.kinds(), spec.id),
                launches: 0,
                staged: None,
            };
            lock(&active.templates).insert(spec.id, template);
            active.runner.provider.register(spec.id, queued);
            active.provider_jobs.insert(spec.id);
        }
        JobSpec::Source { node, .. } | JobSpec::Action { node, .. } => {
            let node = active.run.owner(*node);
            active.live.insert(spec.id, node);
            active.jobs.live.insert(spec.id, view(node));
            active.runner.sources.register(spec.id, work.clone());
            lock(&active.runner.local).insert(spec.id);
        }
    }
    Ok(())
}

/// Absorbs one run's messages; returns whether a paid job settled.
fn absorb(
    session: &mut Session,
    active: &mut Active,
    notices: &mut Vec<RunNotice>,
    runtime: &Arc<dyn Runtime>,
    budget: &JobBudget,
    held: &Mutex<held::Held>,
) -> bool {
    // Before the events, so a job that ended this frame is dropped from the journal after.
    note_submitted(active);
    let (events, rejected, mut reports) = {
        let mut mailbox = lock(&active.mailbox);
        (std::mem::take(&mut mailbox.events), std::mem::take(&mut mailbox.rejected), std::mem::take(&mut mailbox.reports))
    };
    // Before the events, so a job cancelled this frame knows whether the provider had it; again
    // after, so a job that started this frame shows its progress.
    active.jobs.absorb_reports(&mut reports);
    let mut settled = false;
    for (job, reason) in rejected {
        active.jobs.end(job, JobState::Failed { reason: reason.clone(), cause: None });
        settled |= finish(active, job);
        let progress = active.run.job_failed(session.kinds(), job, &reason);
        advance(session, active, progress, notices, runtime, held);
    }
    for event in events {
        let progress = match event {
            Event::Done { job } => {
                let outputs = outputs(session, active, job, held);
                settled |= finish(active, job);
                active.jobs.end(job, ended(&outputs));
                match outputs {
                    Ok((outputs, billed)) => {
                        if let Some(units) = billed {
                            active.run.job_billed(job, units);
                        }
                        active.run.job_done(session.kinds(), job, outputs)
                    }
                    Err(reason) => active.run.job_failed(session.kinds(), job, &reason),
                }
            }
            Event::Failed { job, error } => match unaccounted(active, job, budget) {
                // The provider finished and billed; only writing the charge failed.
                Some(charge) => {
                    let node = active.live.get(&job).copied();
                    let outputs = outputs(session, active, job, held);
                    settled |= finish(active, job);
                    active.jobs.end(job, ended(&outputs));
                    if let Err(reason) = charge {
                        notices.extend(node.map(|node| RunNotice::Unaccounted { canvas: active.canvas, node, reason }));
                        stop(active);
                    }
                    match outputs {
                        Ok((outputs, billed)) => {
                            if let Some(units) = billed {
                                active.run.job_billed(job, units);
                            }
                            active.run.job_done(session.kinds(), job, outputs)
                        }
                        Err(reason) => active.run.job_failed(session.kinds(), job, &reason),
                    }
                }
                None => {
                    if let Some(key) = active.provider_jobs.contains(&job).then(|| active.runner.provider.take_billed(job)).flatten() {
                        active.run.job_may_be_billed(job, &key);
                    }
                    settled |= finish(active, job);
                    active.jobs.end(job, JobState::Failed { reason: error.clone(), cause: None });
                    active.run.job_failed(session.kinds(), job, &error)
                }
            },
            Event::Cancelled { job } => {
                settled |= finish(active, job);
                active.jobs.end(job, JobState::Cancelled);
                active.run.job_cancelled(session.kinds(), job, CANCELLED)
            }
            Event::Skipped { job, .. } => {
                settled |= finish(active, job);
                if active.cancelled {
                    // Stopped by the run's own cancel, not by a failure.
                    active.jobs.end(job, JobState::Cancelled);
                    active.run.job_cancelled(session.kinds(), job, CANCELLED)
                } else {
                    let reason = "skipped because an earlier job failed";
                    active.jobs.end(job, JobState::Failed { reason: reason.into(), cause: None });
                    active.run.job_failed(session.kinds(), job, reason)
                }
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
        advance(session, active, progress, notices, runtime, held);
    }
    active.jobs.absorb_reports(&mut reports);
    let_go_of_ended(active, held);
    if active.run.is_finished() {
        // Dropping the last handle lets the driver drain and return.
        active.handle = None;
    }
    settled
}

/// Moves the handles the provider issued into the run's journal.
fn note_submitted(active: &mut Active) {
    let submitted = std::mem::take(&mut lock(&active.mailbox).submitted);
    for (job, key, generation, handle) in submitted {
        let Some(template) = lock(&active.templates).get(&job).cloned() else { continue };
        // Followed by its handle from here on, not sent again; its staged copy goes once the
        // handle is saved.
        active.journal.insert(job, Submitted { key, generation, handle, ..template });
        active.journal_changed = true;
    }
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
    lock(&active.templates).remove(&job);
    active.journal_changed |= lock(&active.shared).staging.remove(&(active.seq, job)).is_some();
    active.journal_changed |= active.journal.remove(&job).is_some();
    lock(&active.runner.local).remove(&job);
    let provider = active.provider_jobs.contains(&job);
    if provider {
        active.runner.provider.forget(job);
    }
    provider
}

/// A finished job's outputs and the units its provider said it billed (#437).
type Finished = (Vec<(String, nodecraft_types::Value)>, Option<nodecraft_types::BillableUnits>);

/// A finished job's outputs, with any assets it made imported into the project. A model job's
/// result that can't be converted, imported or fitted to its node's ports is held (#369); one
/// that converts waits in `awaiting` until its step is recorded.
fn outputs(session: &mut Session, active: &mut Active, job: JobId, held: &Mutex<held::Held>) -> Result<Finished, String> {
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
        // Rendered videos may be over the resident limit: they stream in.
        for (hash, bytes) in active.actions.staged_videos() {
            if session.assets().contains(&hash) {
                continue;
            }
            let length = bytes.len() as u64;
            let range = nodecraft_format::asset_source::AssetRange::new(Arc::new(nodecraft_format::asset_source::MemorySource::new(bytes)), 0, length)
                .map_err(|e| e.message)?;
            let imported = session.import_asset_from(&range, &Default::default(), &Default::default()).map_err(|e| e.message)?;
            if imported != hash {
                return Err("a rendered video changed while it was imported".into());
            }
        }
        let outputs = take_outputs(&active.runner.sources.outputs, job);
        // A plugin's result is recorded only while the grant it ran under stands (#173).
        if let Some(JobSpec::Action { action, .. }) = active.run.work(job)
            && crate::plugin_runtime::is_plugin(action)
            && !active.plugins.as_ref().and_then(|p| p.get(action)).is_some_and(|a| !a.is_revoked())
        {
            return Err("this plugin's grant was revoked; its result was dropped".into());
        }
        return Ok((outputs, None));
    }
    let delivery = lock(&active.mailbox).deliveries.remove(&job).ok_or("the model's result went missing")?;
    let Some(JobSpec::Generate { node: step, outputs: routes, .. }) = active.run.work(job) else { return Err("the job is not a model call".into()) };
    let step = *step;
    let converted = convert(session, &active.run, job, routes, &delivery.completed);
    let Some(template) = lock(&active.templates).get(&job).cloned() else { return converted.map(|outputs| (outputs, delivery.completed.usage.billable_units)) };
    let kept = |reason: String, completed: Completed| held::HeldResult {
        project: template.project,
        canvas: template.canvas,
        node: template.node,
        provider: template.target.adapter().to_owned(),
        model: template.model.clone(),
        reason,
        completed,
        routes: template.routes.clone(),
        recipe: template.recipe.clone(),
    };
    match converted {
        Ok(outputs) => {
            let billed = delivery.completed.usage.billable_units;
            // A result with nothing in it is nothing to hold.
            if !outputs.is_empty() {
                active.awaiting.insert(job, Awaiting { step, result: kept(String::new(), delivery.completed) });
            }
            Ok((outputs, billed))
        }
        Err(reason) => {
            held::hold(held, kept(reason.clone(), delivery.completed));
            Err(format!("{reason}; {HELD_NOTE}"))
        }
    }
}

/// Holds `results`, one per step, each saying the `reason` for its step; whether there were any.
fn hold_all(held: &Mutex<held::Held>, results: BTreeMap<NodeId, held::HeldResult>, reason: impl Fn(NodeId) -> String) -> bool {
    let any = !results.is_empty();
    for (step, result) in results {
        held::hold(held, held::HeldResult { reason: reason(step), ..result });
    }
    any
}

/// Takes the awaiting results that `matches`, one held result per step: one job's result as it
/// came, or several jobs' outputs in job order (placed or discarded, never retried alone).
fn take_by_step(awaiting: &mut BTreeMap<JobId, Awaiting>, matches: impl Fn(JobId, &Awaiting) -> bool) -> BTreeMap<NodeId, held::HeldResult> {
    let jobs: Vec<JobId> = awaiting.iter().filter(|(job, a)| matches(**job, a)).map(|(job, _)| *job).collect();
    let mut steps: BTreeMap<NodeId, held::HeldResult> = BTreeMap::new();
    for job in jobs {
        let Some(Awaiting { step, result }) = awaiting.remove(&job) else { continue };
        match steps.entry(step) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(result);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let merged = slot.get_mut();
                merged.recipe = None;
                merged.completed.usage = nodecraft_providers::Usage::default();
                merged.completed.outputs.extend(result.completed.outputs);
            }
        }
    }
    steps
}

/// Holds the awaiting results of `dropped` jobs, one per step.
fn hold_dropped(active: &mut Active, dropped: &[JobId], held: &Mutex<held::Held>) {
    if dropped.is_empty() {
        return;
    }
    let run = &active.run;
    for (step, result) in take_by_step(&mut active.awaiting, |job, _| dropped.contains(&job)) {
        let reason = if run.is_running(step) { "another job of its item didn't finish" } else { "it finished after its step had ended" };
        held::hold(held, held::HeldResult { reason: reason.into(), ..result });
    }
}

/// Lets go of the awaiting results of steps that ended. An inlined step's results wait until its
/// technique node ends too (#615), because that node's record may still be refused, which holds
/// them. Results whose step or technique node failed without taking them (it reported another
/// step's failure) are held. A recorded technique node used the rest.
fn let_go_of_ended(active: &mut Active, held: &Mutex<held::Held>) {
    let run = &active.run;
    let ended = |step: NodeId| !run.is_running(step) && run.has_ended(run.owner(step));
    for (step, result) in take_by_step(&mut active.awaiting, |_, a| ended(a.step)) {
        if run.has_failed(step) {
            held::hold(held, held::HeldResult { reason: "its step failed".into(), ..result });
        } else if run.has_failed(run.owner(step)) {
            held::hold(held, held::HeldResult { reason: "its technique failed".into(), ..result });
        }
    }
}

/// Converts a model job's result by `routes`, checks it fits its node's ports, and imports its
/// assets.
fn convert(
    session: &mut Session,
    run: &AuthorizedRun,
    job: JobId,
    routes: &[GenerateOutput],
    completed: &Completed,
) -> Result<Vec<(String, nodecraft_types::Value)>, String> {
    let produced = generate::produced(routes, completed).map_err(|e| e.message)?;
    run.admit_outputs(job, &produced.outputs)?;
    for (hash, bytes) in produced.assets {
        let imported = session.import_asset(&bytes).map_err(|e| e.message)?;
        if imported != hash {
            return Err("a generated asset changed while it was imported".into());
        }
    }
    Ok(produced.outputs)
}

/// Retries or places the held results asked for since the last pump. A result is let go only
/// once that worked; otherwise it stays held and the failure is reported.
fn act_on_held(session: &mut Session, results: &Mutex<held::Held>, notices: &mut Vec<RunNotice>) {
    let pending = lock(results).pending();
    for (id, action, result) in pending {
        let (canvas, node) = (result.canvas, result.node);
        let outcome = match (action, &result.recipe) {
            _ if session.document().id() != result.project => Err("its project isn't open".to_owned()),
            (HeldAction::Retry, Some(recipe)) => resume::record_alone(session, canvas, node, &result.routes, recipe, &result.completed),
            (HeldAction::Retry, None) => Err("its step needs other jobs' results too".to_owned()),
            (HeldAction::Place, _) => held::place(session, &result),
        };
        lock(results).settle(id, outcome.is_ok());
        notices.push(match (outcome, action) {
            (Ok(()), HeldAction::Retry) => RunNotice::Recorded { canvas, node },
            (Ok(()), HeldAction::Place) => RunNotice::Placed { canvas, node },
            (Err(reason), _) => RunNotice::Failed { canvas, node, reason: format!("{reason}; the paid result is still held") },
        });
    }
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
