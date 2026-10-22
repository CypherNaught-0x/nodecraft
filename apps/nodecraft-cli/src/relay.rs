//! `relay`: passes shared projects between the people working on them (#119). Each connection
//! is a WebSocket of binary frames (`nodecraft_sync::Frame`); its first frame names a room, and
//! the room merges and forwards what its peers send. The relay keeps nothing on disk and drops a
//! room when its last peer leaves. Room ids are the keys to the rooms: they never reach a log.
//! A peer may come in through a share link instead (#121): the relay knows only the link's hash,
//! finds the room that holds it, and lets the peer in with the link's role.
use std::collections::BTreeMap;
use std::io::ErrorKind as IoKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nodecraft_sync::{Frame, MAX_FRAME_BYTES, Out, PROTOCOL, Peer, Room, RoomId};
use nodecraft_types::{Error, ErrorKind};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// The most connections at once.
const MAX_CONNECTIONS: usize = 256;
/// The most rooms at once.
const MAX_ROOMS: usize = 64;
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

enum Push {
    Frame(Vec<u8>),
    /// Send this, then hang up.
    Close(Vec<u8>),
}

/// Where one peer's frames go, and how many bytes wait there.
struct Inbox {
    room: RoomId,
    sender: Sender<Push>,
    waiting: Arc<AtomicUsize>,
}

#[derive(Default)]
struct Rooms {
    rooms: BTreeMap<RoomId, Room>,
    peers: BTreeMap<Peer, Inbox>,
    /// When links were last checked for having run out.
    expired: u64,
}

/// Seconds since 1970, by the relay's clock.
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

impl Rooms {
    /// The room holding the link with this hash.
    fn holding(&self, link: &[u8; 32]) -> Option<RoomId> {
        self.rooms.iter().find(|(_, room)| room.grant(link).is_some()).map(|(id, _)| id.clone())
    }

    /// Puts out whoever's link ran out, at most once a second.
    fn expire(&mut self) {
        let now = now();
        if now == self.expired {
            return;
        }
        self.expired = now;
        let outs: Vec<Out> = self.rooms.values_mut().flat_map(|room| room.expire(now)).collect();
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

    fn leave(&mut self, peer: Peer) {
        if let Some(Inbox { room, .. }) = self.peers.remove(&peer)
            && let Some(state) = self.rooms.get_mut(&room)
        {
            state.leave(peer);
            if state.is_empty() {
                self.rooms.remove(&room);
            }
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

fn lock(rooms: &Mutex<Rooms>) -> MutexGuard<'_, Rooms> {
    // A connection that failed mid-way leaves the rooms as consistent as any frame does.
    rooms.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn start(listen: SocketAddr) -> Result<Relay, Error> {
    let listener = TcpListener::bind(listen).map_err(|_| Error::new(ErrorKind::Network, "the relay can't listen on that address"))?;
    let addr = listener.local_addr().map_err(|_| Error::new(ErrorKind::Network, "the relay's address isn't known"))?;
    let rooms = Arc::new(Mutex::new(Rooms::default()));
    let open = Arc::new(AtomicUsize::new(0));
    let next = Arc::new(AtomicU64::new(1));
    let accepting = thread::Builder::new()
        .name("relay".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (rooms, count, peer) = (Arc::clone(&rooms), Arc::clone(&open), next.fetch_add(1, Ordering::SeqCst));
                let spawned = thread::Builder::new().name("relay peer".into()).spawn(move || {
                    connection(stream, peer, &rooms);
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
fn connection(stream: TcpStream, peer: Peer, rooms: &Mutex<Rooms>) {
    if stream.set_read_timeout(Some(HELLO_WITHIN)).is_err() {
        return;
    }
    let Ok(mut socket) = tungstenite::accept_with_config(stream, Some(config(MAX_HELLO_BYTES))) else { return };
    let (sender, inbox) = channel();
    let waiting = Arc::new(AtomicUsize::new(0));
    if !enter(&mut socket, peer, sender, Arc::clone(&waiting), rooms) {
        return;
    }
    socket.set_config(|c| *c = config(MAX_FRAME_BYTES));
    if socket.get_ref().set_read_timeout(Some(POLL)).is_err() || socket.get_ref().set_write_timeout(Some(WRITE_WITHIN)).is_err() {
        return;
    }
    serve(&mut socket, peer, &inbox, &waiting, rooms);
}

/// Reads the hello and joins its room; `false` if the peer doesn't get in.
fn enter(socket: &mut WebSocket<TcpStream>, peer: Peer, sender: Sender<Push>, waiting: Arc<AtomicUsize>, rooms: &Mutex<Rooms>) -> bool {
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
        Ok(Frame::Hello { protocol: PROTOCOL, room }) => (Some(room), None),
        Ok(Frame::HelloLink { protocol: PROTOCOL, link }) => (None, Some(link.hash())),
        Ok(Frame::Hello { .. } | Frame::HelloLink { .. }) => return refuse(socket, "this relay speaks another version of the protocol"),
        _ => return refuse(socket, "a connection starts with a hello"),
    };
    let mut rooms = lock(rooms);
    if let Some(link) = link {
        let Some(room) = rooms.holding(&link) else {
            drop(rooms);
            return refuse(socket, "that share link doesn't open a project being shared now");
        };
        rooms.peers.insert(peer, Inbox { room: room.clone(), sender, waiting });
        let outs = rooms.rooms.get_mut(&room).map(|state| state.join_link(peer, link, now())).unwrap_or_default();
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
    rooms.peers.insert(peer, Inbox { room: room.clone(), sender, waiting });
    let outs = rooms.rooms.entry(room).or_default().join(peer);
    rooms.deliver(outs);
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
    loop {
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
                let Some(room) = rooms.peers.get(&peer).map(|inbox| inbox.room.clone()) else { return };
                let outs = match Frame::decode(&bytes) {
                    // What would add to what the relay holds waits until there is room for it.
                    Ok(Frame::Update(_) | Frame::Step2(_) | Frame::AssetPut { .. } | Frame::CommentPut { .. }) if rooms.held() > MAX_RELAY_BYTES => {
                        vec![Out::Close(peer, Frame::Refused("the relay is full".into()))]
                    }
                    // A link names one room: another room may not take it over.
                    Ok(Frame::LinkPut { hash, .. }) if rooms.holding(&hash).is_some_and(|holder| holder != room) => {
                        vec![Out::Close(peer, Frame::Refused("that share link belongs to another project".into()))]
                    }
                    Ok(frame) => rooms.rooms.get_mut(&room).map(|state| state.handle(peer, frame)).unwrap_or_default(),
                    Err(_) => vec![Out::Close(peer, Frame::Refused("a frame the relay can't read".into()))],
                };
                rooms.deliver(outs);
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if matches!(error.kind(), IoKind::WouldBlock | IoKind::TimedOut) => lock(rooms).expire(),
            Err(_) => return,
        }
    }
}
