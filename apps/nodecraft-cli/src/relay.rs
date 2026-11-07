//! `relay`: passes shared projects between the people working on them (#119). Each connection
//! is a WebSocket of binary frames (`nodecraft_sync::Frame`); its first frame names a room, and
//! the room merges and forwards what its peers send. Without a store the relay keeps nothing on
//! disk and drops a room when its last peer leaves; given a folder to keep rooms in (#1068), it
//! keeps each room, its links and its media there ([`store`]), so a restart loses nothing and a
//! link works while its owner is away. Room ids are the keys to the rooms: they never reach a log
//! or the disk, where a room is named by a hash of its id.
//! A peer may come in through a share link instead (#121): the relay knows only the link's hash,
//! finds the room that holds it, and lets the peer in with the link's role.
mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind as IoKind;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nodecraft_sync::{Frame, MAX_FRAME_BYTES, Out, PROTOCOL, Peer, Role, Room};
use nodecraft_types::{Error, ErrorKind};
use store::{RoomKey, Snapshot, Store};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// The most connections at once.
const MAX_CONNECTIONS: usize = 256;
/// The most rooms at once.
const MAX_ROOMS: usize = 64;
/// The most connections from one address that haven't said yet which room they want. Past it,
/// more from there are turned away until some do or give up, so one address that opens many and
/// says nothing can't take every slot (#701). Behind a proxy every peer has its address.
const MAX_WAITING_PER_ADDRESS: usize = 16;
/// How long a new connection has to say which room it wants.
const HELLO_WITHIN: Duration = Duration::from_secs(10);
/// How often a connection looks for frames to send while it waits for one to arrive.
const POLL: Duration = Duration::from_millis(20);
/// How long a peer may take to accept a frame before the relay gives up on it.
const WRITE_WITHIN: Duration = Duration::from_secs(30);
/// The most bytes waiting for one peer: one frame of the largest size and then some. A peer
/// that falls further behind is dropped, whatever the number of frames.
const MAX_WAITING_BYTES: usize = MAX_FRAME_BYTES + (32 << 20);
/// The largest message before the hello: a hello is small.
const MAX_HELLO_BYTES: usize = 4096;
/// The most every room together may take in; past it, frames that add more are refused.
const MAX_RELAY_BYTES: usize = 2 << 30;
/// How often one peer may say where it is; the app says it at most every 50 ms.
const PRESENCE_EVERY: Duration = Duration::from_millis(25);
/// How often a room that changed is written to the store. A change to its links is written at
/// once.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// How often the store forgets the rooms nobody used for its keep.
const EXPIRE_STORED_EVERY: Duration = Duration::from_secs(3600);

/// Where the relay keeps its rooms, and for how long and how much (#1068).
#[derive(Clone, Debug)]
pub struct Keeping {
    pub dir: PathBuf,
    /// How long a room nobody uses is kept, in seconds.
    pub keep: u64,
    /// The most bytes every room together may take on disk.
    pub max_bytes: u64,
}

enum Push {
    Frame(Vec<u8>),
    /// Send this, then hang up.
    Close(Vec<u8>),
}

/// Where one peer's frames go, and how many bytes wait there.
struct Inbox {
    room: RoomKey,
    sender: Sender<Push>,
    waiting: Arc<AtomicUsize>,
}

#[derive(Default)]
struct Rooms {
    rooms: BTreeMap<RoomKey, Room>,
    peers: BTreeMap<Peer, Inbox>,
    /// When links were last checked for having run out.
    expired: u64,
    /// With a store: how many times each room in memory changed, and how many of those changes
    /// the store has. An empty room stays in memory until the store has all of it.
    changed: BTreeMap<RoomKey, u64>,
    saved: BTreeMap<RoomKey, u64>,
    /// The assets of each room the store already has.
    stored_assets: BTreeMap<RoomKey, BTreeSet<[u8; 32]>>,
    /// Wakes the saver, when there is a store.
    wake: Option<Sender<()>>,
}

/// Seconds since 1970, by the relay's clock.
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

impl Rooms {
    /// The room in memory holding the link with this hash.
    fn holding(&self, link: &[u8; 32]) -> Option<RoomKey> {
        self.rooms.iter().find(|(_, room)| room.grant(link).is_some()).map(|(key, _)| *key)
    }

    /// Notes that the room `key` changed, for the store; a change to its links is written at once.
    fn touch(&mut self, key: RoomKey, links: bool) {
        let Some(wake) = &self.wake else { return };
        let count = self.changed.entry(key).or_default();
        *count = count.saturating_add(1);
        if links {
            let _ = wake.send(());
        }
    }

    /// Puts out whoever's link ran out, at most once a second.
    fn expire(&mut self) {
        let now = now();
        if now == self.expired {
            return;
        }
        self.expired = now;
        let mut outs = Vec::new();
        let mut changed = Vec::new();
        for (key, room) in &mut self.rooms {
            let before = room.links().count();
            outs.extend(room.expire(now));
            if room.links().count() != before {
                changed.push(*key);
            }
        }
        for key in changed {
            self.touch(key, true);
        }
        self.deliver(outs);
    }

    /// Hands each frame to the connection it is for.
    fn deliver(&mut self, outs: Vec<Out>) {
        let mut behind = Vec::new();
        for out in outs {
            let (peer, frame, close) = match out {
                Out::Send(peer, frame) => (peer, frame, false),
                Out::Close(peer, frame) => (peer, frame, true),
            };
            let (Some(inbox), Ok(bytes)) = (self.peers.get(&peer), frame.encode()) else { continue };
            let size = bytes.len();
            if inbox.waiting.load(Ordering::SeqCst).saturating_add(size) > MAX_WAITING_BYTES {
                // Its connection hangs up when it sees its sender gone.
                behind.push(peer);
                continue;
            }
            inbox.waiting.fetch_add(size, Ordering::SeqCst);
            // A connection that went away leaves on its own.
            let _ = inbox.sender.send(if close { Push::Close(bytes) } else { Push::Frame(bytes) });
        }
        for peer in behind {
            self.leave(peer);
        }
    }

    fn held(&self) -> usize {
        self.rooms.values().map(Room::held).fold(0, usize::saturating_add)
    }

    /// Has every room that took in enough since it last measured measure what it holds.
    fn settle(&mut self) {
        for room in self.rooms.values_mut() {
            room.settle();
        }
    }

    /// Takes `peer` out of its room and tells the others it has gone. Each call removes a peer,
    /// so the leaving that delivering can cause ends.
    fn leave(&mut self, peer: Peer) {
        if let Some(Inbox { room, .. }) = self.peers.remove(&peer)
            && let Some(state) = self.rooms.get_mut(&room)
        {
            // Whoever was taking an asset from the peer asks the others for the rest.
            let again = state.leave(peer);
            // With a store, an empty room goes once the store has all of it.
            if state.is_empty() && self.wake.is_none() {
                self.rooms.remove(&room);
            } else if state.is_empty() && self.changed.contains_key(&room) {
                // The last peer leaving is the room's last use: the store notes when, at once.
                self.touch(room, true);
            }
            self.deliver(again);
        }
    }
}

pub struct Relay {
    pub addr: SocketAddr,
    accepting: thread::JoinHandle<()>,
}

impl Relay {
    pub fn wait(self) {
        let _ = self.accepting.join();
    }
}

fn lock<T>(held: &Mutex<T>) -> MutexGuard<'_, T> {
    // A connection that failed mid-way leaves the rooms as consistent as any frame does.
    held.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Connections waiting to say which room they want, by address.
type Waiting = Arc<Mutex<BTreeMap<IpAddr, usize>>>;

/// One connection counted as waiting until it is dropped.
struct Hello {
    waiting: Waiting,
    from: IpAddr,
}

impl Hello {
    /// Counts in a connection from `from`, if that address has room for another.
    fn new(waiting: &Waiting, from: IpAddr) -> Option<Self> {
        let mut held = lock(waiting);
        let count = held.entry(from).or_default();
        if *count >= MAX_WAITING_PER_ADDRESS {
            return None;
        }
        *count = count.saturating_add(1);
        Some(Self { waiting: Arc::clone(waiting), from })
    }
}

impl Drop for Hello {
    fn drop(&mut self) {
        let mut held = lock(&self.waiting);
        if let Some(count) = held.get_mut(&self.from) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                held.remove(&self.from);
            }
        }
    }
}

pub fn start(listen: SocketAddr, keeping: Option<Keeping>) -> Result<Relay, Error> {
    let store = match keeping {
        Some(keeping) => Some(Store::open(&keeping.dir, keeping.keep, keeping.max_bytes, now()).map_err(|message| Error::new(ErrorKind::Internal, message))?),
        None => None,
    };
    let listener = TcpListener::bind(listen).map_err(|_| Error::new(ErrorKind::Network, "the relay can't listen on that address"))?;
    let addr = listener.local_addr().map_err(|_| Error::new(ErrorKind::Network, "the relay's address isn't known"))?;
    let rooms = Arc::new(Mutex::new(Rooms::default()));
    let store = store.map(|store| Arc::new(Mutex::new(store)));
    if let Some(store) = &store {
        let (wake, woken) = channel();
        lock(&rooms).wake = Some(wake);
        let (rooms, store) = (Arc::clone(&rooms), Arc::clone(store));
        thread::Builder::new()
            .name("relay store".into())
            .spawn(move || saver(&rooms, &store, &woken))
            .map_err(|_| Error::new(ErrorKind::Internal, "the relay can't start"))?;
    }
    let open = Arc::new(AtomicUsize::new(0));
    let next = Arc::new(AtomicU64::new(1));
    let waiting = Waiting::default();
    let accepting = thread::Builder::new()
        .name("relay".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let Some(hello) = stream.peer_addr().ok().and_then(|addr| Hello::new(&waiting, addr.ip())) else { continue };
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (rooms, count, peer) = (Arc::clone(&rooms), Arc::clone(&open), next.fetch_add(1, Ordering::SeqCst));
                let store = store.clone();
                let spawned = thread::Builder::new().name("relay peer".into()).spawn(move || {
                    connection(stream, peer, &rooms, store.as_deref(), hello);
                    lock(&rooms).leave(peer);
                    count.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    // The closure (and its count) never ran.
                    open.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })
        .map_err(|_| Error::new(ErrorKind::Internal, "the relay can't start"))?;
    Ok(Relay { addr, accepting })
}

fn config(largest: usize) -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(largest)).max_frame_size(Some(largest))
}

/// Serves one peer until it leaves, is refused, or the connection fails.
fn connection(stream: TcpStream, peer: Peer, rooms: &Mutex<Rooms>, store: Option<&Mutex<Store>>, hello: Hello) {
    if stream.set_read_timeout(Some(HELLO_WITHIN)).is_err() {
        return;
    }
    let Ok(mut socket) = tungstenite::accept_with_config(stream, Some(config(MAX_HELLO_BYTES))) else { return };
    let (sender, inbox) = channel();
    let waiting = Arc::new(AtomicUsize::new(0));
    if !enter(&mut socket, peer, sender, Arc::clone(&waiting), rooms, store) {
        return;
    }
    drop(hello);
    socket.set_config(|c| *c = config(MAX_FRAME_BYTES));
    if socket.get_ref().set_read_timeout(Some(POLL)).is_err() || socket.get_ref().set_write_timeout(Some(WRITE_WITHIN)).is_err() {
        return;
    }
    serve(&mut socket, peer, &inbox, &waiting, rooms);
}

/// Reads the hello and joins its room; `false` if the peer doesn't get in.
fn enter(
    socket: &mut WebSocket<TcpStream>,
    peer: Peer,
    sender: Sender<Push>,
    waiting: Arc<AtomicUsize>,
    rooms: &Mutex<Rooms>,
    store: Option<&Mutex<Store>>,
) -> bool {
    let started = Instant::now();
    let hello = loop {
        match socket.read() {
            Ok(Message::Binary(bytes)) => break Frame::decode(&bytes),
            Ok(Message::Close(_)) | Err(_) => return false,
            Ok(_) if started.elapsed() < HELLO_WITHIN => continue,
            Ok(_) => return false,
        }
    };
    let (room, link) = match hello {
        Ok(Frame::Hello { protocol: PROTOCOL, room }) => (Some(store::key(&room)), None),
        Ok(Frame::HelloLink { protocol: PROTOCOL, link }) => (None, Some(link.hash())),
        Ok(Frame::Hello { .. } | Frame::HelloLink { .. }) => return refuse(socket, "this relay speaks another version of the protocol"),
        _ => return refuse(socket, "a connection starts with a hello"),
    };
    // A kept room comes back from the store before the peer goes in. It is read without the
    // rooms held, so a large room doesn't keep everyone else waiting.
    if let Some(store) = store {
        let wanted = match (&room, &link) {
            (Some(key), _) => Some(*key),
            (None, Some(link)) if lock(rooms).holding(link).is_none() => lock(store).holding(link, now()),
            _ => None,
        };
        if let Some(key) = wanted
            && !lock(rooms).rooms.contains_key(&key)
        {
            let mut held = lock(store);
            if held.has(&key) {
                let restored = held.load(&key);
                let assets = held.assets_of(&key);
                drop(held);
                if let Some(restored) = restored {
                    lock(rooms).bring_back(key, restored, assets);
                }
            }
        }
    }
    let mut rooms = lock(rooms);
    // The saver may have let the room go from memory between the two looks: read it again, now
    // with the rooms held, rather than start an empty room over the kept one.
    if let Some(store) = store {
        let key = match (&room, &link) {
            (Some(key), _) => Some(*key),
            (None, Some(link)) if rooms.holding(link).is_none() => lock(store).holding(link, now()),
            _ => None,
        };
        if let Some(key) = key.filter(|key| !rooms.rooms.contains_key(key)) {
            let mut held = lock(store);
            if let Some(restored) = held.has(&key).then(|| held.load(&key)).flatten() {
                let assets = held.assets_of(&key);
                drop(held);
                // Never an empty room over a kept one: past the relay's limits, the peer waits.
                if !rooms.bring_back(key, restored, assets) {
                    drop(rooms);
                    return refuse(socket, "the relay is full; try again in a moment");
                }
            }
        }
    }
    if let Some(link) = link {
        let Some(room) = rooms.holding(&link) else {
            drop(rooms);
            return refuse(socket, "that share link doesn't open a project being shared now");
        };
        rooms.peers.insert(peer, Inbox { room, sender, waiting });
        let outs = rooms.rooms.get_mut(&room).map(|state| state.join_link(peer, link, now())).unwrap_or_default();
        // A join is a use: the store keeps a room people visit, though nobody edits it.
        rooms.touch(room, false);
        let refused = outs.iter().any(|out| matches!(out, Out::Close(p, _) if *p == peer));
        rooms.deliver(outs);
        if refused {
            // The refusal is on its way through the inbox; the peer is no longer anyone's.
            rooms.leave(peer);
        }
        return true;
    }
    let Some(room) = room else { return false };
    if !rooms.rooms.contains_key(&room) && rooms.rooms.len() >= MAX_ROOMS {
        drop(rooms);
        return refuse(socket, "the relay has no room for another project");
    }
    rooms.peers.insert(peer, Inbox { room, sender, waiting });
    let known = rooms.rooms.contains_key(&room);
    let outs = rooms.rooms.entry(room).or_default().join(peer);
    rooms.deliver(outs);
    // A join of a room the relay has is a use. A new room counts once its owner sends something,
    // so a hello with a made-up id leaves nothing in the store.
    if known {
        rooms.touch(room, false);
    }
    true
}

fn refuse(socket: &mut WebSocket<TcpStream>, message: &str) -> bool {
    if let Ok(bytes) = Frame::Refused(message.into()).encode() {
        let _ = socket.send(Message::binary(bytes));
    }
    let _ = socket.close(None);
    let _ = socket.flush();
    false
}

fn serve(socket: &mut WebSocket<TcpStream>, peer: Peer, inbox: &Receiver<Push>, waiting: &AtomicUsize, rooms: &Mutex<Rooms>) {
    // When the peer last said where it is, and the latest it said too soon after: that one is
    // passed on once PRESENCE_EVERY is up, so the others see where it stopped.
    let mut moved: Option<Instant> = None;
    let mut held: Option<Frame> = None;
    loop {
        if !moved.is_some_and(|at| at.elapsed() < PRESENCE_EVERY)
            && let Some(frame) = held.take()
        {
            moved = Some(Instant::now());
            let mut rooms = lock(rooms);
            let Some(room) = rooms.peers.get(&peer).map(|inbox| inbox.room) else { return };
            let outs = rooms.rooms.get_mut(&room).map(|state| state.handle(peer, frame)).unwrap_or_default();
            rooms.deliver(outs);
        }
        loop {
            let push = match inbox.try_recv() {
                Ok(push) => push,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            };
            let (bytes, close) = match push {
                Push::Frame(bytes) => (bytes, false),
                Push::Close(bytes) => (bytes, true),
            };
            let size = bytes.len();
            // A peer that takes longer than WRITE_WITHIN to accept a frame is dropped.
            if socket.send(Message::binary(bytes)).is_err() {
                return;
            }
            waiting.fetch_sub(size.min(waiting.load(Ordering::SeqCst)), Ordering::SeqCst);
            if close {
                let _ = socket.close(None);
                let _ = socket.flush();
                return;
            }
        }
        match socket.read() {
            Ok(Message::Binary(bytes)) => {
                let mut rooms = lock(rooms);
                let Some(room) = rooms.peers.get(&peer).map(|inbox| inbox.room) else { return };
                let frame = Frame::decode(&bytes);
                // What the store keeps of a room: its document, its media and its links.
                let changes = matches!(
                    frame,
                    Ok(Frame::Update(_) | Frame::Step2(_) | Frame::AssetPut { .. } | Frame::CommentPut { .. } | Frame::LinkPut { .. } | Frame::LinkRevoke(_))
                );
                let links = matches!(frame, Ok(Frame::LinkPut { .. } | Frame::LinkRevoke(_)));
                let grows = matches!(frame, Ok(Frame::Update(_) | Frame::Step2(_) | Frame::AssetPut { .. } | Frame::CommentPut { .. }));
                if grows && rooms.held() > MAX_RELAY_BYTES {
                    // Rooms count what they merged until they measure: measure before refusing.
                    rooms.settle();
                }
                let outs = match frame {
                    Ok(frame @ Frame::Presence(_)) if moved.is_some_and(|at| at.elapsed() < PRESENCE_EVERY) => {
                        held = Some(frame);
                        Vec::new()
                    }
                    // A presence replaces the one the room holds for this peer, so it isn't
                    // weighed against MAX_RELAY_BYTES.
                    Ok(frame @ Frame::Presence(_)) => {
                        moved = Some(Instant::now());
                        rooms.rooms.get_mut(&room).map(|state| state.handle(peer, frame)).unwrap_or_default()
                    }
                    // What would add to what the relay holds waits until there is room for it.
                    Ok(Frame::Update(_) | Frame::Step2(_) | Frame::AssetPut { .. } | Frame::CommentPut { .. }) if rooms.held() > MAX_RELAY_BYTES => {
                        vec![Out::Close(peer, Frame::Refused("the relay is full".into()))]
                    }
                    // A link names one room: another room may not take it over. Only an owner
                    // learns that a hash is taken; anyone else is refused by the room.
                    Ok(Frame::LinkPut { hash, .. })
                        if rooms.rooms.get(&room).and_then(|state| state.role_of(peer)) == Some(Role::Owner)
                            && rooms.holding(&hash).is_some_and(|holder| holder != room) =>
                    {
                        vec![Out::Close(peer, Frame::Refused("that share link belongs to another project".into()))]
                    }
                    Ok(frame) => rooms.rooms.get_mut(&room).map(|state| state.handle(peer, frame)).unwrap_or_default(),
                    Err(_) => vec![Out::Close(peer, Frame::Refused("a frame the relay can't read".into()))],
                };
                rooms.deliver(outs);
                if changes {
                    rooms.touch(room, links);
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if matches!(error.kind(), IoKind::WouldBlock | IoKind::TimedOut) => lock(rooms).expire(),
            Err(_) => return,
        }
    }
}

/// Writes the rooms that changed to the store, every [`SAVE_EVERY`] or when woken, and lets an
/// empty room go once the store has all of it. Only this thread writes the store, so what it
/// writes is always the latest it took. Each room is taken under the rooms' lock and written
/// without it.
fn saver(rooms: &Mutex<Rooms>, store: &Mutex<Store>, woken: &Receiver<()>) {
    let mut expired = Instant::now();
    let mut full = false;
    loop {
        match woken.recv_timeout(SAVE_EVERY) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // Whatever else woke it meanwhile is in this pass.
        while woken.try_recv().is_ok() {}
        let (snapshots, busy, in_memory) = {
            let mut held = lock(rooms);
            let busy: BTreeSet<RoomKey> = held.peers.values().map(|inbox| inbox.room).collect();
            let in_memory: Vec<RoomKey> = held.rooms.keys().copied().collect();
            let mut snapshots = Vec::new();
            let mut gone = Vec::new();
            for (key, room) in &held.rooms {
                // A room nobody changed (or joined) since it came into memory has nothing to keep:
                // a stranger's hello with a new id never takes a place in the store.
                let Some(&at) = held.changed.get(key) else {
                    if room.is_empty() {
                        gone.push(*key);
                    }
                    continue;
                };
                // Written already, with every change since it came into memory.
                if held.saved.get(key) == Some(&at) {
                    if room.is_empty() {
                        gone.push(*key);
                    }
                    continue;
                }
                // A damaged room is kept as it was last written.
                let Some(document) = room.document() else { continue };
                let stored = held.stored_assets.get(key);
                let assets = room
                    .assets()
                    .filter(|(hash, _)| stored.is_none_or(|stored| !stored.contains(*hash)))
                    .map(|(hash, bytes)| (*hash, bytes.to_vec()))
                    .collect();
                let links = room.links().map(|(hash, grant)| (*hash, grant.clone())).collect();
                snapshots.push((*key, at, Snapshot { document, links, assets, used: now() }));
            }
            for key in gone {
                held.forget(key);
            }
            (snapshots, busy, in_memory)
        };
        let mut done = Vec::new();
        let lost: Vec<RoomKey>;
        {
            let mut store = lock(store);
            if expired.elapsed() >= EXPIRE_STORED_EVERY {
                expired = Instant::now();
                store.expire(now(), &busy);
            }
            for (key, at, snapshot) in snapshots {
                let hashes: BTreeSet<[u8; 32]> = snapshot.assets.iter().map(|(hash, _)| *hash).collect();
                match store.save(key, snapshot, &busy) {
                    Ok(()) => {
                        full = false;
                        done.push((key, at, hashes));
                    }
                    Err(not_kept) => {
                        // Said once until a save succeeds again, without naming the room.
                        if !full {
                            let why = if not_kept == store::NotKept::Full { "the store is full" } else { "it can't write its store folder" };
                            let _ = writeln!(std::io::stderr(), "The relay couldn't keep a room: {why}.");
                        }
                        full = true;
                        // Fail closed: an older copy could hold a link revoked since, so it goes.
                        // The room counts as saved, so an empty one leaves memory; the next change
                        // tries again.
                        store.discard(&key);
                        done.push((key, at, BTreeSet::new()));
                    }
                }
            }
            // A room the store let go to make space is written whole again at its next save.
            lost = in_memory.into_iter().filter(|key| !store.has(key)).collect();
        }
        let mut held = lock(rooms);
        for (key, at, hashes) in done {
            held.saved.insert(key, at);
            held.stored_assets.entry(key).or_default().extend(hashes);
            let unchanged = held.changed.get(&key).copied().unwrap_or(0) == at;
            if unchanged && held.rooms.get(&key).is_some_and(Room::is_empty) {
                held.forget(key);
            }
        }
        for key in lost {
            held.stored_assets.remove(&key);
            held.saved.remove(&key);
        }
    }
}

impl Rooms {
    /// Puts a room read back from the store into memory, as the store has it, if there is room
    /// for it: no more rooms than [`MAX_ROOMS`] and no more bytes than [`MAX_RELAY_BYTES`].
    /// Whether it is in memory now.
    fn bring_back(&mut self, key: RoomKey, restored: Room, assets: BTreeSet<[u8; 32]>) -> bool {
        if self.rooms.contains_key(&key) {
            return true;
        }
        if self.rooms.len() >= MAX_ROOMS || self.held().saturating_add(restored.held()) > MAX_RELAY_BYTES {
            return false;
        }
        self.rooms.insert(key, restored);
        self.changed.insert(key, 0);
        self.saved.insert(key, 0);
        self.stored_assets.insert(key, assets);
        true
    }

    /// Lets an empty room go from memory; the store keeps it.
    fn forget(&mut self, key: RoomKey) {
        self.rooms.remove(&key);
        self.changed.remove(&key);
        self.saved.remove(&key);
        self.stored_assets.remove(&key);
    }
}
