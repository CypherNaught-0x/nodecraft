//! One bounded template request per window; results never import into a project automatically.
//! On the desktop the worker thread also owns the template cache (#954): it reads and verifies
//! the cache at startup and applies the session's queued writes, so the UI thread never does
//! template disk I/O, hashing or decoding.
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::templates::disk::{self, FileCache, Loaded, Write};
use nodecraft_engine::{
    Session,
    templates::{
        fetch::{self, Fetched, Request, SERVICE, Source},
        update,
    },
};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::{cell::RefCell, rc::Rc, sync::Arc};

type Ask = (u64, Arc<()>, Request);
const REMOVED: &str = "Downloaded templates were removed. Bundled templates and templates in your projects stay.";
/// Runs the worker's cache read; a panic becomes a failed read, so the session never waits on it
/// forever.
#[cfg(not(target_arch = "wasm32"))]
fn load_guarded(load: impl FnOnce() -> Loaded + std::panic::UnwindSafe) -> Loaded {
    std::panic::catch_unwind(load).unwrap_or_else(|_| Loaded::failed("Reading downloaded templates stopped unexpectedly; the bundled set is in use"))
}
#[cfg(not(target_arch = "wasm32"))]
type Done = (u64, Arc<()>, Result<Fetched, Error>);
/// What the UI thread asks the worker for.
#[cfg(not(target_arch = "wasm32"))]
enum Job {
    Fetch(Ask),
    /// Read and verify the cache for the session `owner` identifies.
    Load(Arc<()>),
    Write(Vec<Write>),
}
/// What the worker hands back.
#[cfg(not(target_arch = "wasm32"))]
enum Out {
    Fetched(Done),
    Loaded(Arc<()>, Loaded),
    /// Problems applying writes, and how a queued clear went.
    Wrote {
        problems: Vec<String>,
        cleared: Option<Result<(), String>>,
    },
}
#[cfg(any(test, target_arch = "wasm32"))]
type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Fetched, Error>>>>;
#[cfg(any(test, target_arch = "wasm32"))]
type AsyncFetch = Box<dyn FnMut(Request) -> Future>;
#[derive(Default)]
struct Asked {
    next: u64,
    queued: Option<Ask>,
    busy: bool,
}
enum Worker {
    #[cfg(not(target_arch = "wasm32"))]
    Thread { jobs: Sender<Job>, done: Receiver<Out>, disk: Option<FileCache> },
    #[cfg(any(test, target_arch = "wasm32"))]
    Async { fetch: AsyncFetch, pending: Option<(u64, Arc<()>, Future)> },
}
/// What the request in flight is, for its notice and `templates.status`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flight {
    Single,
    Update,
    Clear,
}
pub(super) struct TemplateFetcher {
    asked: Rc<RefCell<Asked>>,
    worker: Worker,
    owner: Option<Arc<()>>,
    source: Source,
    flight: Option<Flight>,
}
impl TemplateFetcher {
    /// The desktop fetcher: the same transport as the update check (#123), redirects refused.
    /// With `disk`, the worker also owns the persistent template cache.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn new(source: Source, disk: Option<FileCache>) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::new(ErrorKind::Unsupported, "The template worker could not start"))?;
        let http: Arc<dyn Http> =
            Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|_| Error::new(ErrorKind::Unsupported, "The template HTTP client is unavailable"))?);
        let from = source.clone();
        Self::spawn(source, Box::new(move |request| runtime.block_on(fetch::fetch_from(http.as_ref(), &from, request))), disk)
    }
    /// The configured repository: the official one, or in debug builds a loopback fixture server
    /// named by `NODECRAFT_TEMPLATES_FEED` (`http://127.0.0.1:<port>/`). Release builds don't
    /// read the variable at all.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn configured_source() -> Source {
        #[cfg(debug_assertions)]
        if let Ok(feed) = std::env::var("NODECRAFT_TEMPLATES_FEED") {
            match Source::loopback(&feed) {
                Ok(source) => return source,
                Err(error) => log::warn!("NODECRAFT_TEMPLATES_FEED ignored: {}", error.message),
            }
        }
        Source::official()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(source: Source, mut fetch: Box<dyn FnMut(Request) -> Result<Fetched, Error> + Send>, disk: Option<FileCache>) -> Result<Self, Error> {
        let (jobs, inbox) = channel::<Job>();
        let (outbox, done) = channel::<Out>();
        let store = disk.clone();
        let keys = source.keys().to_vec();
        std::thread::Builder::new()
            .name("nodecraft-templates".into())
            .spawn(move || {
                let stopped = || Error::new(ErrorKind::Internal, "The template worker stopped unexpectedly");
                for job in inbox {
                    let out = match job {
                        Job::Fetch((id, owner, request)) => {
                            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(request))).unwrap_or_else(|_| Err(stopped()));
                            Out::Fetched((id, owner, result))
                        }
                        Job::Load(owner) => {
                            let Some(store) = &store else { continue };
                            Out::Loaded(owner, load_guarded(|| disk::load(store, &keys)))
                        }
                        Job::Write(writes) => {
                            let Some(store) = &store else { continue };
                            let mut problems = Vec::new();
                            let mut cleared = None;
                            for write in &writes {
                                let result = std::panic::catch_unwind(|| write.apply(store)).unwrap_or_else(|_| Err(stopped())).map_err(|error| error.message);
                                if matches!(write, Write::Clear) {
                                    cleared = Some(result);
                                } else if let Err(problem) = result {
                                    problems.push(problem);
                                }
                            }
                            if problems.is_empty() && cleared.is_none() {
                                continue;
                            }
                            Out::Wrote { problems, cleared }
                        }
                    };
                    if outbox.send(out).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| Error::new(ErrorKind::Unsupported, "The template worker could not start"))?;
        Ok(Self { asked: Rc::default(), worker: Worker::Thread { jobs, done, disk }, owner: None, source, flight: None })
    }
    #[cfg(target_arch = "wasm32")]
    pub(super) fn browser() -> Result<Self, Error> {
        let http: Arc<dyn Http> =
            Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|_| Error::new(ErrorKind::Unsupported, "The template HTTP client is unavailable"))?);
        Ok(Self::asynchronous(Box::new(move |request| {
            let http = Arc::clone(&http);
            Box::pin(async move { fetch::fetch(http.as_ref(), request).await })
        })))
    }
    #[cfg(any(test, target_arch = "wasm32"))]
    fn asynchronous(fetch: AsyncFetch) -> Self {
        Self { asked: Rc::default(), worker: Worker::Async { fetch, pending: None }, owner: None, source: Source::official(), flight: None }
    }
    pub(super) fn bind(&mut self, session: &mut Session) -> Result<(), Error> {
        let owner = session.view_identity();
        if self.owner.as_ref().is_some_and(|old| Arc::ptr_eq(old, &owner)) {
            return Ok(());
        }
        let queue = Rc::clone(&self.asked);
        let bound = Arc::clone(&owner);
        session.register_host_service(SERVICE, move |value: &Value| {
            let request: Request = serde_json::from_value(value.clone()).map_err(|_| Error::new(ErrorKind::InvalidInput, "Malformed template request"))?;
            let mut asked = queue.borrow_mut();
            if asked.busy {
                return Err(Error::new(ErrorKind::Busy, "A template request is already running"));
            }
            asked.next = asked.next.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "Too many template requests; reopen the app"))?;
            let id = asked.next;
            asked.queued = Some((id, Arc::clone(&bound), request));
            asked.busy = true;
            Ok(json!({"pending":id}))
        })?;
        update::configure(session, &self.source);
        // The worker reads and verifies this session's cache; templates.update waits for it.
        #[cfg(not(target_arch = "wasm32"))]
        if let Worker::Thread { jobs, disk: Some(store), .. } = &self.worker {
            use nodecraft_engine::templates::disk::TemplateCacheStore;
            disk::install_deferred(session, true, store.location());
            if jobs.send(Job::Load(Arc::clone(&owner))).is_err() {
                disk::adopt(session, disk::load(store, self.source.keys()));
            }
        }
        self.owner = Some(owner);
        Ok(())
    }
    /// Hands the session's queued cache writes to the worker.
    #[cfg(not(target_arch = "wasm32"))]
    fn flush_writes(&self, session: &mut Session) {
        if let Worker::Thread { jobs, disk: Some(_), .. } = &self.worker {
            let writes = disk::take_writes(session);
            if !writes.is_empty() && jobs.send(Job::Write(writes)).is_err() {
                disk::wrote(session, vec!["Downloaded templates couldn't be saved: the template worker stopped".into()]);
            }
        }
    }
    pub(super) fn waiting(&self) -> bool {
        self.asked.borrow().busy
    }
    pub(super) fn tick(&mut self, session: &mut Session, ctx: &egui::Context) -> Option<String> {
        if let Err(error) = self.bind(session) {
            return Some(error.message);
        }
        let notice = self.step(session, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.flush_writes(session);
        notice
    }
    fn step(&mut self, session: &mut Session, _ctx: &egui::Context) -> Option<String> {
        let queued = self.asked.borrow_mut().queued.take();
        if let Some((id, owner, request)) = queued {
            if !Arc::ptr_eq(&owner, &session.view_identity()) {
                self.asked.borrow_mut().busy = false;
                return Some("The template request was discarded after the project Session changed".into());
            }
            let flight = match &request {
                Request::Update { .. } => Flight::Update,
                Request::ClearCache => Flight::Clear,
                Request::Index | Request::Archive { .. } => Flight::Single,
            };
            if flight == Flight::Update {
                update::started(session);
            }
            self.flight = Some(flight);
            match &mut self.worker {
                #[cfg(not(target_arch = "wasm32"))]
                Worker::Thread { jobs, .. } => {
                    if jobs.send(Job::Fetch((id, owner, request))).is_err() {
                        self.asked.borrow_mut().busy = false;
                        return Some("The template worker is unavailable".into());
                    }
                }
                #[cfg(any(test, target_arch = "wasm32"))]
                Worker::Async { fetch, pending } => {
                    *pending = Some((id, owner, fetch(request)));
                    _ctx.request_repaint();
                    return None;
                }
            }
        }
        let done = match &mut self.worker {
            #[cfg(not(target_arch = "wasm32"))]
            Worker::Thread { done, .. } => match done.try_recv() {
                Ok(Out::Fetched(done)) => done,
                Ok(Out::Wrote { problems, cleared }) => {
                    disk::wrote(session, problems);
                    let cleared = cleared?;
                    disk::cleared(session, cleared.clone());
                    return Some(match cleared {
                        Ok(()) => REMOVED.into(),
                        Err(problem) => format!("Downloaded templates couldn't be removed: {problem}"),
                    });
                }
                Ok(Out::Loaded(owner, loaded)) => {
                    if !Arc::ptr_eq(&owner, &session.view_identity()) {
                        return None;
                    }
                    let problems = disk::adopt(session, loaded);
                    return problems.first().map(|first| {
                        log::warn!("template cache: {}", problems.join("; "));
                        format!("Some downloaded templates were skipped: {first}. Clear them from Techniques → Starter templates.")
                    });
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    // Never leave the session waiting on a worker that is gone.
                    if disk::is_loading(session) {
                        disk::adopt(session, Loaded::failed("The template worker stopped before reading downloaded templates"));
                    }
                    if disk::is_clearing(session) {
                        disk::cleared(session, Err("The template worker stopped before removing downloaded templates".into()));
                    }
                    if !self.asked.borrow().busy {
                        return None;
                    }
                    self.asked.borrow_mut().busy = false;
                    return Some("The template worker stopped".into());
                }
            },
            #[cfg(any(test, target_arch = "wasm32"))]
            Worker::Async { pending, .. } => {
                let (id, owner, mut future) = pending.take()?;
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::web_files::poll(&mut future, _ctx))) {
                    Ok(std::task::Poll::Pending) => {
                        *pending = Some((id, owner, future));
                        return None;
                    }
                    Ok(std::task::Poll::Ready(result)) => (id, owner, result),
                    Err(_) => (id, owner, Err(Error::new(ErrorKind::Internal, "The template request stopped unexpectedly"))),
                }
            }
        };
        self.asked.borrow_mut().busy = false;
        let flight = self.flight.take();
        let (_, owner, result) = done;
        if !Arc::ptr_eq(&owner, &session.view_identity()) {
            return Some("The template request was discarded after the project Session changed".into());
        }
        let settled = fetch::settle(session, result);
        Some(match (flight, settled) {
            (Some(Flight::Update), settled) => match update::last(session) {
                Some(outcome) => update::notice(&outcome),
                None => settled.err().map_or_else(|| "Templates are up to date.".to_owned(), |error| format!("Templates were not updated: {}", error.message)),
            },
            // The worker reports the removal itself once it has run.
            #[cfg(not(target_arch = "wasm32"))]
            (Some(Flight::Clear), Ok(())) if disk::is_clearing(session) => "Removing downloaded templates…".into(),
            (Some(Flight::Clear), Ok(())) => REMOVED.into(),
            (_, Ok(())) => {
                "Templates updated. Retrieved versions are verified and kept for later sessions; imported projects keep their assets when saved.".into()
            }
            (_, Err(error)) => format!("Templates were not updated: {}", error.message),
        })
    }
}
impl super::NodeCraftApp {
    pub(super) fn tick_template_fetch(&mut self, ctx: &egui::Context) {
        let Some(fetcher) = &mut self.template_fetch else {
            return;
        };
        if let Some(notice) = fetcher.tick(&mut self.session, ctx) {
            self.notice = Some(notice);
        }
        if fetcher.waiting() || nodecraft_engine::templates::disk::is_loading(&self.session) || nodecraft_engine::templates::disk::is_clearing(&self.session) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests;
