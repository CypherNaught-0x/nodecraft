//! Reading provider model lists in the desktop app: the engine's `models.catalog_fetch` service,
//! which `models.catalog_refresh` asks for.
//!
//! The service only queues the request and answers `{pending: id}`. Each frame
//! [`CatalogFetcher::tick`] hands a queued request, with the provider settings and whether an
//! OpenRouter key account is kept, to the fetch thread; the answer is installed with
//! `model_catalogs::finish`. One fetch runs at a time; asking again while one runs queues one more,
//! which reads the settings in force when it starts (an address just saved, say).
//!
//! The app refreshes once at start when OpenRouter or LiteLLM is set up, and again when either
//! is set up or changed. Those refreshes are quiet: only a failure is told.
use super::NodeCraftApp;
use nodecraft_engine::model_catalogs::{self, Fetched, Report, SERVICE};
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
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

/// Reads the lists for the given settings; the app's reads the network, tests pass a fixture.
pub(super) type Fetch = Box<dyn FnMut(&ProviderSettings, bool) -> Fetched + Send>;

/// What the fetch thread is asked: the request id, the settings, and whether OpenRouter is keyed.
type Ask = (u64, ProviderSettings, bool);

#[derive(Default)]
struct Asked {
    next: u64,
    queued: Option<u64>,
    running: Option<u64>,
    /// Ids whose outcome is told only when something failed.
    quiet: Vec<u64>,
}

pub(super) struct CatalogFetcher {
    asked: Rc<RefCell<Asked>>,
    asks: SyncSender<Ask>,
    done: Receiver<(u64, Fetched)>,
    /// The next request the service queues is quiet.
    quiet_next: Rc<RefCell<bool>>,
}

/// The `models.catalog_fetch` handler [`CatalogFetcher::spawn`] registers.
pub(super) type Service = Box<dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, Error>>;

impl CatalogFetcher {
    /// The fetch thread, over its own runtime and HTTP client, registered as `models.catalog_fetch`.
    pub(super) fn new(services: &mut Services) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the model list thread: {e}")))?;
        let http: Arc<dyn Http> = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let start = std::time::Instant::now();
        let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
        let (fetcher, mut service) = Self::spawn(Box::new(move |settings, openrouter| {
            let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings, now_ms: now_ms.clone(), progress: None };
            runtime.block_on(model_catalogs::fetch(&env, openrouter))
        }))?;
        services.register(SERVICE, move |request| service(request))?;
        Ok(fetcher)
    }

    pub(super) fn spawn(mut fetch: Fetch) -> Result<(Self, Service), Error> {
        let (asks, inbox) = sync_channel::<Ask>(1);
        let (outbox, done) = sync_channel::<(u64, Fetched)>(1);
        std::thread::Builder::new()
            .name("nodecraft-model-lists".into())
            .spawn(move || {
                for (id, settings, openrouter) in inbox {
                    // A keychain backend or reply that panics fails this read, not the thread.
                    let fetched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(&settings, openrouter))).unwrap_or_else(|_| {
                        let failed = || model_catalogs::Listing::Failed(Error::new(ErrorKind::Internal, "reading the model list failed unexpectedly"));
                        Fetched(model_catalogs::Source::ALL.iter().map(|source| (*source, failed())).collect())
                    });
                    if outbox.send((id, fetched)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the model list thread: {e}")))?;
        let asked = Rc::new(RefCell::new(Asked::default()));
        let quiet_next = Rc::new(RefCell::new(false));
        let (queue, quiet) = (asked.clone(), quiet_next.clone());
        let service = move |_: &serde_json::Value| {
            let mut asked = queue.borrow_mut();
            let quiet = std::mem::take(&mut *quiet.borrow_mut());
            // A refresh already waiting reads the settings in force when it starts: share it.
            if let Some(id) = asked.queued {
                if !quiet {
                    asked.quiet.retain(|q| *q != id);
                }
                return Ok(json!({"pending": id}));
            }
            if asked.next == u64::MAX {
                return Err(Error::new(ErrorKind::Busy, "too many model list refreshes; restart NodeCraft to read them again"));
            }
            asked.next += 1;
            let id = asked.next;
            asked.queued = Some(id);
            if quiet && asked.quiet.len() < 64 {
                asked.quiet.push(id);
            }
            Ok(json!({"pending": id}))
        };
        Ok((Self { asked, asks, done, quiet_next }, Box::new(service)))
    }

    /// Asks for a refresh whose outcome is told only when it fails.
    pub(super) fn refresh_quietly(&mut self, session: &mut Session) {
        *self.quiet_next.borrow_mut() = true;
        if let Err(error) = session.dispatch("models.catalog_refresh", &json!({})) {
            log::warn!("model lists can't be refreshed: {}", error.message);
        }
        *self.quiet_next.borrow_mut() = false;
    }

    pub(super) fn is_waiting(&self) -> bool {
        let asked = self.asked.borrow();
        asked.queued.is_some() || asked.running.is_some()
    }

    /// Installs a finished read and starts a queued one; returns what to tell the person.
    pub(super) fn tick(&mut self, session: &mut Session, settings: impl FnOnce() -> (ProviderSettings, bool)) -> Option<String> {
        let notice = match self.done.try_recv() {
            Ok((id, fetched)) => {
                let mut asked = self.asked.borrow_mut();
                asked.running = asked.running.filter(|running| *running != id);
                let quiet = asked.quiet.contains(&id);
                asked.quiet.retain(|q| *q != id);
                drop(asked);
                let report = model_catalogs::finish(session, id, fetched);
                told(&report, quiet)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let running = self.asked.borrow_mut().running.take();
                running.map(|_| "The model list thread stopped; restart NodeCraft to read provider models again.".to_owned())
            }
        };
        if self.asked.borrow().running.is_some() {
            return notice;
        }
        let queued = self.asked.borrow_mut().queued.take();
        if let Some(id) = queued {
            let (settings, openrouter) = settings();
            match self.asks.try_send((id, settings, openrouter)) {
                Ok(()) => self.asked.borrow_mut().running = Some(id),
                Err(TrySendError::Full(_)) => self.asked.borrow_mut().queued = Some(id),
                Err(TrySendError::Disconnected(_)) => return Some("The model list thread stopped; restart NodeCraft to read provider models again.".into()),
            }
        }
        notice
    }
}

/// The notice for a finished refresh: always when asked for, only on failure when quiet.
fn told(report: &Report, quiet: bool) -> Option<String> {
    let failed = report.error.is_some() || report.sources.iter().any(|source| source.error.is_some());
    (!quiet || failed).then(|| report.summary())
}

/// Whether a provider that lists its models is set up: an OpenRouter key account or a LiteLLM address.
pub(super) fn wanted(settings: &ProviderSettings, openrouter: bool) -> bool {
    openrouter || settings.litellm_url.as_deref().is_some_and(|url| !url.trim().is_empty())
}

impl NodeCraftApp {
    /// The provider settings and whether an OpenRouter key account is kept.
    fn catalog_settings(store: Option<&nodecraft_engine::settings::SettingsStore>) -> (ProviderSettings, bool) {
        store.map_or_else(Default::default, |store| {
            let saved = store.get();
            (saved.providers.clone(), saved.keys.accounts().iter().any(|account| account.as_str() == "openrouter"))
        })
    }

    pub(super) fn tick_model_catalogs(&mut self, ctx: &egui::Context) {
        let Some(fetcher) = &mut self.model_catalogs else {
            return;
        };
        if !self.model_catalogs_started {
            self.model_catalogs_started = true;
            let (settings, openrouter) = Self::catalog_settings(self.store.as_ref());
            if wanted(&settings, openrouter) {
                fetcher.refresh_quietly(&mut self.session);
            }
        }
        let store = &self.store;
        if let Some(notice) = fetcher.tick(&mut self.session, || Self::catalog_settings(store.as_ref())) {
            self.notice = Some(notice);
        }
        if fetcher.is_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Reads the model lists again, quietly, after OpenRouter or LiteLLM was set up or changed.
    pub(super) fn refresh_model_catalogs(&mut self) {
        if let Some(fetcher) = &mut self.model_catalogs {
            fetcher.refresh_quietly(&mut self.session);
        }
    }
}

#[cfg(test)]
mod tests;
