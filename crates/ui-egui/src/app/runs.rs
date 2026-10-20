//! The desktop app's runs (#409): the engine's [`RunHost`] on a tokio runtime, with provider
//! adapters built from the keychain on their own thread.
//!
//! Generate first makes sure the card's adapter is built ([`Runs::route`]). Building reads the
//! provider's key, which can show a keychain prompt, so it happens on the provider thread and
//! the quote waits for the answer ([`Tick::ready`]). Once the adapter is ready, the card is routed
//! to it and quoted as before. [`Runs::tick`] pumps the host once a frame and keeps what the
//! cards show: which are running and why the last run of one failed.
//!
//! With a run journal ([`Runs::open_journal`], #508) jobs cut off by quitting or a crash are
//! followed again once their provider is set up ([`Runs::resume_waiting`]), and quitting leaves
//! running jobs at the provider instead of cancelling them.
use super::generate::RunKind;
use nodecraft_engine::{
    Session,
    jobs::JobBudget,
    provider_routing::{ProviderRouting, RegisteredProvider},
    provider_setup::{self, HostEnv, ProviderSetup, Readiness},
    run_host::{RunHost, RunNotice},
    run_jobs::{JobKey, JobState, JobView},
    run_journal::FileJournal,
    secrets::KeyringStore,
    settings::ProviderSettings,
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
};
use nodecraft_providers::{Provider, http::Http};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Adapters the provider thread may be asked for before it answers; more fail as busy.
const ASKS: usize = 16;

/// The engine's runtime over a tokio runtime the app owns.
struct Tokio {
    handle: tokio::runtime::Handle,
    start: Instant,
}

impl Runtime for Tokio {
    fn spawn(&self, task: TaskFuture<()>) {
        self.handle.spawn(task);
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        // Spawned rather than created here, so the timer always belongs to the runtime, whichever
        // thread asks. A dropped sleep stops its timer too.
        let timer = Timer(self.handle.spawn(tokio::time::sleep(Duration::from_millis(ms))));
        Box::pin(async move {
            let mut timer = timer;
            let _ = (&mut timer.0).await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A sleep's timer task, aborted when the sleep is dropped unfinished.
struct Timer(tokio::task::JoinHandle<()>);
impl Drop for Timer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The notice while a card waits for its provider.
pub(super) const SETTING_UP: &str = "Setting up the provider…";

/// An adapter asked for in one key epoch, and what building it gave.
type Ask = (u64, String, ProviderSettings);
type Made = (u64, String, Result<Arc<dyn Provider>, Readiness>);
/// Builds one adapter; the app's reads the keychain ([`provider_setup::make`]), tests pass mocks.
pub(super) type Make = Box<dyn Fn(&str, &ProviderSettings) -> Result<Arc<dyn Provider>, Readiness> + Send>;

/// The thread that builds adapters: it reads keys, so an unlock prompt never blocks a frame.
struct Maker {
    asks: SyncSender<Ask>,
    made: Receiver<Made>,
}

impl Maker {
    fn spawn(make: Make) -> Result<Self, Error> {
        let (asks, inbox) = sync_channel::<Ask>(ASKS);
        let (outbox, made) = sync_channel::<Made>(ASKS);
        std::thread::Builder::new()
            .name("nodecraft-providers".into())
            .spawn(move || {
                for (epoch, adapter, settings) in inbox {
                    // A keychain backend that panics fails this adapter, not the thread.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make(&adapter, &settings)))
                        .unwrap_or_else(|_| Err(Readiness::Failed("setting it up failed unexpectedly".into())));
                    if outbox.send((epoch, adapter, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the provider thread: {e}")))?;
        Ok(Self { asks, made })
    }
}

/// Whether a card can be quoted now.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Its provider is routed (or it needs none): quote it.
    Ready,
    /// Its provider is being set up; [`Tick::ready`] names the card once it is.
    Waiting,
}

/// A run waiting for its providers to be set up.
#[derive(Clone)]
struct Waiting {
    canvas: CanvasId,
    nodes: Vec<NodeId>,
    kind: RunKind,
    /// Adapters asked for and not yet answered.
    pending: BTreeSet<String>,
    settings: ProviderSettings,
}

/// What one frame's pump brought.
#[derive(Default)]
pub(super) struct Tick {
    /// A run whose providers are now set up, to quote again.
    pub ready: Option<(Vec<NodeId>, RunKind)>,
    /// A card stopped waiting for its provider without being quoted (it's on another canvas).
    pub abandoned: bool,
    /// Something to tell the person: a run that couldn't start or a provider that can't run.
    pub notice: Option<String>,
}

pub(super) struct Runs {
    host: RunHost,
    setup: ProviderSetup,
    maker: Maker,
    /// Adapters asked for in this epoch and not yet answered.
    asked: BTreeSet<String>,
    /// Bumped when a key changes: adapters asked for before then are built with the old key.
    epoch: u64,
    /// The run waiting for its adapters.
    waiting: Option<Waiting>,
    /// Why the last run of a card failed, until it runs again.
    failed: BTreeMap<(CanvasId, NodeId), String>,
    running: BTreeSet<(CanvasId, NodeId)>,
    /// Every job the host is running or recently ran, for the queue panel.
    jobs: Vec<JobView>,
    /// Runs the host's tasks; shut down without waiting when the app closes.
    tokio: Option<tokio::runtime::Runtime>,
    /// Why the run journal couldn't be opened, told on the first frame.
    journal_problem: Option<String>,
}

impl Runs {
    /// The runtime, HTTP client and provider thread; registers `run.start` in `services`.
    pub(super) fn new(services: &mut nodecraft_engine::commands::Services) -> Result<Self, Error> {
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let start = Instant::now();
        let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
        Self::with_maker(
            services,
            Box::new(move |adapter, settings| {
                let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings, now_ms: now_ms.clone() };
                provider_setup::make(adapter, &env)
            }),
        )
    }

    /// [`Self::new`] with adapters built by `make`.
    pub(super) fn with_maker(services: &mut nodecraft_engine::commands::Services, make: Make) -> Result<Self, Error> {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("nodecraft-runs")
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the run threads: {e}")))?;
        let start = Instant::now();
        let runtime = Arc::new(Tokio { handle: tokio.handle().clone(), start });
        let maker = Maker::spawn(make)?;
        let mut id = [0u8; 8];
        getrandom::fill(&mut id).map_err(|_| Error::new(ErrorKind::Internal, "session identity entropy is unavailable"))?;
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: id.iter().map(|b| format!("{b:02x}")).collect(),
            wall_clock_ms: Arc::new(|| {
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            }),
        };
        let host = RunHost::new(runtime, budget);
        host.register(services)?;
        host.register_ownership(services)?;
        host.register_resolution(services)?;
        Ok(Self {
            host,
            setup: ProviderSetup::default(),
            maker,
            asked: BTreeSet::new(),
            epoch: 0,
            waiting: None,
            failed: BTreeMap::new(),
            running: BTreeSet::new(),
            jobs: Vec::new(),
            tokio: Some(tokio),
            journal_problem: None,
        })
    }

    /// Keeps runs in the journal at `path` across restarts (see the module docs), restoring
    /// what it holds. Call before [`Self::install`], and only while this window owns `path`.
    pub(super) fn open_journal(&mut self, path: std::path::PathBuf) {
        if let Err(error) = self.host.open_journal(Box::new(FileJournal::at(path))) {
            log::warn!("run journal: {}", error.message);
            self.journal_problem = Some(format!("Runs from the last session couldn't be restored: {}", error.message));
        }
    }

    /// Follows jobs restored from the journal whose provider is ready, and asks for the
    /// providers of the others. A provider that can't run leaves its jobs waiting (their charges
    /// stay counted) until it can, or until the next launch.
    pub(super) fn resume_waiting(&mut self, session: &Session, settings: &ProviderSettings) {
        if !self.host.has_waiting() {
            return;
        }
        for adapter in self.host.waiting_adapters(session.document().id()) {
            match self.setup.readiness(&adapter) {
                Some(Readiness::Ready) => {
                    let binding = self.setup.provider(&adapter).map(|provider| RegisteredProvider::default_instance(Arc::clone(provider)));
                    match binding {
                        Some(Ok(binding)) => self.host.resume(session, &binding),
                        Some(Err(error)) => log::warn!("can't resume jobs on {adapter}: {}", error.message),
                        None => {}
                    }
                }
                Some(_) => {}
                None => {
                    // Asked again on a later frame if the thread is busy.
                    let _ = self.ask(&adapter, settings);
                }
            }
        }
    }

    /// Whether restored jobs wait for their provider.
    pub(super) fn has_waiting(&self) -> bool {
        self.host.has_waiting()
    }

    /// Puts the budget resources in place, before the first quote.
    pub(super) fn install(&self, session: &mut Session) {
        self.host.install(session);
    }

    /// Refreshes the usage snapshot after a charge-ownership decision, so the review panel reads
    /// the log as it now is.
    pub(super) fn sync_usage(&self, session: &mut Session) {
        self.host.sync_usage(session);
    }

    /// Routes the cards a run of `ids` will run (the cards alone, or with their stale or unrun
    /// upstream for a refresh; `stale` is the canvas's stale set) to their providers when those
    /// are set up, or asks for them to be.
    pub(super) fn route(
        &mut self,
        session: &mut Session,
        canvas: CanvasId,
        ids: &[NodeId],
        kind: RunKind,
        stale: &BTreeSet<NodeId>,
        settings: &ProviderSettings,
    ) -> Result<Route, Error> {
        let (adapters, inner) = adapters_of(session, canvas);
        let mut reached: BTreeSet<NodeId> = match kind {
            RunKind::Generate => ids.iter().copied().collect(),
            RunKind::Refresh => with_upstream(session, canvas, ids, stale),
        };
        // A technique node runs the nodes inlined for it.
        reached.extend(inner.iter().filter(|(_, owner)| reached.contains(owner)).map(|(id, _)| *id).collect::<Vec<_>>());
        let needed: BTreeSet<String> = reached.iter().filter_map(|node| adapters.get(node).cloned()).collect();
        let mut pending = BTreeSet::new();
        let mut unready = Vec::new();
        for adapter in needed {
            match self.setup.readiness(&adapter) {
                Some(Readiness::Ready) => {}
                Some(other) => {
                    unready.push(other.message(&adapter).unwrap_or_default());
                    // Tried afresh next time: the person may add the key or the server meanwhile.
                    self.setup.forget(&adapter);
                }
                None => {
                    self.ask(&adapter, settings)?;
                    pending.insert(adapter);
                }
            }
        }
        // Every provider that can't run is named at once, not one per attempt.
        if !unready.is_empty() {
            return Err(Error::new(ErrorKind::Unsupported, unready.join(" ")));
        }
        if !pending.is_empty() {
            self.waiting = Some(Waiting { canvas, nodes: ids.to_vec(), kind, pending, settings: settings.clone() });
            return Ok(Route::Waiting);
        }
        // Every card whose provider is ready, so a run of more than these cards reaches its
        // providers too.
        let ready = adapters.iter().filter(|(_, a)| self.setup.readiness(a) == Some(&Readiness::Ready)).map(|(node, a)| (*node, a.as_str()));
        session.set_provider_routing(self.setup.routing(ready)?)?;
        Ok(Route::Ready)
    }

    /// Asks the provider thread for `adapter`, unless it already was in this epoch.
    fn ask(&mut self, adapter: &str, settings: &ProviderSettings) -> Result<(), Error> {
        if self.asked.contains(adapter) {
            return Ok(());
        }
        match self.maker.asks.try_send((self.epoch, adapter.to_owned(), settings.clone())) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(Error::new(ErrorKind::Busy, "providers are still being set up; try again")),
            Err(TrySendError::Disconnected(_)) => return Err(Error::new(ErrorKind::Internal, "the provider thread has stopped")),
        }
        self.asked.insert(adapter.to_owned());
        Ok(())
    }

    /// Whether a card is waiting for its provider.
    pub(super) fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// Forgets every adapter, so the next run reads keys again (after one was saved or deleted).
    /// The session's routing goes too, so nothing quotes or starts on an adapter holding the old
    /// key, and one still being built with it is dropped when it arrives. A card waiting for its
    /// provider is asked for again.
    pub(super) fn forget_adapters(&mut self, session: &mut Session) -> Result<(), Error> {
        let adapters: Vec<String> = self.setup.adapters().map(|(adapter, _)| adapter.to_owned()).collect();
        for adapter in adapters {
            self.setup.forget(&adapter);
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.asked.clear();
        session.set_provider_routing(ProviderRouting::default())?;
        if let Some(waiting) = self.waiting.clone() {
            for adapter in &waiting.pending {
                if let Err(error) = self.ask(adapter, &waiting.settings) {
                    self.waiting = None;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// One frame: takes built adapters and pumps the host.
    pub(super) fn tick(&mut self, session: &mut Session, canvas: CanvasId) -> Tick {
        let mut tick = Tick::default();
        let made: Vec<Made> = self.maker.made.try_iter().collect();
        for (epoch, adapter, result) in made {
            if epoch != self.epoch {
                continue;
            }
            self.asked.remove(&adapter);
            self.setup.forget(&adapter);
            self.setup.ensure(&adapter, |_| result);
            if let Some(waiting) = &mut self.waiting
                && waiting.pending.remove(&adapter)
                && waiting.pending.is_empty()
            {
                if waiting.canvas == canvas {
                    tick.ready = Some((std::mem::take(&mut waiting.nodes), waiting.kind));
                } else {
                    tick.abandoned = true;
                }
                self.waiting = None;
            }
        }
        let mut notices: Vec<String> = self.journal_problem.take().into_iter().collect();
        for notice in self.host.pump(session) {
            match notice {
                RunNotice::Started { canvas, nodes } => {
                    for node in nodes {
                        self.failed.remove(&(canvas, node));
                    }
                }
                RunNotice::Recorded { canvas, node } | RunNotice::Cancelled { canvas, node } => {
                    self.failed.remove(&(canvas, node));
                }
                RunNotice::Failed { canvas, node, reason } | RunNotice::Unaccounted { canvas, node, reason } => {
                    notices.push(format!("A run failed: {reason}"));
                    self.failed.insert((canvas, node), reason);
                }
                RunNotice::Refused { reason } => notices.push(format!("The run didn't start: {reason}")),
                RunNotice::Unsaved { reason } => notices.push(format!("Runs can't be kept for a restart: {reason}")),
                RunNotice::Finished { .. } => {}
            }
        }
        // The first says why; the rest are on their cards.
        tick.notice = notices.first().map(|first| match notices.len().saturating_sub(1) {
            0 => first.clone(),
            more => format!("{first} (and {more} more)"),
        });
        self.running = self.host.running();
        self.jobs = self.host.jobs();
        tick
    }

    /// The queue panel's jobs: recently ended ones first, then those queued or running.
    pub(super) fn jobs(&self) -> &[JobView] {
        &self.jobs
    }

    /// Asks one job to stop; its end shows on the next frames.
    pub(super) fn cancel(&mut self, key: JobKey) -> Result<(), Error> {
        self.host.cancel(key)?;
        self.jobs = self.host.jobs();
        Ok(())
    }

    /// Forgets the jobs that ended.
    pub(super) fn clear_finished(&mut self) {
        self.host.clear_finished();
        self.jobs = self.host.jobs();
    }

    /// A card's status while it has work queued or running: "Queued", "Generating 40%", ….
    pub(super) fn card_status(&self, canvas: CanvasId, node: NodeId) -> Option<String> {
        let view = self.jobs.iter().rev().find(|view| view.canvas == canvas && view.node == node && !view.state.is_final())?;
        Some(match &view.state {
            _ if view.cancel_refused => "Cancel refused".into(),
            _ if view.cancel_requested => "Cancelling…".into(),
            JobState::Running { waiting: false, fraction: None, message: None, reconnecting: false, .. } => "Generating…".into(),
            JobState::Retrying { .. } => "Retrying…".into(),
            _ => view.status(),
        })
    }

    /// A text card's answer still streaming in, or left by its last job when that was cancelled
    /// or failed (#504), with why it stopped.
    pub(super) fn partial(&self, canvas: CanvasId, node: NodeId) -> Option<crate::card::generate::Partial<'_>> {
        let view = self.jobs.iter().rev().find(|view| view.canvas == canvas && view.node == node)?;
        let text = view.partial.as_deref().filter(|text| !text.is_empty())?;
        let stopped = match &view.state {
            JobState::Done => return None,
            JobState::Cancelled => Some("Stopped before it finished"),
            JobState::Failed { .. } => Some("Failed before it finished"),
            _ => None,
        };
        Some(crate::card::generate::Partial { text, stopped })
    }

    /// Records the partial answer a card's last job left, without changing the answer it shows.
    pub(super) fn keep_partial(&mut self, session: &mut Session, canvas: CanvasId, node: NodeId) -> Result<(), Error> {
        let key = self.ended_partial(canvas, node)?;
        let kept = self.host.keep_partial(session, key);
        self.jobs = self.host.jobs();
        kept
    }

    /// Lets go of the partial answer a card's last job left.
    pub(super) fn discard_partial(&mut self, canvas: CanvasId, node: NodeId) -> Result<(), Error> {
        let key = self.ended_partial(canvas, node)?;
        self.host.discard_partial(key);
        self.jobs = self.host.jobs();
        Ok(())
    }

    fn ended_partial(&self, canvas: CanvasId, node: NodeId) -> Result<JobKey, Error> {
        let view = self.jobs.iter().rev().find(|view| view.canvas == canvas && view.node == node && view.partial.is_some());
        view.filter(|view| matches!(view.state, JobState::Cancelled | JobState::Failed { .. }))
            .map(|view| view.key)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "that partial answer is no longer kept"))
    }

    /// Whether anything is in progress, so the next frame comes without input.
    pub(super) fn is_busy(&self) -> bool {
        self.host.is_busy() || !self.asked.is_empty()
    }

    pub(super) fn is_running(&self, canvas: CanvasId, node: NodeId) -> bool {
        self.running.contains(&(canvas, node))
    }

    pub(super) fn failure(&self, canvas: CanvasId, node: NodeId) -> Option<&str> {
        self.failed.get(&(canvas, node)).map(String::as_str)
    }
}

impl Drop for Runs {
    fn drop(&mut self) {
        // With a journal, running jobs are saved and left at the provider for the next launch.
        self.host.shut_down();
        // Don't wait on a provider that is slow to answer a cancel: the window is closing.
        if let Some(tokio) = self.tokio.take() {
            tokio.shutdown_background();
        }
    }
}

/// `ids` and every node upstream of them on `canvas` that a `with_upstream` run would run too.
fn with_upstream(session: &Session, canvas: CanvasId, ids: &[NodeId], stale: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
    let mut seen: BTreeSet<NodeId> = ids.iter().copied().collect();
    let Some(canvas) = session.document().canvases().get(&canvas) else {
        return seen;
    };
    let mut inputs: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        inputs.entry(edge.to.node).or_default().push(edge.from.node);
    }
    // The graph may have cycles: the seen-set ends the walk.
    let mut next = ids.to_vec();
    while let Some(node) = next.pop() {
        for from in inputs.get(&node).into_iter().flatten() {
            // As the planner walks: a card with a current result is not run, and stops the walk.
            let runs = canvas.nodes().get(from).is_some_and(|up| stale.contains(from) || !has_result(up));
            if runs && seen.insert(*from) {
                next.push(*from);
            }
        }
    }
    seen
}

/// Whether a card has an active result to reuse.
fn has_result(node: &nodecraft_graph::Node) -> bool {
    node.active.is_some_and(|active| node.history.iter().any(|generation| generation.id == active))
}

/// The adapter each card with a catalog model on `canvas` runs on, and the technique node each
/// inlined one runs for.
fn adapters_of(session: &Session, canvas: CanvasId) -> (BTreeMap<NodeId, String>, BTreeMap<NodeId, NodeId>) {
    let (Some(canvas), Ok(catalog)) = (session.document().canvases().get(&canvas), session.generation_catalog()) else {
        return Default::default();
    };
    let catalog = catalog.run_catalog();
    // Routes are per node, so the nodes a run inlines for its technique nodes (#94) need theirs;
    // their ids are derived, the same for the run as here. A canvas that can't be inlined runs
    // only its own nodes.
    let expanded = nodecraft_exec::technique::expand(session.document().canvases(), canvas, session.kinds())
        .unwrap_or_else(|_| nodecraft_exec::technique::Expanded::plain(canvas));
    let adapters = expanded
        .canvas
        .nodes()
        .iter()
        .filter_map(|(id, node)| match node.params.get(nodecraft_nodes::generation::MODEL) {
            Some(nodecraft_types::Value::Text(model)) => catalog.catalog().get(model).map(|descriptor| (*id, descriptor.provider.clone())),
            _ => None,
        })
        .collect();
    (adapters, expanded.inner)
}
