//! Fetching option lists in the desktop app (#529): the engine's `options.fetch` service.
//!
//! `options.refresh` (palette, MCP, control channel) asks the service, which only queues the
//! request and answers `{pending: id}`. Each frame [`OptionFetcher::tick`] hands a queued request,
//! with the current provider settings, to the fetch thread, which reads the server's token from
//! the keychain and calls `/object_info`, so neither an unlock prompt nor a slow server holds up
//! a frame. The answer is installed with `option_fetch::finish` and told as a notice. One fetch
//! runs at a time; asking again meanwhile is refused as busy.
use nodecraft_engine::option_fetch::{self, FetchRequest, Fetched, OPTION_SERVICE, Report};
use nodecraft_engine::provider_setup::HostEnv;
use nodecraft_engine::secrets::KeyringStore;
use nodecraft_engine::settings::ProviderSettings;
use nodecraft_engine::{Session, commands::Services};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

/// Fetches the named sources with the given settings; the app's reads the keychain and the
/// network, tests pass a fixture.
pub(super) type Fetch = Box<dyn FnMut(&[String], &ProviderSettings) -> Result<Fetched, Error> + Send>;

type Ask = (u64, Vec<String>, ProviderSettings);
type Done = (u64, Result<Fetched, Error>);

/// The `options.fetch` handler [`OptionFetcher::spawn`] registers.
pub(super) type Service = Box<dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, Error>>;

/// What the service asked for and the frame hasn't handed on.
#[derive(Default)]
struct Asked {
    next: u64,
    queued: Option<(u64, Vec<String>)>,
    /// The fetch handed to the thread and not yet answered.
    running: Option<u64>,
    /// A fetch is queued or running.
    busy: bool,
}

pub(super) struct OptionFetcher {
    asked: Rc<RefCell<Asked>>,
    asks: SyncSender<Ask>,
    done: Receiver<Done>,
}

impl OptionFetcher {
    /// The fetch thread, over its own runtime and HTTP client, registered as `options.fetch`.
    pub(super) fn new(services: &mut Services) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the option fetch thread: {e}")))?;
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let start = std::time::Instant::now();
        let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
        let (fetcher, mut service) = Self::spawn(Box::new(move |sources, settings| {
            let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings, now_ms: now_ms.clone(), progress: None };
            runtime.block_on(option_fetch::fetch(&env, sources))
        }))?;
        services.register(OPTION_SERVICE, move |request| service(request))?;
        Ok(fetcher)
    }

    /// The fetch thread over `fetch`, and the `options.fetch` handler that queues for it.
    pub(super) fn spawn(mut fetch: Fetch) -> Result<(Self, Service), Error> {
        let (asks, inbox) = sync_channel::<Ask>(1);
        let (outbox, done) = sync_channel::<Done>(1);
        std::thread::Builder::new()
            .name("nodecraft-options".into())
            .spawn(move || {
                for (id, sources, settings) in inbox {
                    // A keychain backend or reply that panics fails this fetch, not the thread.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(&sources, &settings)))
                        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "fetching option lists failed unexpectedly")));
                    if outbox.send((id, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the option fetch thread: {e}")))?;
        let asked = Rc::new(RefCell::new(Asked::default()));
        let queue = asked.clone();
        let service = move |request: &serde_json::Value| {
            let request: FetchRequest =
                serde_json::from_value(request.clone()).map_err(|_| Error::new(ErrorKind::InvalidInput, "malformed option fetch request"))?;
            let mut asked = queue.borrow_mut();
            if asked.busy {
                return Err(Error::new(ErrorKind::Busy, "option lists are already being fetched; try again when that finishes"));
            }
            asked.next = asked.next.saturating_add(1);
            let id = asked.next;
            asked.queued = Some((id, request.sources));
            asked.busy = true;
            Ok(json!({"pending": id}))
        };
        Ok((Self { asked, asks, done }, Box::new(service)))
    }

    /// Whether a fetch is queued or running.
    pub(super) fn is_waiting(&self) -> bool {
        self.asked.borrow().busy
    }

    /// Hands a queued request to the fetch thread and installs a finished fetch, returning what to
    /// tell the person about it.
    pub(super) fn tick(&mut self, session: &mut Session, settings: impl FnOnce() -> ProviderSettings) -> Option<String> {
        let queued = self.asked.borrow_mut().queued.take();
        if let Some((id, sources)) = queued {
            if self.asks.try_send((id, sources, settings())).is_err() {
                // The thread is gone: say so, and let the next refresh try again.
                return Some(self.stopped(session, id));
            }
            self.asked.borrow_mut().running = Some(id);
        }
        let (id, result) = match self.done.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                let running = self.asked.borrow_mut().running.take();
                return running.map(|id| self.stopped(session, id));
            }
        };
        let mut asked = self.asked.borrow_mut();
        asked.busy = false;
        asked.running = None;
        drop(asked);
        let report = option_fetch::finish(session, id, result);
        log::info!("option lists: {} installed, {} refused", report.installed.len(), report.refused.len());
        Some(notice(&report))
    }
}

impl OptionFetcher {
    /// Records fetch `id` as failed because the thread stopped, so `options.status` says so too.
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
        let Some(fetcher) = &mut self.option_fetch else {
            return;
        };
        let store = &self.store;
        if let Some(notice) = fetcher.tick(&mut self.session, || store.as_ref().map(|store| store.get().providers.clone()).unwrap_or_default()) {
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
