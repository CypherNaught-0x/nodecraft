//! Option lists handed over on later frames. The desktop worker reads the keychain and blocks
//! on its private runtime; the browser polls an owned future on the page's queue (#721).
use nodecraft_engine::option_fetch::{self, FetchRequest, Fetched, OPTION_SERVICE, Report};
use nodecraft_engine::provider_setup::HostEnv;
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::secrets::KeyringStore;
use nodecraft_engine::settings::ProviderSettings;
use nodecraft_engine::{Session, commands::Services};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

#[cfg(not(target_arch = "wasm32"))]
pub(super) type Fetch = Box<dyn FnMut(&[String], &ProviderSettings) -> Result<Fetched, Error> + Send>;
#[cfg(any(test, target_arch = "wasm32"))]
type FetchFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Fetched, Error>>>>;
#[cfg(any(test, target_arch = "wasm32"))]
type AsyncFetch = Box<dyn FnMut(Vec<String>, ProviderSettings) -> FetchFuture>;
#[cfg(not(target_arch = "wasm32"))]
type Ask = (u64, Vec<String>, ProviderSettings);
#[cfg(not(target_arch = "wasm32"))]
type Done = (u64, Result<Fetched, Error>);
pub(super) type Service = Box<dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, Error>>;

#[derive(Default)]
struct Asked {
    next: u64,
    queued: Option<(u64, Vec<String>)>,
    running: Option<u64>,
    busy: bool,
}

enum Worker {
    #[cfg(not(target_arch = "wasm32"))]
    Thread { asks: SyncSender<Ask>, done: Receiver<Done> },
    #[cfg(any(test, target_arch = "wasm32"))]
    Async { fetch: AsyncFetch, pending: Option<(u64, FetchFuture)> },
}

pub(super) struct OptionFetcher {
    asked: Rc<RefCell<Asked>>,
    worker: Worker,
}

impl OptionFetcher {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn new(services: &mut Services) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the option fetch thread: {e}")))?;
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let now_ms = super::runs::monotonic_ms();
        let (fetcher, mut service) = Self::spawn(Box::new(move |sources, settings| {
            let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings, now_ms: now_ms.clone(), progress: None };
            runtime.block_on(option_fetch::fetch(&env, sources))
        }))?;
        services.register(OPTION_SERVICE, move |request| service(request))?;
        Ok(fetcher)
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn browser(services: &mut Services, keys: Arc<dyn nodecraft_engine::secrets::SecretStore>) -> Result<Self, Error> {
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let now_ms = super::runs::monotonic_ms();
        let (fetcher, mut service) = Self::asynchronous(Box::new(move |sources, settings| {
            let (keys, http, now_ms) = (Arc::clone(&keys), Arc::clone(&http), Arc::clone(&now_ms));
            Box::pin(async move {
                let env = HostEnv { keys: keys.as_ref(), http, settings: &settings, now_ms, progress: None };
                option_fetch::fetch(&env, &sources).await
            })
        }));
        services.register(OPTION_SERVICE, move |request| service(request))?;
        Ok(fetcher)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn spawn(mut fetch: Fetch) -> Result<(Self, Service), Error> {
        let (asks, inbox) = sync_channel::<Ask>(1);
        let (outbox, done) = sync_channel::<Done>(1);
        std::thread::Builder::new()
            .name("nodecraft-options".into())
            .spawn(move || {
                for (id, sources, settings) in inbox {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(&sources, &settings)))
                        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "fetching option lists failed unexpectedly")));
                    if outbox.send((id, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the option fetch thread: {e}")))?;
        Ok(Self::with_worker(Worker::Thread { asks, done }))
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn asynchronous(fetch: AsyncFetch) -> (Self, Service) {
        Self::with_worker(Worker::Async { fetch, pending: None })
    }

    fn with_worker(worker: Worker) -> (Self, Service) {
        let asked = Rc::new(RefCell::new(Asked::default()));
        let queue = Rc::clone(&asked);
        let service = move |request: &serde_json::Value| {
            let request: FetchRequest =
                serde_json::from_value(request.clone()).map_err(|_| Error::new(ErrorKind::InvalidInput, "malformed option fetch request"))?;
            let mut asked = queue.borrow_mut();
            if asked.busy {
                return Err(Error::new(ErrorKind::Busy, "option lists are already being fetched; try again when that finishes"));
            }
            asked.next = asked.next.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "too many option requests; reopen the app"))?;
            let id = asked.next;
            asked.queued = Some((id, request.sources));
            asked.busy = true;
            Ok(json!({"pending": id}))
        };
        (Self { asked, worker }, Box::new(service))
    }

    pub(super) fn is_waiting(&self) -> bool {
        self.asked.borrow().busy
    }

    /// A new endpoint invalidates both queued work and any old server's late answer.
    pub(super) fn invalidate(&mut self) {
        let mut asked = self.asked.borrow_mut();
        asked.queued = None;
        asked.running = None;
        asked.busy = false;
        match &mut self.worker {
            #[cfg(not(target_arch = "wasm32"))]
            Worker::Thread { .. } => {}
            #[cfg(any(test, target_arch = "wasm32"))]
            Worker::Async { pending, .. } => *pending = None,
        }
    }

    pub(super) fn tick(&mut self, session: &mut Session, settings: impl FnOnce() -> ProviderSettings, _ctx: &egui::Context) -> Option<String> {
        let queued = self.asked.borrow_mut().queued.take();
        if let Some((id, sources)) = queued {
            match &mut self.worker {
                #[cfg(not(target_arch = "wasm32"))]
                Worker::Thread { asks, .. } => {
                    if asks.try_send((id, sources, settings())).is_err() {
                        return Some(self.stopped(session, id));
                    }
                }
                #[cfg(any(test, target_arch = "wasm32"))]
                Worker::Async { fetch, pending } => {
                    *pending = Some((id, fetch(sources, settings())));
                    self.asked.borrow_mut().running = Some(id);
                    _ctx.request_repaint();
                    return None;
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.asked.borrow_mut().running = Some(id);
            }
        }
        let (id, result) = match &mut self.worker {
            #[cfg(not(target_arch = "wasm32"))]
            Worker::Thread { done, .. } => match done.try_recv() {
                Ok(done) => done,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    let running = self.asked.borrow_mut().running.take();
                    return running.map(|id| self.stopped(session, id));
                }
            },
            #[cfg(any(test, target_arch = "wasm32"))]
            Worker::Async { pending, .. } => {
                let (id, mut future) = pending.take()?;
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::web_files::poll(&mut future, _ctx))) {
                    Ok(std::task::Poll::Pending) => {
                        *pending = Some((id, future));
                        return None;
                    }
                    Ok(std::task::Poll::Ready(result)) => (id, result),
                    Err(_) => (id, Err(Error::new(ErrorKind::Internal, "fetching option lists failed unexpectedly"))),
                }
            }
        };
        if self.asked.borrow().running != Some(id) {
            return None;
        }
        let mut asked = self.asked.borrow_mut();
        asked.busy = false;
        asked.running = None;
        drop(asked);
        let report = option_fetch::finish(session, id, result);
        log::info!("option lists: {} installed, {} refused", report.installed.len(), report.refused.len());
        Some(notice(&report))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn stopped(&mut self, session: &mut Session, id: u64) -> String {
        let mut asked = self.asked.borrow_mut();
        asked.busy = false;
        asked.running = None;
        drop(asked);
        let report = option_fetch::finish(session, id, Err(Error::new(ErrorKind::Unsupported, "the option fetch thread stopped")));
        notice(&report)
    }
}

/// One line about a fetch: what was installed, the first refusal, or why nothing was fetched.
pub(super) fn notice(report: &Report) -> String {
    if let Some(error) = &report.error {
        return format!("Option lists weren't fetched: {error}");
    }
    let values: usize = report.installed.iter().map(|list| list.values).sum();
    let lists = report.installed.len();
    // Lists of a provider that isn't set up are told only when they are all there was (the
    // catalog changed during the fetch; a fetch that asked no provider is an error).
    if lists == 0
        && report.refused.is_empty()
        && let Some(first) = report.unavailable.first()
    {
        return format!("Option lists weren't fetched: {}", first.reason);
    }
    let mut line = format!("Fetched {lists} option list{} ({values} value{}).", plural(lists), plural(values));
    if let Some(first) = report.refused.first() {
        let others = report.refused.len().saturating_sub(1);
        line.push_str(&format!(" {} for {} wasn't installed: {}", first.source, first.owner, first.reason.trim_end_matches('.')));
        if others > 0 {
            line.push_str(&format!(" (and {others} more)"));
        }
        line.push('.');
    }
    line
}

impl super::NodeCraftApp {
    /// Hands on and installs option fetches; repaints while one runs so its answer is seen.
    pub(super) fn tick_option_fetch(&mut self, ctx: &egui::Context) {
        let settings = self.provider_settings();
        let Some(fetcher) = &mut self.option_fetch else {
            return;
        };
        if let Some(notice) = fetcher.tick(&mut self.session, || settings, ctx) {
            // Cards drawing a list resolve against the new lists (#676).
            self.schemas.forget_listed();
            self.notice = Some(notice);
        }
        if fetcher.is_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests;
