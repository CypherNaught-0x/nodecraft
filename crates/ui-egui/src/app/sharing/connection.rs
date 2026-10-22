//! The connection to a relay (#698), on a thread of its own so a slow network never holds up a
//! frame. It reconnects by itself, waiting longer each time up to [`MAX_BACKOFF`], until the app
//! drops it. Each connection has a number; frames the app sends for an earlier one are dropped,
//! so nothing meant for a connection that has gone reaches the next before its hello.
use std::io::ErrorKind as IoKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use nodecraft_sync::MAX_FRAME_BYTES;
use nodecraft_types::{Error, ErrorKind};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// The longest wait between tries.
pub(super) const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How long connecting and the WebSocket handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a read waits before the thread looks for frames to send.
const POLL: Duration = Duration::from_millis(15);
/// Bytes of frames waiting for the app; past this the thread stops reading until the app catches
/// up, so a flooding relay can't fill memory.
const INBOX_BYTES: usize = 2 * MAX_FRAME_BYTES;
/// Bytes the app takes from the inbox in one frame; the rest wait for the next.
const TAKE_BYTES: usize = MAX_FRAME_BYTES;
/// What a frame costs in the inbox beyond its bytes, so empty frames count too.
const FRAME_COST: usize = 64;
/// Messages read in one pass before the thread sends what is waiting, so a busy room never holds
/// back this side's changes.
const READ_BURST: usize = 16;

/// How often the thread asks a quiet relay whether it is still there, and how long it waits for
/// anything at all before it takes the connection for lost.
#[derive(Clone, Copy)]
pub(super) struct Keepalive {
    pub(super) ping: Duration,
    pub(super) silent: Duration,
}

impl Default for Keepalive {
    fn default() -> Self {
        Self { ping: Duration::from_secs(10), silent: Duration::from_secs(30) }
    }
}

/// What the thread tells the app.
#[derive(Debug, PartialEq)]
pub(super) enum Event {
    /// Connection `n` is open; its first frame is the app's hello.
    Connected(u64),
    Frame(Vec<u8>),
    /// The connection closed or couldn't open; the next try is in `retry`.
    Dropped {
        why: String,
        retry: Duration,
    },
}

enum Out {
    Frame(u64, Vec<u8>),
}

/// The app's end. Dropping it ends the thread.
pub(super) struct Connection {
    out: Sender<Out>,
    events: Receiver<Event>,
    queued: Arc<AtomicUsize>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Connection {
    /// Connects to `relay` (a checked `ws://` address); `wake` asks for a frame when something
    /// comes in.
    pub(super) fn open(relay: String, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, Error> {
        Self::open_with(relay, wake, Keepalive::default())
    }

    pub(super) fn open_with(relay: String, wake: impl Fn() + Send + Sync + 'static, keepalive: Keepalive) -> Result<Self, Error> {
        let (out, outbox) = channel();
        let (inbox, events) = channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let inbox = Inbox { events: inbox, queued: Arc::clone(&queued) };
        let thread_wake = Arc::clone(&wake);
        std::thread::Builder::new()
            .name("nodecraft-share".into())
            .spawn(move || run(&relay, &outbox, &inbox, &*thread_wake, keepalive))
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the sharing connection: {e}")))?;
        Ok(Self { out, events, queued, wake })
    }

    /// Sends `frame` on connection `n`, if it is still the open one.
    pub(super) fn send(&self, n: u64, frame: Vec<u8>) {
        // The thread has ended only if the app dropped the connection.
        let _ = self.out.send(Out::Frame(n, frame));
    }

    /// What came in since the last call, up to [`TAKE_BYTES`] of frames; more asks for another
    /// frame.
    pub(super) fn events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut taken = 0usize;
        while taken < TAKE_BYTES {
            let Ok(event) = self.events.try_recv() else { return events };
            let cost = cost(&event);
            self.queued.fetch_sub(cost.min(self.queued.load(Ordering::Acquire)), Ordering::AcqRel);
            taken = taken.saturating_add(cost);
            events.push(event);
        }
        (self.wake)();
        events
    }
}

fn cost(event: &Event) -> usize {
    match event {
        Event::Frame(bytes) => bytes.len().saturating_add(FRAME_COST),
        _ => 0,
    }
}

/// The thread's end of what goes to the app, counting the bytes waiting there.
struct Inbox {
    events: Sender<Event>,
    queued: Arc<AtomicUsize>,
}

impl Inbox {
    /// Hands `event` to the app; `None` once the app has let go.
    fn send(&self, event: Event) -> Option<()> {
        self.queued.fetch_add(cost(&event), Ordering::AcqRel);
        self.events.send(event).ok()
    }

    fn full(&self) -> bool {
        self.queued.load(Ordering::Acquire) >= INBOX_BYTES
    }
}

type Socket = WebSocket<TcpStream>;

fn run(relay: &str, outbox: &Receiver<Out>, inbox: &Inbox, wake: &dyn Fn(), keepalive: Keepalive) {
    let mut n = 0u64;
    let mut backoff = Duration::from_secs(1);
    loop {
        let why = match connect(relay) {
            Ok(mut socket) => {
                n = n.saturating_add(1);
                backoff = Duration::from_secs(1);
                // Frames for the connection before this one go nowhere.
                while outbox.try_recv().is_ok() {}
                if inbox.send(Event::Connected(n)).is_none() {
                    return;
                }
                wake();
                match carry(&mut socket, n, outbox, inbox, wake, keepalive) {
                    Some(why) => why,
                    None => {
                        let _ = socket.close(None);
                        let _ = socket.flush();
                        return;
                    }
                }
            }
            Err(why) => why,
        };
        if inbox.send(Event::Dropped { why, retry: backoff }).is_none() {
            return;
        }
        wake();
        // Waits out the backoff, ending at once if the app lets go.
        let until = Instant::now() + backoff;
        loop {
            match outbox.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

/// Moves frames both ways until the connection drops (`Some(why)`) or the app lets go (`None`).
fn carry(socket: &mut Socket, n: u64, outbox: &Receiver<Out>, inbox: &Inbox, wake: &dyn Fn(), keepalive: Keepalive) -> Option<String> {
    let mut heard = Instant::now();
    let mut pinged = Instant::now();
    loop {
        if inbox.full() {
            // The app is behind, not the relay: wait for it without reading.
            heard = Instant::now();
            std::thread::sleep(POLL);
        } else {
            for _ in 0..READ_BURST {
                match socket.read() {
                    Ok(Message::Binary(bytes)) => {
                        heard = Instant::now();
                        inbox.send(Event::Frame(bytes.to_vec()))?;
                        wake();
                    }
                    Ok(Message::Close(_)) => return Some("the relay closed the connection".into()),
                    Ok(_) => heard = Instant::now(),
                    Err(tungstenite::Error::Io(e)) if matches!(e.kind(), IoKind::WouldBlock | IoKind::TimedOut) => break,
                    Err(e) => return Some(said(&e)),
                }
            }
        }
        if heard.elapsed() >= keepalive.silent {
            return Some("the relay stopped answering".into());
        }
        if pinged.elapsed() >= keepalive.ping {
            pinged = Instant::now();
            if let Err(e) = socket.send(Message::Ping(Default::default())) {
                return Some(said(&e));
            }
        }
        loop {
            match outbox.try_recv() {
                Ok(Out::Frame(to, frame)) if to == n => {
                    if let Err(e) = socket.send(Message::binary(frame)) {
                        return Some(said(&e));
                    }
                }
                Ok(Out::Frame(..)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }
}

fn config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(MAX_FRAME_BYTES)).max_frame_size(Some(MAX_FRAME_BYTES))
}

fn connect(relay: &str) -> Result<Socket, String> {
    let request = relay.into_client_request().map_err(|_| "the relay address isn't one NodeCraft can reach".to_string())?;
    let uri = request.uri();
    let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(80);
    let addresses = (host.as_str(), port).to_socket_addrs().map_err(|_| "the relay's host can't be found".to_string())?;
    let mut last = "the relay's host has no address".to_string();
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                stream.set_read_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| e.to_string())?;
                stream.set_write_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| e.to_string())?;
                match tungstenite::client::client_with_config(request.clone(), stream, Some(config())) {
                    Ok((socket, _)) => {
                        socket.get_ref().set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
                        return Ok(socket);
                    }
                    Err(tungstenite::HandshakeError::Failure(e)) => last = said(&e),
                    Err(tungstenite::HandshakeError::Interrupted(_)) => last = "the relay didn't answer in time".to_string(),
                }
            }
            Err(e) => last = io_said(&e),
        }
    }
    Err(last)
}

/// Why a connection failed, in words that hold no address or frame.
fn said(error: &tungstenite::Error) -> String {
    match error {
        tungstenite::Error::Io(e) => io_said(e),
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => "the relay closed the connection".into(),
        tungstenite::Error::Capacity(_) => "the relay sent more than a frame may hold".into(),
        tungstenite::Error::Http(response) => format!("the relay refused the connection ({})", response.status()),
        _ => "the connection to the relay failed".into(),
    }
}

fn io_said(error: &std::io::Error) -> String {
    match error.kind() {
        IoKind::ConnectionRefused => "the relay isn't accepting connections".into(),
        IoKind::TimedOut | IoKind::WouldBlock => "the relay didn't answer in time".into(),
        IoKind::ConnectionReset | IoKind::ConnectionAborted | IoKind::BrokenPipe | IoKind::UnexpectedEof => "the connection to the relay was lost".into(),
        _ => "the relay can't be reached".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Events as they come, kept until a test looks for them.
    struct Events<'a>(&'a Connection, std::collections::VecDeque<Event>);

    impl Events<'_> {
        /// The events up to and including the first for which `done` holds.
        #[track_caller]
        fn until(&mut self, done: impl Fn(&Event) -> bool) -> Vec<Event> {
            let started = Instant::now();
            let mut seen = Vec::new();
            while started.elapsed() < Duration::from_secs(10) {
                self.1.extend(self.0.events());
                while let Some(event) = self.1.pop_front() {
                    let last = done(&event);
                    seen.push(event);
                    if last {
                        return seen;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("never came: {seen:?}");
        }
    }

    #[test]
    fn frames_go_both_ways_and_a_dropped_connection_comes_back() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = format!("ws://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for round in 0..2u8 {
                let (stream, _) = listener.accept().unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                let hello = socket.read().unwrap();
                socket.send(Message::binary(vec![round, hello.into_data()[0]])).unwrap();
                // The first connection ends here; the client must come back.
            }
        });
        let connection = Connection::open(relay, || {}).unwrap();
        let mut events = Events(&connection, Default::default());
        let first = events.until(|e| matches!(e, Event::Connected(_)));
        assert_eq!(first.last(), Some(&Event::Connected(1)));
        connection.send(1, vec![7]);
        events.until(|e| *e == Event::Frame(vec![0, 7]));
        let dropped = events.until(|e| matches!(e, Event::Dropped { .. }));
        assert!(matches!(dropped.last(), Some(Event::Dropped { retry, .. }) if *retry == Duration::from_secs(1)));
        events.until(|e| *e == Event::Connected(2));
        // A frame for the connection that has gone is dropped; the hello for this one goes.
        connection.send(1, vec![8]);
        connection.send(2, vec![9]);
        events.until(|e| *e == Event::Frame(vec![1, 9]));
        server.join().unwrap();
    }

    #[test]
    fn a_relay_that_stops_answering_is_dropped_and_retried() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = format!("ws://{}/", listener.local_addr().unwrap());
        // Accepts and then never reads again, so pings go unanswered, as on a half-open socket.
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let socket = tungstenite::accept(stream).unwrap();
            std::thread::sleep(Duration::from_secs(2));
            drop(socket);
        });
        let keepalive = Keepalive { ping: Duration::from_millis(100), silent: Duration::from_millis(400) };
        let connection = Connection::open_with(relay, || {}, keepalive).unwrap();
        let seen = Events(&connection, Default::default()).until(|e| matches!(e, Event::Dropped { .. }));
        assert_eq!(seen.first(), Some(&Event::Connected(1)));
        assert!(matches!(seen.last(), Some(Event::Dropped { why, .. }) if why == "the relay stopped answering"), "{seen:?}");
        server.join().unwrap();
    }

    #[test]
    fn a_relay_that_isnt_there_is_retried_and_its_address_never_shown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let connection = Connection::open(format!("ws://{address}/"), || {}).unwrap();
        let seen = Events(&connection, Default::default()).until(|e| matches!(e, Event::Dropped { .. }));
        let Some(Event::Dropped { why, .. }) = seen.last() else { panic!("{seen:?}") };
        assert!(!why.contains(&address.port().to_string()), "{why}");
    }
}
