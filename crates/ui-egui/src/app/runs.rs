//! The app's runs (#409): the engine's [`RunHost`] on a tokio runtime, with provider adapters
//! built from the keychain on their own thread. In the browser (#118) the host runs on the page's
//! queue and adapters are built at once from the keys the page keeps ([`browser`]).
//!
//! Generate first makes sure the card's adapter is built ([`Runs::route`]). Building reads the
//! provider's key, which can show a keychain prompt, so it happens on the provider thread and
//! the quote waits for the answer ([`Tick::ready`]). Once the adapter is ready, the card is routed
//! to it and quoted as before. [`Runs::tick`] pumps the host once a frame and keeps what the
//! cards show: which are running and why the last run of one failed.
//!
//! With a run journal ([`Runs::open_journal`], #508, desktop only) jobs cut off by quitting or a crash are
//! followed again once their provider is set up ([`Runs::resume_waiting`]), and quitting leaves
//! running jobs at the provider instead of cancelling them.
use super::generate::RunKind;
use nodecraft_engine::{
    Session,
    jobs::JobBudget,
    plugin_runtime::{PluginSnapshot, is_plugin},
    provider_routing::{ProviderRouting, RegisteredProvider},
    provider_setup::{self, HostEnv, ProviderSetup, Readiness},
    run_host::{HeldView, RunHost, RunNotice},
    run_jobs::{JobKey, JobState, JobView},
    secrets::SecretStore,
    settings::ProviderSettings,
};
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{held_store::FileHeld, run_journal::FileJournal};
use nodecraft_exec::cost::{Budgets, UsageLog};
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_exec::driver::{Runtime, TaskFuture};
use nodecraft_providers::{Provider, http::Http};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

#[cfg(target_arch = "wasm32")]
mod browser;

/// Adapters the provider thread may be asked for before it answers; more fail as busy.
#[cfg(not(target_arch = "wasm32"))]
const ASKS: usize = 16;

/// The engine's runtime over a tokio runtime the app owns.
#[cfg(not(target_arch = "wasm32"))]
struct Tokio {
    handle: tokio::runtime::Handle,
    start: Instant,
}

#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
struct Timer(tokio::task::JoinHandle<()>);
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
pub(super) type Make = Box<dyn Fn(&str, &ProviderSettings) -> Result<Arc<dyn Provider>, Readiness> + Send>;
/// In the browser adapters are built on the page's one thread.
#[cfg(target_arch = "wasm32")]
pub(super) type Make = Box<dyn Fn(&str, &ProviderSettings) -> Result<Arc<dyn Provider>, Readiness>>;

/// Builds plugin brokers (#174) on a client that reaches public addresses only, so a template
/// can't aim a request at the person's own network.
struct PluginMaker {
    board: Board,
    #[cfg(not(target_arch = "wasm32"))]
    http: Result<Arc<dyn Http>, String>,
}

impl PluginMaker {
    fn new(board: Board) -> Self {
        Self {
            board,
            #[cfg(not(target_arch = "wasm32"))]
            http: nodecraft_providers::http::ReqwestHttp::public_only().map(|h| Arc::new(h) as Arc<dyn Http>).map_err(|e| e.message),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn make(&self, adapter: &str, keys: &dyn SecretStore) -> Result<Arc<dyn Provider>, Readiness> {
        let provider = self.board.lock().unwrap_or_else(std::sync::PoisonError::into_inner).provider(adapter).cloned();
        let provider = provider.ok_or_else(|| Readiness::NotSetUp("that plugin isn't enabled".into()))?;
        let http = self.http.clone().map_err(Readiness::Failed)?;
        nodecraft_engine::plugin_providers::broker(adapter, &provider, keys, http)
    }

    /// The browser can't keep a request to public addresses, so plugin providers don't run there.
    #[cfg(target_arch = "wasm32")]
    fn make(&self, _: &str, _: &dyn SecretStore) -> Result<Arc<dyn Provider>, Readiness> {
        let _ = &self.board;
        Err(Readiness::NotSetUp("plugin providers run in the desktop app only".into()))
    }
}

/// Builds adapters as runs ask for them.
enum Maker {
    /// On a thread: the keychain may show an unlock prompt, which must never block a frame.
    #[cfg(not(target_arch = "wasm32"))]
    Thread { asks: SyncSender<Ask>, made: Receiver<Made> },
    /// At once, in the browser: its key store answers without a prompt. They are handed over on
    /// the next tick, as the thread's are.
    #[cfg(any(test, target_arch = "wasm32"))]
    Inline { make: Make, made: Vec<Made> },
}

impl Maker {
    #[cfg(not(target_arch = "wasm32"))]
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
        Ok(Self::Thread { asks, made })
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn(make: Make) -> Result<Self, Error> {
        Ok(Self::inline(make))
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn inline(make: Make) -> Self {
        Self::Inline { make, made: Vec::new() }
    }

    fn ask(&mut self, ask: Ask) -> Result<(), Error> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Thread { asks, .. } => match asks.try_send(ask) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => Err(Error::new(ErrorKind::Busy, "providers are still being set up; try again")),
                Err(TrySendError::Disconnected(_)) => Err(Error::new(ErrorKind::Internal, "the provider thread has stopped")),
            },
            #[cfg(any(test, target_arch = "wasm32"))]
            Self::Inline { make, made } => {
                let (epoch, adapter, settings) = ask;
                let result = make(&adapter, &settings);
                made.push((epoch, adapter, result));
                Ok(())
            }
        }
    }

    fn made(&mut self) -> Vec<Made> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Thread { made, .. } => made.try_iter().collect(),
            #[cfg(any(test, target_arch = "wasm32"))]
            Self::Inline { made, .. } => std::mem::take(made),
        }
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
    /// Providers answered `run.estimate` requests (#663), or they couldn't ask them.
    pub estimated: Vec<Estimated>,
    /// A Soul ID training was recorded, so the account's Soul IDs are worth fetching again (#680).
    pub trained: bool,
}

/// How the `run.estimate` that returned `request` ended.
pub(super) enum Estimated {
    /// The providers answered; `unquoted` jobs had no price.
    Answered {
        request: u64,
        unquoted: usize,
    },
    Failed {
        request: u64,
        reason: String,
    },
}

/// The catalog model whose recorded result is a new identity on the Higgsfield account.
const SOUL_TRAINER: &str = "higgsfield.soul-id-trainer";

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
    /// Paid results the host couldn't keep, waiting for a decision (#369).
    held: Vec<HeldView>,
    /// Runs the host's tasks; shut down without waiting when the app closes.
    #[cfg(not(target_arch = "wasm32"))]
    tokio: Option<tokio::runtime::Runtime>,
    /// Why the run journal couldn't be opened, told on the first frame.
    journal_problem: Option<String>,
    /// The plugin providers the provider thread may build brokers for (#174).
    plugins: Board,
    /// Whether the plugin store has been read, so a plugin missing from `plugins` is gone and
    /// not just still loading.
    plugins_known: bool,
}

/// The session's plugins as the provider thread last saw them.
type Board = Arc<Mutex<Arc<PluginSnapshot>>>;

impl Runs {
    /// The runtime, HTTP client and provider thread, with adapters reading their keys from
    /// `keys`; registers `run.start` in `services`.
    pub(super) fn new(services: &mut nodecraft_engine::commands::Services, keys: Arc<dyn SecretStore>) -> Result<Self, Error> {
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let now_ms = monotonic_ms();
        // ComfyUI's progress socket; the browser reports progress by polling only.
        #[cfg(not(target_arch = "wasm32"))]
        let progress: Option<Arc<dyn nodecraft_providers::comfyui::progress::Feeds>> =
            Some(Arc::new(nodecraft_providers::comfyui::progress::native::NativeFeeds));
        #[cfg(target_arch = "wasm32")]
        let progress: Option<Arc<dyn nodecraft_providers::comfyui::progress::Feeds>> = None;
        let board = Board::default();
        let plugins = PluginMaker::new(Arc::clone(&board));
        let mut runs = Self::with_maker(
            services,
            Box::new(move |adapter, settings| {
                if is_plugin(adapter) {
                    return plugins.make(adapter, keys.as_ref());
                }
                let env = HostEnv { keys: keys.as_ref(), http: http.clone(), settings, now_ms: now_ms.clone(), progress: progress.clone() };
                provider_setup::make(adapter, &env)
            }),
        )?;
        runs.plugins = board;
        Ok(runs)
    }

    /// Publishes the session's plugin providers to the provider thread (#174). When they
    /// changed, every adapter is forgotten, so no run goes to a broker built under a grant that
    /// is gone (it would refuse anyway) and a new provider is built afresh. `loaded` says the
    /// plugin store was just read in full and published, so restored jobs of a plugin that isn't
    /// there can be given up; until the next such read, they wait.
    pub(super) fn set_plugins(&mut self, session: &mut Session, loaded: bool) -> Result<(), Error> {
        self.plugins_known = loaded;
        let next = Arc::clone(session.plugins());
        let changed = {
            let mut board = self.plugins.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = !board.same_providers(&next);
            *board = next;
            changed
        };
        if changed {
            self.forget_adapters(session)?;
        }
        Ok(())
    }

    /// [`Self::new`] with adapters built by `make`.
    pub(super) fn with_maker(services: &mut nodecraft_engine::commands::Services, make: Make) -> Result<Self, Error> {
        Self::on(services, Maker::spawn(make)?)
    }

    /// [`Self::with_maker`] building adapters at once, as the browser does.
    #[cfg(test)]
    pub(super) fn with_inline_maker(services: &mut nodecraft_engine::commands::Services, make: Make) -> Result<Self, Error> {
        Self::on(services, Maker::inline(make))
    }

    fn on(services: &mut nodecraft_engine::commands::Services, maker: Maker) -> Result<Self, Error> {
        #[cfg(not(target_arch = "wasm32"))]
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("nodecraft-runs")
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the run threads: {e}")))?;
        #[cfg(not(target_arch = "wasm32"))]
        let runtime = Arc::new(Tokio { handle: tokio.handle().clone(), start: Instant::now() });
        #[cfg(target_arch = "wasm32")]
        let runtime = Arc::new(browser::Page::new());
        let mut id = [0u8; 8];
        getrandom::fill(&mut id).map_err(|_| Error::new(ErrorKind::Internal, "session identity entropy is unavailable"))?;
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: id.iter().map(|b| format!("{b:02x}")).collect(),
            wall_clock_ms: wall_clock_ms(),
        };
        let host = RunHost::new(runtime, budget);
        host.register(services)?;
        host.register_estimates(services)?;
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
            held: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tokio: Some(tokio),
            journal_problem: None,
            plugins: Board::default(),
            plugins_known: false,
        })
    }

    /// The caps later quotes and jobs are checked against (#201).
    pub(super) fn set_budgets(&self, session: &mut Session, budgets: Budgets) {
        self.host.set_budgets(session, budgets);
    }

    /// What this session, everything and the open project have spent so far.
    pub(super) fn spent(&self, session: &Session) -> nodecraft_exec::cost::Spent {
        self.host.spent(Some(session.document().id()))
    }

    /// Checks quotes against the open project's spending, after another project was opened.
    pub(super) fn project_opened(&self, session: &mut Session) {
        self.host.install(session);
    }

    /// The ffmpeg and ffprobe the settings name, for the Timeline's renders (#109).
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn set_video_tools(&mut self, tools: Option<(&std::path::Path, &std::path::Path)>) {
        self.host.set_video_tools(tools);
    }

    /// Keeps runs in the journal at `path` across restarts (see the module docs), restoring
    /// what it holds, and held results in the `held` folder beside it (#614). Call before
    /// [`Self::install`], and only while this window owns `path`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn open_journal(&mut self, path: std::path::PathBuf) {
        let held = path.with_file_name("held");
        let mut problems = Vec::new();
        if let Err(error) = self.host.open_journal(Box::new(FileJournal::at(path))) {
            log::warn!("run journal: {}", error.message);
            problems.push(format!("Runs from the last session couldn't be restored: {}", error.message));
        }
        if let Err(error) = self.host.open_held(Box::new(FileHeld::at(held))) {
            log::warn!("held results: {}", error.message);
            problems.push(format!("Paid results held from the last session: {}", error.message));
        }
        // Restored results show in the queue panel before anything runs.
        self.held = self.host.held_results();
        if !problems.is_empty() {
            self.journal_problem = Some(problems.join("\n"));
        }
    }

    /// Follows jobs restored from the journal whose provider is ready, and asks for the
    /// providers of the others. A provider that can't run leaves its jobs waiting (their charges
    /// stay counted) until it can, or until the next launch.
    pub(super) fn resume_waiting(&mut self, session: &Session, settings: &ProviderSettings) {
        if !self.host.has_waiting() {
            return;
        }
        let project = session.document().id();
        for target in self.host.waiting_targets(project) {
            let adapter = target.adapter().to_owned();
            if is_plugin(&adapter) {
                // A plugin's broker is built from its grant, which is known once the store is read.
                if !self.plugins_known {
                    continue;
                }
                let granted = self.plugins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).provider(&adapter).is_some();
                if !granted {
                    self.host.abandon(project, &adapter, "its plugin is no longer enabled or granted");
                    continue;
                }
            }
            // A ComfyUI job goes back to the server it was sent to (#864); one sent before servers
            // had names, to the default server. A server since removed leaves its jobs waiting.
            let slot = match (adapter.as_str(), target.instance()) {
                ("comfyui", Some(instance)) => {
                    let server = settings.comfy_servers().into_iter().find(|server| provider_setup::comfy_instance(&server.name) == Some(instance));
                    let Some(server) = server else { continue };
                    provider_setup::comfy_slot(&server.name)
                }
                ("comfyui", None) => settings.default_comfy().map_or_else(|| adapter.clone(), |server| provider_setup::comfy_slot(&server.name)),
                _ => adapter.clone(),
            };
            match self.setup.readiness(&slot) {
                Some(Readiness::Ready) => {
                    let binding = self.setup.provider(&slot).map(|provider| match target.instance() {
                        Some(instance) => RegisteredProvider::named(Arc::clone(provider), instance),
                        None => RegisteredProvider::default_instance(Arc::clone(provider)),
                    });
                    match binding {
                        Some(Ok(binding)) => self.host.resume(session, &binding),
                        Some(Err(error)) => log::warn!("can't resume jobs on {adapter}: {}", error.message),
                        None => {}
                    }
                }
                Some(_) => {}
                None => {
                    // Asked again on a later frame if the thread is busy.
                    let _ = self.ask(&slot, settings);
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
    #[cfg(not(target_arch = "wasm32"))]
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
        let (adapters, inner) = adapters_of(session, canvas, settings);
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
                    unready.push(other.message(provider_setup::slot_adapter(&adapter)).unwrap_or_default());
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
        session.set_provider_routing(self.setup.routing_for(ready, settings)?)?;
        Ok(Route::Ready)
    }

    /// Asks the provider thread for `adapter`, unless it already was in this epoch.
    fn ask(&mut self, adapter: &str, settings: &ProviderSettings) -> Result<(), Error> {
        if self.asked.contains(adapter) {
            return Ok(());
        }
        self.maker.ask((self.epoch, adapter.to_owned(), settings.clone()))?;
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
        self.rebuild(session)
    }

    /// Forgets the adapter of the ComfyUI server called `name` alone, after it changed or was
    /// removed (#864): other servers and providers keep theirs. Builds in flight are asked again
    /// with `settings`, since they read the old ones.
    pub(super) fn forget_comfy_server(&mut self, session: &mut Session, name: &str, settings: &ProviderSettings) -> Result<(), Error> {
        if let Some(waiting) = &mut self.waiting {
            waiting.settings = settings.clone();
        }
        self.setup.forget(&provider_setup::comfy_slot(name));
        // A card that found no server tries again.
        self.setup.forget("comfyui");
        self.rebuild(session)
    }

    /// Drops builds in flight and the session's routing, and asks again for what a waiting card
    /// needs.
    fn rebuild(&mut self, session: &mut Session) -> Result<(), Error> {
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
        let made = self.maker.made();
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
                RunNotice::Recorded { canvas, node } => {
                    self.failed.remove(&(canvas, node));
                    let card = session.document().canvases().get(&canvas).and_then(|c| c.nodes().get(&node));
                    tick.trained |= card.is_some_and(|card| matches!(card.params.get("model"), Some(Value::Text(model)) if model == SOUL_TRAINER));
                }
                RunNotice::Cancelled { canvas, node } => {
                    self.failed.remove(&(canvas, node));
                }
                RunNotice::Failed { canvas, node, reason } | RunNotice::Unaccounted { canvas, node, reason } => {
                    notices.push(format!("A run failed: {reason}"));
                    self.failed.insert((canvas, node), reason);
                }
                RunNotice::Placed { canvas, node } => {
                    notices.push("The held result is on the canvas.".into());
                    self.failed.remove(&(canvas, node));
                }
                RunNotice::Refused { reason } => notices.push(format!("The run didn't start: {reason}")),
                RunNotice::Unsaved { reason } => notices.push(format!("Runs can't be kept for a restart: {reason}")),
                RunNotice::Estimated { request, unquoted, .. } => tick.estimated.push(Estimated::Answered { request, unquoted }),
                RunNotice::NotEstimated { request, reason } => tick.estimated.push(Estimated::Failed { request, reason }),
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
        self.held = self.host.held_results();
        tick
    }

    /// The queue panel's jobs: recently ended ones first, then those queued or running.
    pub(super) fn jobs(&self) -> &[JobView] {
        &self.jobs
    }

    /// The paid results waiting for a decision, oldest first.
    pub(super) fn held(&self) -> &[HeldView] {
        &self.held
    }

    /// Reads the held results again, after a decision about one.
    pub(super) fn refresh_held(&mut self) {
        self.held = self.host.held_results();
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
        self.host.is_busy() || self.host.is_estimating() || !self.asked.is_empty()
    }

    pub(super) fn is_running(&self, canvas: CanvasId, node: NodeId) -> bool {
        self.running.contains(&(canvas, node))
    }

    pub(super) fn failure(&self, canvas: CanvasId, node: NodeId) -> Option<&str> {
        self.failed.get(&(canvas, node)).map(String::as_str)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Runs {
    pub(super) fn shut_down(&mut self) {
        self.host.shut_down();
    }
}

impl Drop for Runs {
    fn drop(&mut self) {
        // on_exit cancels before the GPU drain; Drop also covers non-eframe owners such as
        // Harnesses, so keep this fallback even when shutdown was already requested.
        // With a journal, running jobs are saved and left at the provider for the next launch.
        self.host.shut_down();
        // Don't wait on a provider that is slow to answer a cancel: the window is closing.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(tokio) = self.tokio.take() {
            tokio.shutdown_background();
        }
    }
}

/// Milliseconds on a monotonic clock, for the adapters (ComfyUI's prompt bookkeeping).
#[cfg(not(target_arch = "wasm32"))]
fn monotonic_ms() -> Arc<dyn Fn() -> u64 + Send + Sync> {
    let start = Instant::now();
    Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
}
#[cfg(target_arch = "wasm32")]
fn monotonic_ms() -> Arc<dyn Fn() -> u64 + Send + Sync> {
    Arc::new(browser::now_ms)
}

/// Wall-clock milliseconds since the Unix epoch, for the usage log.
#[cfg(not(target_arch = "wasm32"))]
fn wall_clock_ms() -> Arc<dyn Fn() -> u64 + Send + Sync> {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)))
}
#[cfg(target_arch = "wasm32")]
fn wall_clock_ms() -> Arc<dyn Fn() -> u64 + Send + Sync> {
    // `SystemTime::now` isn't there in the browser.
    Arc::new(|| browser::millis(js_sys::Date::now()))
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

/// The setup key each card with a catalog model on `canvas` runs on (its adapter, or the slot of
/// the ComfyUI server it uses, #864), and the technique node each inlined one runs for.
fn adapters_of(session: &Session, canvas: CanvasId, settings: &ProviderSettings) -> (BTreeMap<NodeId, String>, BTreeMap<NodeId, NodeId>) {
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
            Some(nodecraft_types::Value::Text(model)) => {
                catalog.catalog().get(model).map(|descriptor| (*id, provider_setup::slot_for(&descriptor.provider, node, settings)))
            }
            _ => None,
        })
        .collect();
    (adapters, expanded.inner)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod plugin_maker_tests {
    use super::*;

    #[test]
    fn a_plugin_provider_runs_only_while_the_session_publishes_it() {
        let maker = PluginMaker::new(Board::default());
        let keys = nodecraft_engine::secrets::MemoryStore::default();
        match maker.make("plugin.io.example.acme.acme", &keys) {
            Err(Readiness::NotSetUp(why)) => assert_eq!(why, "that plugin isn't enabled"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("built a broker for a plugin nobody enabled"),
        }
    }

    /// #174: a job a plugin provider was running when the app quit waits until the plugin store
    /// has been read; if its plugin isn't granted then, it is given up and its charge settled as
    /// unresolved, without asking for a provider.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_restored_job_of_a_plugin_that_is_gone_is_given_up_once_the_store_is_read() {
        use nodecraft_engine::run_journal::{JOURNAL_VERSION, Journal, JournalStore, Submitted};
        use nodecraft_exec::cost::{Charge, UsageEntry};
        use nodecraft_types::{Currency, Money, ProjectId, ProviderTarget};
        const PLUGIN: &str = "plugin.io.example.acme.acme";
        const P: ProjectId = ProjectId::from_bytes([71; 16]);
        const C: CanvasId = CanvasId::from_bytes([72; 16]);
        const N: NodeId = NodeId::from_bytes([73; 16]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.json");
        let cost = Money::new(Currency::USD, 40_000);
        let mut usage = UsageLog::default();
        usage
            .record(UsageEntry {
                at_ms: 0,
                session: "before".into(),
                node: N,
                provider: PLUGIN.into(),
                instance: None,
                model: format!("{PLUGIN}/v1"),
                cost,
                idempotency_key: nodecraft_engine::jobs::sent_key("k", 0),
                status: Charge::Estimate,
                project: Some(P),
            })
            .unwrap();
        let job = Submitted {
            project: P,
            canvas: C,
            node: N,
            target: ProviderTarget::new(PLUGIN, None).unwrap(),
            key: "k".into(),
            generation: 0,
            handle: nodecraft_providers::JobHandle { remote_id: "job-1".into(), data: serde_json::Value::Null },
            model: format!("{PLUGIN}/v1"),
            estimate: Some(cost),
            priced: true,
            routes: Vec::new(),
            recipe: None,
            launches: 0,
            staged: None,
        };
        let journal = nodecraft_engine::run_journal::FileJournal::at(path.clone());
        journal.save(&Journal { version: JOURNAL_VERSION, usage, submitted: vec![job] }).unwrap();

        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&asked);
        let mut services = nodecraft_engine::commands::Services::default();
        let mut runs = Runs::with_inline_maker(
            &mut services,
            Box::new(move |adapter, _| {
                seen.lock().unwrap().push(adapter.to_owned());
                Err(Readiness::NotSetUp("not in this test".into()))
            }),
        )
        .unwrap();
        runs.open_journal(path);
        let mut project = nodecraft_graph::Project::new(P, "Plugin resume").unwrap();
        nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let mut session = Session::new(project).unwrap().with_services(services);
        runs.install(&mut session);
        let settings = ProviderSettings::default();
        assert!(runs.has_waiting());

        // The store hasn't been read: an empty board says nothing yet.
        runs.set_plugins(&mut session, false).unwrap();
        runs.resume_waiting(&session, &settings);
        runs.tick(&mut session, C);
        assert!(runs.has_waiting(), "kept until the store is read");
        // A full read followed by one that wasn't (or whose plugins couldn't be published)
        // knows nothing again.
        runs.set_plugins(&mut session, true).unwrap();
        runs.set_plugins(&mut session, false).unwrap();
        runs.resume_waiting(&session, &settings);
        assert!(runs.has_waiting(), "kept while the last read is partial");
        // Read, and the plugin isn't there.
        runs.set_plugins(&mut session, true).unwrap();
        runs.resume_waiting(&session, &settings);
        assert!(!runs.has_waiting());
        let tick = runs.tick(&mut session, C);
        assert_eq!(tick.notice.as_deref(), Some("A run failed: it couldn't be followed after a restart: its plugin is no longer enabled or granted"));
        assert!(runs.failure(C, N).is_some());
        assert!(asked.lock().unwrap().is_empty(), "no provider was asked for: {:?}", asked.lock().unwrap());
        let saved = journal.load().unwrap().unwrap();
        assert!(saved.submitted.is_empty());
        assert_eq!(saved.usage.entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved], "it may have been billed");
    }
}
