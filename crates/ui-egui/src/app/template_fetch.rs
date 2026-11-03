//! One bounded template request per window; results never import into a project automatically.
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
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::{cell::RefCell, rc::Rc, sync::Arc};

type Ask = (u64, Arc<()>, Request);
#[cfg(not(target_arch = "wasm32"))]
type Done = (u64, Arc<()>, Result<Fetched, Error>);
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
    Thread { asks: SyncSender<Ask>, done: Receiver<Done> },
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
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn new(source: Source) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::new(ErrorKind::Unsupported, "The template worker could not start"))?;
        let http: Arc<dyn Http> =
            Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|_| Error::new(ErrorKind::Unsupported, "The template HTTP client is unavailable"))?);
        let from = source.clone();
        Self::spawn(source, Box::new(move |request| runtime.block_on(fetch::fetch_from(http.as_ref(), &from, request))))
    }
    /// The configured repository: the official one, or in debug builds a loopback fixture server
    /// named by `NODECRAFT_TEMPLATES_FEED` (`http://127.0.0.1:<port>/`).
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn configured_source() -> Source {
        let feed = std::env::var("NODECRAFT_TEMPLATES_FEED").ok().filter(|_| cfg!(debug_assertions));
        match feed.map(|feed| Source::loopback(&feed)) {
            Some(Ok(source)) => source,
            Some(Err(error)) => {
                log::warn!("NODECRAFT_TEMPLATES_FEED ignored: {}", error.message);
                Source::official()
            }
            None => Source::official(),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(source: Source, mut fetch: Box<dyn FnMut(Request) -> Result<Fetched, Error> + Send>) -> Result<Self, Error> {
        let (asks, inbox) = sync_channel::<Ask>(1);
        let (outbox, done) = sync_channel::<Done>(1);
        std::thread::Builder::new()
            .name("nodecraft-templates".into())
            .spawn(move || {
                for (id, owner, request) in inbox {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(request)))
                        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "The template request stopped unexpectedly")));
                    if outbox.send((id, owner, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| Error::new(ErrorKind::Unsupported, "The template worker could not start"))?;
        Ok(Self { asked: Rc::default(), worker: Worker::Thread { asks, done }, owner: None, source, flight: None })
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
        self.owner = Some(owner);
        Ok(())
    }
    pub(super) fn waiting(&self) -> bool {
        self.asked.borrow().busy
    }
    pub(super) fn tick(&mut self, session: &mut Session, _ctx: &egui::Context) -> Option<String> {
        if let Err(error) = self.bind(session) {
            return Some(error.message);
        }
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
                Worker::Thread { asks, .. } => {
                    if asks.try_send((id, owner, request)).is_err() {
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
                Ok(done) => done,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
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
            (Some(Flight::Clear), Ok(())) => "Downloaded templates were removed. Bundled templates and templates in your projects stay.".into(),
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
        if fetcher.waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests;
