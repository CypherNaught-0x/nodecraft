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
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_format::SealedAsset;
#[cfg(target_arch = "wasm32")]
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
    /// What came of the seal of a large asset that came, run on a thread of its own (#880).
    #[cfg(not(target_arch = "wasm32"))]
    Sealed {
        hash: Blake3,
        result: Result<SealedAsset, Error>,
    },
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
        let sealed = asks.clone();
        std::thread::Builder::new()
            .name("nodecraft-share-merge".into())
            .spawn(move || match make() {
                Ok(collab) => run(Thread { connection, n: None, document: None, sealed }, collab, &inbox, &outbox, &*wake),
                Err(error) => {
                    let _ = outbox.send(Report { ended: Some(error.message), ..Report::default() });
                    wake();
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start live sharing: {e}")))?;
        Ok(Self { asks, reports })
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
        // The thread lets go of its Collab, which stops a seal under way (#855). The thread may
        // have ended already.
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
                Ok(collab) => inline.running = Some((Thread { connection, n: None, document: None }, collab)),
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
    /// Where a seal says what came of it.
    #[cfg(not(target_arch = "wasm32"))]
    sealed: Sender<Ask>,
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
            // The next turn takes it in, and starts the next seal.
            #[cfg(not(target_arch = "wasm32"))]
            Ask::Sealed { hash, result } => report.dirty |= collab.sealed(hash, result),
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
                let frames = collab.frames(out, &turn.assets);
                self.send(frames)?;
                report.plan = Some(plan);
                // After the plan, which lets go of what no card uses any more (#880).
                let progressed = self.seal(collab, &turn.assets);
                // Asks not answered this turn are, the next, and so is what a seal brought.
                // Assets and asks the storage was too busy for wait a while instead, so the
                // window isn't asked for turn after turn.
                report.dirty |= collab.owes_assets() || progressed;
            }
        }
        Ok(())
    }

    /// Starts the next seal on a thread of its own: slow for a large asset, so done away from
    /// the window (#842) and from this thread, which meanwhile takes in what the others do and
    /// stops the seal if that leaves no card using its asset (#880). Whether it took in what
    /// came of one at once.
    #[cfg(not(target_arch = "wasm32"))]
    fn seal(&self, collab: &mut Collab, assets: &AssetStore) -> bool {
        let Some(seal) = collab.next_seal() else { return false };
        let hash = seal.hash();
        let assets = assets.clone();
        let to = self.sealed.clone();
        let spawned = std::thread::Builder::new().name("nodecraft-share-seal".into()).spawn(move || {
            // Made on the thread: one that never started has nothing to say.
            let done = SealDone { hash, to: Some(to) };
            done.send(seal.run(&assets));
        });
        match spawned {
            Ok(_) => false,
            // Tried again after a while, as when another transfer holds the storage.
            Err(_) => collab.sealed(hash, Err(Error::new(ErrorKind::Busy, "can't start sealing an asset that came"))),
        }
    }

    /// The browser keeps no assets coming in parts, so it has none to seal; one that did would
    /// seal here, in the frame.
    #[cfg(target_arch = "wasm32")]
    fn seal(&self, collab: &mut Collab, assets: &AssetStore) -> bool {
        collab.seal_arrivals(assets, &TransferCancel::default())
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

/// Says what came of a seal of `hash`, run on a thread of its own: once it is sent, or, if the
/// thread ends without sending it, as having failed, so another seal can start (#880).
#[cfg(not(target_arch = "wasm32"))]
struct SealDone {
    hash: Blake3,
    to: Option<Sender<Ask>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl SealDone {
    fn send(mut self, result: Result<SealedAsset, Error>) {
        if let Some(to) = self.to.take() {
            // Gone only once the sharing has ended.
            let _ = to.send(Ask::Sealed { hash: self.hash, result });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for SealDone {
    fn drop(&mut self) {
        if let Some(to) = self.to.take() {
            let _ = to.send(Ask::Sealed { hash: self.hash, result: Err(Error::new(ErrorKind::Internal, "sealing an asset that came stopped unexpectedly")) });
        }
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

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use nodecraft_engine::collab::{MemorySpool, Spool, Taken, joining};
    use nodecraft_engine::session::Session;
    use nodecraft_format::asset_source::{AssetRange, AssetSource, MemorySource};
    use nodecraft_graph::{Canvas, Change, Changeset, Node};
    use nodecraft_sync::RoomId;
    use nodecraft_types::{AssetRef, CanvasId, MediaMeta, NodeId, Value};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn canvas() -> CanvasId {
        CanvasId::from_bytes([2; 16])
    }

    fn room() -> RoomId {
        RoomId::from_bytes([5; 32])
    }

    /// A session of `project` keeping its assets on disk under `dir`, as the desktop's do.
    fn on_disk(project: Project, dir: &std::path::Path) -> Session {
        std::fs::create_dir_all(dir).unwrap();
        let snapshot = nodecraft_format::Snapshot { project, assets: Default::default(), epoch: 0 };
        Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(dir.join("recovery")).unwrap())).unwrap()
    }

    /// A sharer whose one card shows an asset too large for one frame.
    fn sharer(dir: &std::path::Path) -> (Session, Blake3) {
        let mut project = Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "Shared").unwrap();
        Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "Main") }]).apply(&mut project).unwrap();
        let mut session = on_disk(project, &dir.join("sharer"));
        let mut bytes = vec![0; nodecraft_sync::WINDOW_BYTES as usize + 7];
        blake3::Hasher::new().update(b"noise").finalize_xof().fill(&mut bytes);
        let len = bytes.len() as u64;
        let range = AssetRange::new(Arc::new(MemorySource::new(bytes.into())), 0, len).unwrap();
        let hash = session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap();
        let mut card = Node::new(NodeId::from_bytes([1; 16]), "text");
        card.params.insert("image".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
        session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: card }])).unwrap();
        (session, hash)
    }

    /// One side of a shared session, driven as the window drives its worker.
    struct Side {
        worker: Option<Worker>,
        session: Session,
        sent: Option<u64>,
        in_flight: bool,
        connected: bool,
        dirty: bool,
        /// A joiner, until it knows which project the room holds.
        joining: bool,
        status: Status,
    }

    impl Side {
        fn new(relay: &str, collab: Collab, session: Session, joining: bool) -> Self {
            let worker = Worker::spawn(relay.to_owned(), move || Ok(collab), || {}).unwrap();
            Self { worker: Some(worker), session, sent: None, in_flight: false, connected: false, dirty: false, joining, status: Status::default() }
        }

        fn tick(&mut self) {
            let Some(worker) = &self.worker else { return };
            for report in worker.reports().unwrap() {
                self.connected |= report.events.iter().any(|event| matches!(event, Event::Connected(_)));
                assert!(report.ended.is_none(), "{:?}", report.ended);
                self.dirty |= report.dirty;
                self.status = report.status;
                let Some(plan) = report.plan else { continue };
                self.in_flight = false;
                match plan.take(&mut self.session).unwrap() {
                    Taken::Nothing => {}
                    Taken::Stale => {
                        worker.taken(self.session.revision(), None, false).unwrap();
                        self.dirty = true;
                    }
                    Taken::Applied(applied) => {
                        applied.unwrap();
                        let revision = self.session.revision();
                        worker.taken(revision, Some(self.session.document().clone()), true).unwrap();
                        self.sent = Some(revision);
                    }
                }
            }
            self.joining &= self.status.project_id.is_none();
            let revision = self.session.revision();
            if self.connected && !self.joining && !self.in_flight && (self.dirty || self.sent != Some(revision)) {
                let document = (self.sent != Some(revision)).then(|| self.session.document().clone());
                worker.turn(Turn { revision, document, assets: self.session.assets().clone() }).unwrap();
                self.sent = Some(revision);
                self.in_flight = true;
                self.dirty = false;
            }
        }
    }

    /// Ticks both sides until `done` holds, for up to a generous while.
    fn until(sides: &mut [&mut Side], what: &str, done: impl Fn(&[&mut Side]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(sides) {
            assert!(Instant::now() < deadline, "never came: {what}");
            for side in sides.iter_mut() {
                side.tick();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Two people in one room: the sharer, and a joiner whose collab is `joiner`.
    fn pair(dir: &std::path::Path, joiner: Collab) -> (Side, Side, Blake3) {
        let (session, hash) = sharer(dir);
        let relay = super::super::tests::relay();
        let collab = Collab::share(&session, room()).unwrap();
        let owner = Side::new(&relay, collab, session, false);
        let joined = on_disk(joining(owner.session.document().id()).unwrap(), &dir.join("joiner"));
        (owner, Side::new(&relay, joiner, joined, true), hash)
    }

    #[test]
    fn a_large_asset_is_sealed_on_its_own_thread_and_the_next_turn_takes_it() {
        let dir = tempfile::tempdir().unwrap();
        let joiner = Collab::join(room()).with_spool(Box::new(MemorySpool::new(64 << 20))).sealing();
        let (mut owner, mut joiner, hash) = pair(dir.path(), joiner);
        until(&mut [&mut owner, &mut joiner], "the card and its asset", |sides| {
            sides[1].session.assets().contains(&hash) && sides[1].session.document() == sides[0].session.document()
        });
        assert_eq!(joiner.status.unusable_assets, 0);
    }

    /// A spool whose assets, once whole, trickle out a byte at a time, so a seal of one lasts
    /// until it is stopped; `started` is set at the seal's first read, `gone` once let go.
    struct Trickle {
        kept: BTreeMap<Blake3, Vec<u8>>,
        started: Arc<AtomicBool>,
        gone: Arc<AtomicBool>,
    }

    struct Slow {
        bytes: Vec<u8>,
        started: Arc<AtomicBool>,
        gone: Arc<AtomicBool>,
    }

    impl Drop for Slow {
        fn drop(&mut self) {
            self.gone.store(true, Ordering::Release);
        }
    }

    impl AssetSource for Slow {
        fn byte_len(&self) -> u64 {
            self.bytes.len() as u64
        }

        fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
            self.started.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(2));
            let rest = self.bytes.get(usize::try_from(offset).unwrap()..).unwrap_or_default();
            let n = rest.len().min(buffer.len()).min(1);
            buffer[..n].copy_from_slice(&rest[..n]);
            Ok(n)
        }
    }

    impl Spool for Trickle {
        fn limit(&self) -> u64 {
            64 << 20
        }

        fn budget(&self) -> u64 {
            64 << 20
        }

        fn begin(&mut self, hash: Blake3, _total: u64) -> Result<(), Error> {
            self.kept.insert(hash, Vec::new());
            Ok(())
        }

        fn append(&mut self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
            self.kept.get_mut(hash).unwrap().extend_from_slice(bytes);
            Ok(())
        }

        fn finish(&mut self, hash: &Blake3) -> Result<AssetRange, Error> {
            let bytes = self.kept.remove(hash).unwrap();
            let len = bytes.len() as u64;
            AssetRange::new(Arc::new(Slow { bytes, started: self.started.clone(), gone: self.gone.clone() }), 0, len)
        }

        fn discard(&mut self, hash: &Blake3) {
            self.kept.remove(hash);
        }
    }

    /// A sharer and a joiner whose seal of the sharer's large asset is under way.
    fn sealing(dir: &std::path::Path) -> (Side, Side, Arc<AtomicBool>) {
        let (started, gone) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let spool = Trickle { kept: BTreeMap::new(), started: started.clone(), gone: gone.clone() };
        let joiner = Collab::join(room()).with_spool(Box::new(spool)).sealing();
        let (mut owner, mut joiner, _) = pair(dir, joiner);
        until(&mut [&mut owner, &mut joiner], "the seal to begin", |_| started.load(Ordering::Acquire));
        (owner, joiner, gone)
    }

    #[test]
    fn deleting_the_card_stops_the_seal_its_asset_is_in() {
        let dir = tempfile::tempdir().unwrap();
        let (mut owner, mut joiner, gone) = sealing(dir.path());
        assert!(!gone.load(Ordering::Acquire));
        owner.session.dispatch("node.remove", &serde_json::json!({"canvas": canvas(), "id": NodeId::from_bytes([1; 16])})).unwrap();
        until(&mut [&mut owner, &mut joiner], "the seal to stop and let go of its spool", |sides| {
            gone.load(Ordering::Acquire) && sides[1].status.waiting_for_assets == 0
        });
    }

    #[test]
    fn letting_go_of_the_worker_stops_a_seal_under_way() {
        let dir = tempfile::tempdir().unwrap();
        let (_owner, mut joiner, gone) = sealing(dir.path());
        assert!(!gone.load(Ordering::Acquire));
        joiner.worker = None;
        let deadline = Instant::now() + Duration::from_secs(60);
        while !gone.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "the seal went on");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_seal_thread_that_ends_without_saying_so_reports_a_failure() {
        let (to, inbox) = channel();
        let hash = Blake3::from_bytes([3; 32]);
        let done = SealDone { hash, to: Some(to) };
        // As when the seal panics, unwinding past it.
        drop(done);
        match inbox.try_recv() {
            Ok(Ask::Sealed { hash: said, result: Err(error) }) => {
                assert_eq!(said, hash);
                assert_eq!(error.kind, ErrorKind::Internal);
            }
            _ => panic!("no failure was reported"),
        }
        assert!(inbox.try_recv().is_err(), "said once");
    }
}
