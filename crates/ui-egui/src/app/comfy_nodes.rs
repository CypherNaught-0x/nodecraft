//! Reading the ComfyUI server's node list in the desktop app (#9): the engine's
//! `comfy.nodes.fetch` service, which `comfy.workflow.import` needs.
//!
//! As with option lists, the service only queues the request and answers `{pending: id}`. Each
//! frame [`ComfyNodesFetcher::tick`] hands a queued request, with the current provider settings
//! aimed at the server it names (the default when it names none, #864), to the fetch thread, which reads the server's token from the keychain and calls
//! `/object_info`, then `/system_stats` and `/queue` for the server panel's health line. The answer is installed
//! with `comfy_workflow_cmds::finish` and told as a notice. One fetch runs at a time; ordinary
//! concurrent requests are refused as busy, but panel changes queue bounded follow-up checks.
use nodecraft_engine::comfy_server_cmds;
use nodecraft_engine::comfy_workflow_cmds::{self, Fetched, NODES_SERVICE, Report};
use nodecraft_engine::provider_setup::HostEnv;
use nodecraft_engine::secrets::KeyringStore;
use nodecraft_engine::settings::ProviderSettings;
use nodecraft_engine::{Session, commands::Services};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

/// Reads the node list with the given settings; the app's reads the keychain and the network,
/// tests pass a fixture.
pub(super) type Fetch = Box<dyn FnMut(&ProviderSettings) -> Result<Fetched, Error> + Send>;

type Done = (u64, Result<Fetched, Error>);

/// The server a fetch reads, by name, and whether it is the default; `None` when there's no server.
type Aim = Option<(String, bool)>;

/// The `comfy.nodes.fetch` handler [`ComfyNodesFetcher::spawn`] registers.
pub(super) type Service = Box<dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, Error>>;

#[derive(Default)]
struct Asked {
    next: u64,
    /// A fetch asked for, with the server it names (`None` for the default).
    queued: VecDeque<(Vec<u64>, Option<String>)>,
    running: Option<(Vec<u64>, Aim)>,
    /// A running check may be abandoned without losing any later queued id.
    abandoned: Option<u64>,
    /// Only the panel's follow-up checks may queue behind a running check.
    followup: bool,
    #[cfg(test)]
    completed: Vec<u64>,
}

impl Asked {
    fn is_waiting(&self) -> bool {
        // Even an abandoned fetch must be drained and reported, so keep repainting.
        !self.queued.is_empty() || self.running.is_some()
    }
}

pub(super) struct ComfyNodesFetcher {
    asked: Rc<RefCell<Asked>>,
    asks: SyncSender<(u64, ProviderSettings)>,
    done: Receiver<Done>,
}

impl ComfyNodesFetcher {
    /// The fetch thread, over its own runtime and HTTP client, registered as `comfy.nodes.fetch`.
    pub(super) fn new(services: &mut Services) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the ComfyUI node fetch thread: {e}")))?;
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let start = std::time::Instant::now();
        let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
        let (fetcher, mut service) = Self::spawn(Box::new(move |settings| {
            let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings, now_ms: now_ms.clone(), progress: None };
            runtime.block_on(comfy_workflow_cmds::fetch(&env))
        }))?;
        services.register(NODES_SERVICE, move |request| service(request))?;
        Ok(fetcher)
    }

    /// The fetch thread over `fetch`, and the `comfy.nodes.fetch` handler that queues for it.
    pub(super) fn spawn(mut fetch: Fetch) -> Result<(Self, Service), Error> {
        let (asks, inbox) = sync_channel::<(u64, ProviderSettings)>(1);
        let (outbox, done) = sync_channel::<Done>(1);
        std::thread::Builder::new()
            .name("nodecraft-comfy-nodes".into())
            .spawn(move || {
                for (id, settings) in inbox {
                    // A keychain backend or reply that panics fails this fetch, not the thread.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(&settings)))
                        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "reading the ComfyUI server's nodes failed unexpectedly")));
                    if outbox.send((id, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the ComfyUI node fetch thread: {e}")))?;
        let asked = Rc::new(RefCell::new(Asked::default()));
        let queue = asked.clone();
        let service = move |request: &serde_json::Value| {
            let server = request.get("server").and_then(serde_json::Value::as_str).map(str::to_owned);
            let mut asked = queue.borrow_mut();
            // Reuse the pending id for repeated panel saves of this server: every
            // caller waits on the SAME terminal answer, and no additional queue
            // slots or network reads are consumed by edits made before the read.
            if asked.followup
                && let Some((ids, _)) = asked.queued.iter().find(|(_, queued_server)| *queued_server == server)
                && let Some(id) = ids.first()
            {
                return Ok(json!({"pending": id}));
            }
            if asked.queued.len() >= 64 || asked.next == u64::MAX {
                return Err(Error::new(ErrorKind::Busy, "too many ComfyUI node checks are waiting; try again after the queued checks finish"));
            }
            if asked.is_waiting() && !asked.followup {
                return Err(Error::new(ErrorKind::Busy, "the ComfyUI server's nodes are already being read; try again when that finishes"));
            }
            asked.next += 1;
            let id = asked.next;
            // The settings are read when this queued fetch actually starts.
            asked.queued.push_back((vec![id], server));
            Ok(json!({"pending": id}))
        };
        Ok((Self { asked, asks, done }, Box::new(service)))
    }

    #[cfg(test)]
    pub(super) fn completed_ids(&self) -> Vec<u64> {
        self.asked.borrow().completed.clone()
    }

    /// Abandons only the running fetch. Queued checks have their own accepted pending ids and
    /// must be answered, even when they belong to a different server.
    pub(super) fn abandon(&mut self) {
        let mut asked = self.asked.borrow_mut();
        asked.abandoned = asked.running.as_ref().and_then(|(ids, _)| ids.first().copied());
    }

    /// Temporarily permit a panel action to enqueue its follow-up check.
    pub(super) fn allow_followup(&mut self, allow: bool) {
        self.asked.borrow_mut().followup = allow;
    }

    /// Abandons the running fetch when it reads the server `name`, or the default server and
    /// `default_changed`: another server's check carries on. A queued fetch isn't sent yet and
    /// reads the settings in force when it is.
    pub(super) fn abandon_for(&mut self, name: &str, default_changed: bool) {
        let reads = match &self.asked.borrow().running {
            Some((_, Some((server, is_default)))) => server == name || (default_changed && *is_default),
            Some((_, None)) => default_changed,
            None => false,
        };
        if reads {
            self.abandon();
        }
    }

    /// Whether a fetch is queued or running.
    pub(super) fn is_waiting(&self) -> bool {
        self.asked.borrow().is_waiting()
    }

    /// Hands a queued request to the fetch thread and installs a finished fetch, returning what to
    /// tell the person about it.
    pub(super) fn tick(&mut self, session: &mut Session, settings: impl FnOnce() -> ProviderSettings) -> Option<String> {
        // Drain the previous answer before handing the thread another id. Otherwise a newly
        // started check could be cleared while settling the previous server's answer.
        let notice = match self.done.try_recv() {
            Ok((id, result)) => {
                let mut asked = self.asked.borrow_mut();
                let running = asked.running.take();
                let abandoned = asked.abandoned.take() == Some(id);
                drop(asked);
                running.filter(|(ids, _)| ids.first() == Some(&id)).map(|(ids, aim)| {
                    if abandoned { self.settle_all(session, ids, aim, Err(abandoned_error())) } else { self.settle_all(session, ids, aim, result) }
                })
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let mut asked = self.asked.borrow_mut();
                let running = asked.running.take();
                let abandoned = asked.abandoned.take();
                drop(asked);
                running.map(|(ids, aim)| {
                    let error = if abandoned == ids.first().copied() { abandoned_error() } else { stopped() };
                    self.settle_all(session, ids, aim, Err(error))
                })
            }
        };
        if self.asked.borrow().running.is_some() {
            return notice;
        }
        let queued = self.asked.borrow_mut().queued.pop_front();
        if let Some((ids, server)) = queued {
            let Some((settings, aim)) = aimed(settings(), server.as_deref()) else {
                // Every accepted id gets a status, even if its server was removed while waiting.
                let message = format!("There's no ComfyUI server called {}.", server.unwrap_or_default());
                self.settle_all(session, ids, None, Err(Error::new(ErrorKind::NotFound, message.clone())));
                return Some(message);
            };
            let Some(&id) = ids.first() else {
                return notice;
            };
            match self.asks.try_send((id, settings)) {
                Ok(()) => self.asked.borrow_mut().running = Some((ids, aim)),
                Err(TrySendError::Full(_)) => self.asked.borrow_mut().queued.push_front((ids, server)),
                Err(TrySendError::Disconnected(_)) => return Some(self.settle_all(session, ids, aim, Err(stopped()))),
            }
        }
        notice
    }

    /// Complete each accepted id exactly once, sharing one network outcome without another read.
    fn settle_all(&mut self, session: &mut Session, ids: Vec<u64>, aim: Aim, result: Result<Fetched, Error>) -> String {
        let mut notice = String::new();
        for id in ids {
            let shared = match &result {
                Ok(fetched) => Ok(Fetched { nodes: fetched.nodes.clone(), health: fetched.health.clone() }),
                Err(error) => Err(error.clone()),
            };
            notice = self.settle(session, id, aim.clone(), shared);
            #[cfg(test)]
            self.asked.borrow_mut().completed.push(id);
        }
        notice
    }

    fn settle(&mut self, session: &mut Session, id: u64, aim: Aim, result: Result<Fetched, Error>) -> String {
        match aim {
            Some((name, is_default)) => {
                let report = comfy_server_cmds::finish(session, id, &name, is_default, result);
                if is_default { notice(&report) } else { format!("{name}: {}", notice(&report)) }
            }
            None => notice(&comfy_workflow_cmds::finish(session, id, result)),
        }
    }
}

/// `settings` aimed at the server called `server` (the default when `None`), and which server
/// that is; `None` when there's no server called `server`.
fn aimed(mut settings: ProviderSettings, server: Option<&str>) -> Option<(ProviderSettings, Aim)> {
    let default = settings.default_comfy().map(|server| server.name.clone());
    match server {
        Some(name) if settings.comfy_server(name).is_some() => {
            let is_default = default.as_deref() == Some(name);
            settings.comfyui_default = Some(name.to_owned());
            Some((settings, Some((name.to_owned(), is_default))))
        }
        Some(_) => None,
        None => Some((settings, default.map(|name| (name, true)))),
    }
}

fn abandoned_error() -> Error {
    Error::new(ErrorKind::Conflict, "the ComfyUI node check was abandoned because the server changed; check the updated server's result")
}

fn stopped() -> Error {
    Error::new(ErrorKind::Unsupported, "the ComfyUI node fetch thread stopped")
}

/// One line about a fetch.
pub(super) fn notice(report: &Report) -> String {
    match (&report.error, report.nodes) {
        (Some(error), _) => format!("ComfyUI's nodes weren't read: {error}"),
        (None, Some(nodes)) => {
            let read = format!("Read {nodes} ComfyUI node type{}; workflows can be imported.", if nodes == 1 { "" } else { "s" });
            match &report.server {
                Some(health) => format!("{read} {}.", health.summary()),
                None => read,
            }
        }
        (None, None) => "ComfyUI's nodes weren't read.".into(),
    }
}

impl super::NodeCraftApp {
    /// Hands on and installs node list fetches; repaints while one runs so its answer is seen.
    pub(super) fn tick_comfy_nodes(&mut self, ctx: &egui::Context) {
        let Some(fetcher) = &mut self.comfy_nodes else {
            return;
        };
        let store = &self.store;
        if let Some(notice) = fetcher.tick(&mut self.session, || store.as_ref().map(|store| store.get().providers.clone()).unwrap_or_default()) {
            self.notice = Some(notice);
        }
        if fetcher.is_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests;
