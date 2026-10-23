//! Live sharing's own thread (#714). It holds the connection to the relay and the [`Collab`], so
//! reading frames, merging what the others did into the shared document and working out what
//! the session must take to match it never hold up a frame. The window hands it a copy of the
//! document when that has changed ([`Turn`]), and takes the [`Plan`] that comes back only if
//! nothing was edited meanwhile; otherwise the next turn plans again from the newer copy.
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use nodecraft_engine::collab::{Collab, Out, Plan, Stretch, View};
use nodecraft_graph::Project;
use nodecraft_sync::{Frame, Role};
use nodecraft_types::{Blake3, Error, ErrorKind, ProjectId};

use super::connection::{Connection, Event};

/// What the window asks of the thread.
enum Ask {
    /// The connection has something.
    Wake,
    Turn(Turn),
    /// Frames for the next publish: the owner's share links.
    Queue(Vec<Frame>),
    /// Frames for connection `n` now: the assets the others asked for.
    Send(u64, Vec<Frame>),
    /// What came of a plan, as the session took it: `document` (at `revision`) is what it now
    /// holds if `applied`.
    Taken {
        revision: u64,
        document: Option<Project>,
        applied: bool,
    },
    /// The window let go: the thread ends, and the connection with it.
    Stop,
}

/// The session as the thread settles it.
pub(super) struct Turn {
    pub(super) revision: u64,
    /// `None`: the same document as the last turn's.
    pub(super) document: Option<Project>,
    pub(super) assets: BTreeSet<Blake3>,
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
}

/// What the thread tells the window.
#[derive(Default)]
pub(super) struct Report {
    /// The connection opened or dropped, in order.
    pub(super) events: Vec<Event>,
    /// The answer to a turn.
    pub(super) plan: Option<Plan>,
    /// Assets the others asked for, which only the session has the bytes of, for connection `n`.
    pub(super) assets: Option<(u64, Vec<(Blake3, Stretch)>)>,
    pub(super) status: Status,
    /// Something to tell the person.
    pub(super) notices: Vec<String>,
    /// Why the sharing ended.
    pub(super) ended: Option<String>,
    /// Something came in that the next turn settles.
    pub(super) dirty: bool,
}

/// The window's end. Dropping it ends the thread, and with it the connection.
pub(super) struct Worker {
    asks: Sender<Ask>,
    reports: Receiver<Report>,
}

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
        std::thread::Builder::new()
            .name("nodecraft-share-merge".into())
            .spawn(move || match make() {
                Ok(collab) => run(connection, collab, &inbox, &outbox, &*wake),
                Err(error) => {
                    let _ = outbox.send(Report { ended: Some(error.message), ..Report::default() });
                    wake();
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start live sharing: {e}")))?;
        Ok(Self { asks, reports })
    }

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

    /// Frames for connection `n` now, if it is still the open one.
    pub(super) fn send(&self, n: u64, frames: Vec<Frame>) -> Result<(), String> {
        if frames.is_empty() { Ok(()) } else { self.ask(Ask::Send(n, frames)) }
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

impl Drop for Worker {
    fn drop(&mut self) {
        // The thread may have ended already.
        let _ = self.asks.send(Ask::Stop);
    }
}

fn gone() -> String {
    "live sharing stopped unexpectedly".into()
}

/// The most asks the thread takes in before reporting, so a busy room still shows progress.
const BATCH: usize = 64;

/// The thread: waits for something to do, does all of it, and reports.
fn run(connection: Connection, mut collab: Collab, inbox: &Receiver<Ask>, outbox: &Sender<Report>, wake: &(dyn Fn() + Send + Sync)) {
    let mut thread = Thread { connection, n: None, document: None };
    while let Ok(first) = inbox.recv() {
        let mut report = Report::default();
        let mut quiet = true;
        for ask in std::iter::once(first).chain(inbox.try_iter().take(BATCH - 1)) {
            if matches!(ask, Ask::Stop) {
                return;
            }
            quiet &= matches!(ask, Ask::Wake);
            if let Err(error) = thread.handle(&mut collab, ask, &mut report) {
                report.ended = Some(error.message);
                break;
            }
        }
        let ended = report.ended.is_some();
        match status(&mut collab) {
            Ok(status) => report.status = status,
            Err(error) => report.ended = report.ended.or(Some(error.message)),
        }
        report.notices.extend(collab.take_refusal().map(|why| format!("The relay didn't take a change to a discussion: {why}")));
        report.notices.extend(collab.take_clash());
        // A wake that brought nothing needn't wake the window.
        let news = !quiet || report.dirty || !report.events.is_empty() || report.ended.is_some() || !report.notices.is_empty();
        if news {
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

struct Thread {
    connection: Connection,
    /// The open connection's number.
    n: Option<u64>,
    /// The session's document as of the last turn.
    document: Option<Project>,
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
                            let replies = Frame::decode(&bytes).and_then(|frame| collab.receive(frame))?;
                            self.send(replies)?;
                            report.dirty = true;
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
            Ask::Send(n, frames) => {
                for frame in frames {
                    self.connection.send(n, frame.encode()?);
                }
            }
            // Handled before it gets here.
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
                let has = |hash: &Blake3| turn.assets.contains(hash);
                let (out, plan) = collab.plan(&View { revision: turn.revision, document, has: &has })?;
                let mut assets = Vec::new();
                let mut frames = Vec::new();
                for out in out {
                    match out {
                        Out::Frame(frame) => frames.push(frame),
                        Out::Asset(hash, stretch) => assets.push((hash, stretch)),
                    }
                }
                self.send(frames)?;
                if let Some(n) = self.n.filter(|_| !assets.is_empty()) {
                    report.assets = Some((n, assets));
                }
                report.plan = Some(plan);
                // Asks not answered this turn are, the next.
                report.dirty |= collab.owes_assets();
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
    })
}
