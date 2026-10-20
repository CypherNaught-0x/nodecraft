//! The desktop app's runs (#409): the engine's [`RunHost`] on a tokio runtime, with provider
//! adapters built from the keychain on their own thread.
//!
//! Generate first makes sure the card's adapter is built ([`Runs::route`]). Building reads the
//! provider's key, which can show a keychain prompt, so it happens on the provider thread and
//! the quote waits for the answer ([`Tick::ready`]). Once the adapter is ready, the card is routed
//! to it and quoted as before. [`Runs::tick`] pumps the host once a frame and keeps what the
//! cards show: which are running and why the last run of one failed.
use nodecraft_engine::{
    Session,
    jobs::JobBudget,
    provider_setup::{self, HostEnv, ProviderSetup, Readiness},
    run_host::{RunHost, RunNotice},
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
        // thread asks.
        let timer = self.handle.spawn(tokio::time::sleep(Duration::from_millis(ms)));
        Box::pin(async move {
            let _ = timer.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

type Made = (String, Result<Arc<dyn Provider>, Readiness>);
/// Builds one adapter; the app's reads the keychain ([`provider_setup::make`]), tests pass mocks.
pub(super) type Make = Box<dyn Fn(&str, &ProviderSettings) -> Result<Arc<dyn Provider>, Readiness> + Send>;

/// The thread that builds adapters: it reads keys, so an unlock prompt never blocks a frame.
struct Maker {
    asks: SyncSender<(String, ProviderSettings)>,
    made: Receiver<Made>,
}

impl Maker {
    fn spawn(make: Make) -> Result<Self, Error> {
        let (asks, inbox) = sync_channel::<(String, ProviderSettings)>(ASKS);
        let (outbox, made) = sync_channel::<Made>(ASKS);
        std::thread::Builder::new()
            .name("nodecraft-providers".into())
            .spawn(move || {
                for (adapter, settings) in inbox {
                    // A keychain backend that panics fails this adapter, not the thread.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make(&adapter, &settings)))
                        .unwrap_or_else(|_| Err(Readiness::Failed("setting it up failed unexpectedly".into())));
                    if outbox.send((adapter, result)).is_err() {
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

/// What one frame's pump brought.
#[derive(Default)]
pub(super) struct Tick {
    /// A card whose provider is now set up, to quote again.
    pub ready: Option<NodeId>,
    /// Something to tell the person: a run that couldn't start or a provider that can't run.
    pub notice: Option<String>,
}

pub(super) struct Runs {
    host: RunHost,
    setup: ProviderSetup,
    maker: Maker,
    /// Adapters asked for and not yet answered.
    asked: BTreeSet<String>,
    /// The card waiting for its adapter, with that adapter.
    waiting: Option<(CanvasId, NodeId, String)>,
    /// Why the last run of a card failed, until it runs again.
    failed: BTreeMap<(CanvasId, NodeId), String>,
    running: BTreeSet<(CanvasId, NodeId)>,
    /// Runs the host's tasks; shut down without waiting when the app closes.
    tokio: Option<tokio::runtime::Runtime>,
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
        Ok(Self {
            host,
            setup: ProviderSetup::default(),
            maker,
            asked: BTreeSet::new(),
            waiting: None,
            failed: BTreeMap::new(),
            running: BTreeSet::new(),
            tokio: Some(tokio),
        })
    }

    /// Puts the budget resources in place, before the first quote.
    pub(super) fn install(&self, session: &mut Session) {
        self.host.install(session);
    }

    /// Routes card `id` to its provider when it is set up, or asks for it to be.
    pub(super) fn route(&mut self, session: &mut Session, canvas: CanvasId, id: NodeId, settings: &ProviderSettings) -> Result<Route, Error> {
        let adapters = adapters_of(session, canvas);
        let Some(adapter) = adapters.get(&id).cloned() else {
            return Ok(Route::Ready);
        };
        match self.setup.readiness(&adapter) {
            Some(Readiness::Ready) => {
                // Every card whose provider is ready, so a run of more than this card reaches its
                // providers too.
                let ready = adapters.iter().filter(|(_, a)| self.setup.readiness(a) == Some(&Readiness::Ready)).map(|(node, a)| (*node, a.as_str()));
                session.set_provider_routing(self.setup.routing(ready)?)?;
                Ok(Route::Ready)
            }
            Some(other) => {
                let message = other.message(&adapter).unwrap_or_default();
                // Tried afresh next time: the person may add the key or the server meanwhile.
                self.setup.forget(&adapter);
                Err(Error::new(ErrorKind::Unsupported, message))
            }
            None => {
                if !self.asked.contains(&adapter) {
                    match self.maker.asks.try_send((adapter.clone(), settings.clone())) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => return Err(Error::new(ErrorKind::Busy, "providers are still being set up; try again")),
                        Err(TrySendError::Disconnected(_)) => return Err(Error::new(ErrorKind::Internal, "the provider thread has stopped")),
                    }
                    self.asked.insert(adapter.clone());
                }
                self.waiting = Some((canvas, id, adapter));
                Ok(Route::Waiting)
            }
        }
    }

    /// Whether a card is waiting for its provider.
    pub(super) fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// Forgets every adapter, so the next run reads keys again (after one was saved or deleted).
    pub(super) fn forget_adapters(&mut self) {
        let adapters: Vec<String> = self.setup.adapters().map(|(adapter, _)| adapter.to_owned()).collect();
        for adapter in adapters {
            self.setup.forget(&adapter);
        }
    }

    /// One frame: takes built adapters and pumps the host.
    pub(super) fn tick(&mut self, session: &mut Session, canvas: CanvasId) -> Tick {
        let mut tick = Tick::default();
        for (adapter, result) in self.maker.made.try_iter() {
            self.asked.remove(&adapter);
            self.setup.forget(&adapter);
            self.setup.ensure(&adapter, |_| result);
            if let Some((on, node, wanted)) = &self.waiting
                && *wanted == adapter
            {
                if *on == canvas {
                    tick.ready = Some(*node);
                }
                self.waiting = None;
            }
        }
        for notice in self.host.pump(session) {
            match notice {
                RunNotice::Started { canvas, nodes } => {
                    for node in nodes {
                        self.failed.remove(&(canvas, node));
                    }
                }
                RunNotice::Recorded { canvas, node } => {
                    self.failed.remove(&(canvas, node));
                }
                RunNotice::Failed { canvas, node, reason } | RunNotice::Unaccounted { canvas, node, reason } => {
                    tick.notice = Some(format!("A run failed: {reason}"));
                    self.failed.insert((canvas, node), reason);
                }
                RunNotice::Refused { reason } => tick.notice = Some(format!("The run didn't start: {reason}")),
                RunNotice::Finished { .. } => {}
            }
        }
        self.running = self.host.running();
        tick
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
        self.host.cancel_all();
        // Don't wait on a provider that is slow to answer a cancel: the window is closing.
        if let Some(tokio) = self.tokio.take() {
            tokio.shutdown_background();
        }
    }
}

/// The adapter each card with a catalog model on `canvas` runs on.
fn adapters_of(session: &Session, canvas: CanvasId) -> BTreeMap<NodeId, String> {
    let (Some(canvas), Ok(catalog)) = (session.document().canvases().get(&canvas), session.generation_catalog()) else {
        return BTreeMap::new();
    };
    let catalog = catalog.run_catalog();
    canvas
        .nodes()
        .iter()
        .filter_map(|(id, node)| match node.params.get(nodecraft_nodes::generation::MODEL) {
            Some(nodecraft_types::Value::Text(model)) => catalog.catalog().get(model).map(|descriptor| (*id, descriptor.provider.clone())),
            _ => None,
        })
        .collect()
}
