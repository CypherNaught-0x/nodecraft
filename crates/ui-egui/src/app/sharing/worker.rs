//! Live sharing's own thread (#714). It holds the connection to the relay and the [`Collab`], so
//! reading frames, merging what the others did into the shared document and working out what
//! the session must take to match it never hold up a frame. The window hands it a copy of the
//! document when that has changed ([`Turn`]), and takes the [`Plan`] that comes back only if
//! nothing was edited meanwhile; otherwise the next turn plans again from the newer copy. The
//! browser has no threads (#699): there the same work is done in the frame, a batch at a time.
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use nodecraft_engine::collab::{Collab, Plan, View};
use nodecraft_format::AssetStore;
use nodecraft_format::asset_source::TransferCancel;
use nodecraft_graph::Project;
use nodecraft_sync::{Frame, Person, Presence, Role};
use nodecraft_types::{Blake3, Error, ErrorKind, ProjectId};

use super::Event;
use super::connection::Connection;

/// What the window asks of the thread.
enum Ask {
    /// The connection has something.
    Wake,
    Turn(Turn),
    /// Frames for the next publish: the owner's share links.
    Queue(Vec<Frame>),
    /// A while has passed: assets put off are tried again.
    #[cfg(not(target_arch = "wasm32"))]
    Retry,
    /// What came of a plan, as the session took it: `document` (at `revision`) is what it now
    /// holds if `applied`.
    Taken {
        revision: u64,
        document: Option<Project>,
        applied: bool,
    },
    /// Where this person is now (#120).
    Presence(Presence),
    /// The window let go: the thread ends, and the connection with it.
    #[cfg(not(target_arch = "wasm32"))]
    Stop,
}

/// The session as the thread settles it.
pub(super) struct Turn {
    pub(super) revision: u64,
    /// `None`: the same document as the last turn's.
    pub(super) document: Option<Project>,
    /// A clone of the session's store, sharing its storage.
    pub(super) assets: AssetStore,
}

/// How the shared session is doing, as of the thread's last report.
#[derive(Clone, Debug, Default)]
pub(super) struct Status {
    pub(super) role: Option<Role>,
    pub(super) waiting_for_assets: usize,
    /// The bytes received of the assets coming in parts, and of how many.
    pub(super) asset_progress: (u64, u64),
    pub(super) unusable_assets: usize,
    pub(super) caught_up: bool,
    pub(super) project_id: Option<ProjectId>,
    /// The others who have said where they are.
    pub(super) people: Vec<Person>,
}

/// What the thread tells the window.
#[derive(Default)]
pub(super) struct Report {
    /// The connection opened or dropped, in order.
    pub(super) events: Vec<Event>,
    /// The answer to a turn.
    pub(super) plan: Option<Plan>,
    pub(super) status: Status,
    /// Something to tell the person.
    pub(super) notices: Vec<String>,
    /// Why the sharing ended.
    pub(super) ended: Option<String>,
    /// Something came in that the next turn settles.
    pub(super) dirty: bool,
    /// Someone moved, came or went: only what is drawn changes.
    pub(super) moved: bool,
}

/// The window's end. Dropping it ends the thread, and with it the connection.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct Worker {
    asks: Sender<Ask>,
    reports: Receiver<Report>,
    /// Stops a seal the thread is in the middle of when the window lets go (#855).
    cancel: TransferCancel,
}

#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    /// Connects to `relay` and takes part in its room through the [`Collab`] `make` makes on the
    /// thread (sharing a large project takes a while too); `wake` asks for a frame when there is
    /// something to report.
    pub(super) fn spawn(
        relay: String,
        make: impl FnOnce() -> Result<Collab, Error> + Send + 'static,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let (asks, inbox) = channel();
        let (outbox, reports) = channel();
        let nudge = asks.clone();
        let connection = Connection::open(relay, move || {
            // Gone only once the window has let go.
            let _ = nudge.send(Ask::Wake);
        })?;
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let cancel = TransferCancel::default();
        let stop = cancel.clone();
        std::thread::Builder::new()
            .name("nodecraft-share-merge".into())
            .spawn(move || match make() {
                Ok(collab) => run(Thread { connection, n: None, document: None, cancel: stop }, collab, &inbox, &outbox, &*wake),
                Err(error) => {
                    let _ = outbox.send(Report { ended: Some(error.message), ..Report::default() });
                    wake();
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start live sharing: {e}")))?;
        Ok(Self { asks, reports, cancel })
    }

    /// What the thread reported since the last call; an error if it has ended unasked.
    pub(super) fn reports(&self) -> Result<Vec<Report>, String> {
        let mut reports = Vec::new();
        loop {
            match self.reports.try_recv() {
                Ok(report) => reports.push(report),
                Err(TryRecvError::Empty) => return Ok(reports),
                // The thread ends by itself only after reporting why.
                Err(TryRecvError::Disconnected) if reports.is_empty() => return Err(gone()),
                Err(TryRecvError::Disconnected) => return Ok(reports),
            }
        }
    }

    fn ask(&self, ask: Ask) -> Result<(), String> {
        self.asks.send(ask).map_err(|_| gone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Worker {
    fn drop(&mut self) {
        // A seal underway stops at once, so the thread sees the Stop.
        self.cancel.cancel();
        // The thread may have ended already.
        let _ = self.asks.send(Ask::Stop);
    }
}

/// The window's end in the browser, which has no threads: the work is done in [`Self::reports`],
/// a batch each frame, asking for another frame while some is left. Dropping it closes the
/// connection.
#[cfg(target_arch = "wasm32")]
pub(super) struct Worker {
    inline: std::cell::RefCell<Inline>,
}

#[cfg(target_arch = "wasm32")]
struct Inline {
    /// Makes the [`Collab`], in the first frame, with the connection it then works through.
    start: Option<(Box<dyn FnOnce() -> Result<Collab, Error>>, Connection)>,
    running: Option<(Thread, Collab)>,
    asks: std::collections::VecDeque<Ask>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Said why it ended; a later call is an error, as once the thread has ended.
    ended: bool,
}

#[cfg(target_arch = "wasm32")]
impl Worker {
    /// Connects to `relay` and takes part in its room through the [`Collab`] `make` makes in the
    /// next frame; `wake` asks for a frame when there is something to do.
    pub(super) fn spawn(relay: String, make: impl FnOnce() -> Result<Collab, Error> + 'static, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, Error> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let nudge = Arc::clone(&wake);
        let connection = Connection::open(relay, move || nudge())?;
        wake();
        let inline = Inline { start: Some((Box::new(make), connection)), running: None, asks: Default::default(), wake, ended: false };
        Ok(Self { inline: std::cell::RefCell::new(inline) })
    }

    /// Does the next batch of what was asked, with what came in, and reports what came of it.
    pub(super) fn reports(&self) -> Result<Vec<Report>, String> {
        let mut inline = self.inline.try_borrow_mut().map_err(|_| gone())?;
        if inline.ended {
            return Err(gone());
        }
        if let Some((make, connection)) = inline.start.take() {
            match make() {
                Ok(collab) => inline.running = Some((Thread { connection, n: None, document: None, cancel: TransferCancel::default() }, collab)),
                Err(error) => {
                    inline.ended = true;
                    return Ok(vec![Report { ended: Some(error.message), ..Report::default() }]);
                }
            }
        }
        let take = inline.asks.len().min(BATCH - 1);
        let asks: Vec<Ask> = std::iter::once(Ask::Wake).chain(inline.asks.drain(..take)).collect();
        if !inline.asks.is_empty() {
            (inline.wake)();
        }
        let Some((thread, collab)) = inline.running.as_mut() else { return Err(gone()) };
        // No timer here: an asset put off is asked for again at the next turn.
        if asks.iter().any(|ask| matches!(ask, Ask::Turn(_))) {
            collab.retry_put_off();
        }
        let (report, ended) = work(thread, collab, asks);
        inline.ended = ended;
        Ok(report.into_iter().collect())
    }

    fn ask(&self, ask: Ask) -> Result<(), String> {
        let mut inline = self.inline.try_borrow_mut().map_err(|_| gone())?;
        if inline.ended {
            return Err(gone());
        }
        inline.asks.push_back(ask);
        (inline.wake)();
        Ok(())
    }
}

impl Worker {
    /// Hands the thread `turn`. Its answer comes as a report's plan.
    pub(super) fn turn(&self, turn: Turn) -> Result<(), String> {
        self.ask(Ask::Turn(turn))
    }

    /// Says what came of a plan at once, so the thread compares later publishes with what the
    /// session held right after taking it (`document`, at `revision`, when `applied`).
    pub(super) fn taken(&self, revision: u64, document: Option<Project>, applied: bool) -> Result<(), String> {
        self.ask(Ask::Taken { revision, document, applied })
    }

    /// Frames for the next publish.
    pub(super) fn queue(&self, frames: Vec<Frame>) -> Result<(), String> {
        if frames.is_empty() { Ok(()) } else { self.ask(Ask::Queue(frames)) }
    }

    /// Says where this person is; the thread sends it only if it changed.
    pub(super) fn presence(&self, presence: Presence) -> Result<(), String> {
        self.ask(Ask::Presence(presence))
    }
}

fn gone() -> String {
    "live sharing stopped unexpectedly".into()
}

/// The most asks the thread takes in before reporting, so a busy room still shows progress.
const BATCH: usize = 64;

/// How long an asset the storage was too busy or full for, or the spool had no room for, waits
/// before it is tried again (#842).
#[cfg(not(target_arch = "wasm32"))]
const RETRY: Duration = Duration::from_secs(2);

/// The thread: waits for something to do, does all of it, and reports.
#[cfg(not(target_arch = "wasm32"))]
fn run(mut thread: Thread, mut collab: Collab, inbox: &Receiver<Ask>, outbox: &Sender<Report>, wake: &(dyn Fn() + Send + Sync)) {
    // When what waits is tried again; a busy room doesn't put it off.
    let mut retry_at: Option<Instant> = None;
    loop {
        if !(collab.puts_off() || collab.seals_pending()) {
            retry_at = None;
        } else if retry_at.is_none() {
            retry_at = Instant::now().checked_add(RETRY);
        }
        let first = match retry_at {
            Some(at) => match inbox.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(ask) => ask,
                Err(RecvTimeoutError::Timeout) => {
                    retry_at = None;
                    Ask::Retry
                }
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match inbox.recv() {
                Ok(ask) => ask,
                Err(_) => return,
            },
        };
        let asks: Vec<Ask> = std::iter::once(first).chain(inbox.try_iter().take(BATCH - 1)).collect();
        if asks.iter().any(|ask| matches!(ask, Ask::Stop)) {
            return;
        }
        let (report, ended) = work(&mut thread, &mut collab, asks);
        if let Some(report) = report {
            if outbox.send(report).is_err() {
                return;
            }
            wake();
        }
        if ended {
            return;
        }
    }
}

/// Does `asks` and says what came of them, if anything worth waking the window for, and whether
/// the sharing ended.
fn work(thread: &mut Thread, collab: &mut Collab, asks: Vec<Ask>) -> (Option<Report>, bool) {
    let mut report = Report::default();
    let mut quiet = true;
    for ask in asks {
        quiet &= matches!(ask, Ask::Wake | Ask::Presence(_));
        if let Err(error) = thread.handle(collab, ask, &mut report) {
            report.ended = Some(error.message);
            break;
        }
    }
    let ended = report.ended.is_some();
    match status(collab) {
        Ok(status) => report.status = status,
        Err(error) => report.ended = report.ended.or(Some(error.message)),
    }
    report.notices.extend(collab.take_refusal().map(|why| format!("The relay didn't take a change to a discussion: {why}")));
    report.notices.extend(collab.take_clash());
    // A wake that brought nothing needn't wake the window.
    let news = !quiet || report.dirty || report.moved || !report.events.is_empty() || report.ended.is_some() || !report.notices.is_empty();
    (news.then_some(report), ended)
}

struct Thread {
    connection: Connection,
    /// The open connection's number.
    n: Option<u64>,
    /// The session's document as of the last turn.
    document: Option<Project>,
    /// Tripped when the window lets go, stopping a seal underway.
    cancel: TransferCancel,
}

impl Thread {
    fn handle(&mut self, collab: &mut Collab, ask: Ask, report: &mut Report) -> Result<(), Error> {
        match ask {
            Ask::Wake => {
                for event in self.connection.events() {
                    match event {
                        Event::Connected(n) => {
                            self.n = Some(n);
                            self.send(vec![collab.hello()])?;
                            report.dirty = true;
                            report.events.push(Event::Connected(n));
                        }
                        Event::Frame(bytes) => {
                            if self.n.is_none() {
                                continue;
                            }
                            let frame = Frame::decode(&bytes)?;
                            // Where the others are changes nothing to settle.
                            let moved = matches!(frame, Frame::Person(_) | Frame::Gone(_));
                            let replies = collab.receive(frame)?;
                            self.send(replies)?;
                            if moved {
                                report.moved = true;
                            } else {
                                report.dirty = true;
                            }
                        }
                        Event::Dropped { why, retry } => {
                            self.n = None;
                            report.events.push(Event::Dropped { why, retry });
                        }
                    }
                }
            }
            Ask::Queue(frames) => {
                collab.queue(frames);
                report.dirty = true;
            }
            #[cfg(not(target_arch = "wasm32"))]
            Ask::Retry => {
                // The next turn seals what waits, and asks again for what was put off and fits.
                let asks = collab.retry_put_off();
                report.dirty |= asks || collab.seals_pending();
            }
            Ask::Presence(presence) => {
                // The window checks it first; one that still doesn't pass just isn't said.
                if let Ok(Some(frame)) = collab.set_presence(presence) {
                    self.send(vec![frame])?;
                }
            }
            // Handled before it gets here.
            #[cfg(not(target_arch = "wasm32"))]
            Ask::Stop => {}
            Ask::Taken { revision, document, applied } => {
                if let Some(document) = document {
                    self.document = Some(document);
                }
                let document = self.document.as_ref().ok_or_else(|| Error::new(ErrorKind::Internal, "live sharing was asked to settle no document"))?;
                let frames = collab.taken(document, revision, applied)?;
                self.send(frames)?;
            }
            Ask::Turn(turn) => {
                if let Some(document) = turn.document {
                    self.document = Some(document);
                }
                let document = self.document.as_ref().ok_or_else(|| Error::new(ErrorKind::Internal, "live sharing was asked to settle no document"))?;
                // Slow for a large asset, so done here rather than in the window (#842), one a
                // turn (#855).
                let progressed = collab.seal_arrivals(&turn.assets, &self.cancel);
                let has = |hash: &Blake3| turn.assets.contains(hash);
                let (out, plan) = collab.plan(&View { revision: turn.revision, document, has: &has })?;
                let frames = collab.frames(out, &turn.assets);
                self.send(frames)?;
                report.plan = Some(plan);
                // Asks not answered this turn are, the next, and so is the next asset to seal.
                // Assets and asks the storage was too busy for wait a while instead, so the
                // window isn't asked for turn after turn.
                report.dirty |= collab.owes_assets() || (progressed && collab.seals_pending());
            }
        }
        Ok(())
    }

    /// Sends `frames` on the open connection; with none open they are dropped, and the next
    /// hello catches both sides up.
    fn send(&self, frames: Vec<Frame>) -> Result<(), Error> {
        let Some(n) = self.n else { return Ok(()) };
        for frame in frames {
            self.connection.send(n, frame.encode()?);
        }
        Ok(())
    }
}

fn status(collab: &mut Collab) -> Result<Status, Error> {
    Ok(Status {
        role: collab.role(),
        waiting_for_assets: collab.waiting_for_assets(),
        asset_progress: collab.asset_progress(),
        unusable_assets: collab.unusable_assets(),
        caught_up: collab.caught_up(),
        project_id: collab.project_id()?,
        people: collab.people().cloned().collect(),
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn letting_go_of_the_worker_stops_a_seal_under_way() {
        let worker = Worker::spawn("ws://127.0.0.1:9".into(), || Err(Error::new(ErrorKind::Unsupported, "no project")), || {}).unwrap();
        // The thread seals with a clone of this (#855).
        let seals = worker.cancel.clone();
        assert!(seals.check().is_ok());
        drop(worker);
        assert!(seals.check().is_err_and(|error| TransferCancel::is_cancellation(&error)));
    }
}
