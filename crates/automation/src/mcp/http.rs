//! The MCP Streamable HTTP transport (#116), native only.
//!
//! One endpoint, [`ENDPOINT`], on a loopback address. Each `POST` carries one JSON-RPC message;
//! a request is answered with one `application/json` reply, a notification or response with
//! `202 Accepted`. The server never streams (`GET` is `405`, which the specification allows), so
//! no Server-Sent Events are involved. `initialize` opens a session whose id comes back in
//! `Mcp-Session-Id`; later requests must carry it, and `DELETE` ends it.
//!
//! Secure by default, whatever the caller does:
//! - it binds only to loopback addresses, so other machines can't reach it;
//! - every request needs `Authorization: Bearer <token>`, compared in constant time;
//! - the `Host` header must name a loopback host and this port, and an `Origin`, when sent, must
//!   be a loopback one: a web page that rebinds its own name to 127.0.0.1 (DNS rebinding) is
//!   refused before anything else is read. No CORS headers are ever sent;
//! - request heads, header counts and bodies are capped ([`MAX_HEAD_BYTES`], [`MAX_HEADERS`],
//!   [`super::MAX_MESSAGE_BYTES`]), every read has a deadline, and at most [`MAX_CONNECTIONS`]
//!   connections are served at once.
//!
//! Connection threads only parse, check and route. Every message is handled, in arrival order,
//! by whoever calls [`HttpServer::serve`]: the thread that owns the [`super::McpServer`], which
//! isn't `Send`. Sessions are protocol bookkeeping: all of them share that one server (one
//! project, or the bridged app), as stdio clients of one process would.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

use super::{INVALID_REQUEST, MAX_MESSAGE_BYTES, PARSE_ERROR, PROTOCOL_VERSIONS};

/// The one path the server answers.
pub const ENDPOINT: &str = "/mcp";
/// Longest request line and headers accepted, in bytes.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Most header lines accepted in one request.
pub const MAX_HEADERS: usize = 64;
/// Connections served at once; more are answered `503` and closed.
pub const MAX_CONNECTIONS: usize = 16;
/// Sessions remembered at once; a new one past this ends the least recently used.
pub const MAX_SESSIONS: usize = 32;
/// Messages waiting for the serving thread, across all connections.
const QUEUE: usize = 64;
/// Shortest and longest bearer tokens accepted.
pub const MIN_TOKEN_BYTES: usize = 32;
pub const MAX_TOKEN_BYTES: usize = 256;

/// How long each part of an exchange may take (tests shorten them).
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// Reading a request's line and headers: from its first byte, or from connecting for a
    /// connection's first request.
    pub head: Duration,
    /// Reading its body.
    pub body: Duration,
    /// An open connection waiting for its next request.
    pub idle: Duration,
    /// Sending a reply.
    pub write: Duration,
    /// A message waiting for the serving thread to start it. One not started by then is dropped
    /// and answered `503`, never run late; so is one whose client disconnected while it waited.
    /// Shorter than common client timeouts (60 s), so a client's retry finds the first attempt
    /// already refused rather than still queued.
    pub queue: Duration,
    /// The serving thread finishing a message it started. A command that is still running when
    /// this passes is answered `503` but may still finish.
    pub answer: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            head: Duration::from_secs(10),
            body: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            write: Duration::from_secs(30),
            queue: Duration::from_secs(30),
            // Longer than the bridge's 30 s and a provider estimate's 62 s, so those report
            // their own timeouts first.
            answer: Duration::from_secs(120),
        }
    }
}

/// The secret every request must present as `Authorization: Bearer <token>`.
#[derive(Clone, PartialEq, Eq)]
pub struct BearerToken(String);

impl std::fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerToken(…)")
    }
}

impl BearerToken {
    /// 32 bytes from the operating system's random source, as 64 hex digits.
    pub fn generate() -> Result<Self, Error> {
        Ok(Self(crate::control::Token::generate()?.to_hex()))
    }

    /// A token chosen elsewhere (`NODECRAFT_MCP_TOKEN`, the keychain): 32–256 characters from
    /// the bearer-token alphabet (letters, digits and `-._~+/=`). The error never repeats it.
    pub fn new(token: &str) -> Result<Self, Error> {
        let ok = (MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len())
            && token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'='));
        if !ok {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("an MCP token must be {MIN_TOKEN_BYTES}–{MAX_TOKEN_BYTES} characters of letters, digits and -._~+/="),
            ));
        }
        Ok(Self(token.to_owned()))
    }

    /// The token itself, for the person to copy into their client. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The last four characters, for telling tokens apart without showing them.
    pub fn masked(&self) -> String {
        let tail: String = self.0.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    }

    /// Whether `presented` is this token, compared in constant time for tokens of its length.
    pub fn matches(&self, presented: &str) -> bool {
        if presented.len() != self.0.len() {
            return false;
        }
        presented.bytes().zip(self.0.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
    }
}

/// Where to listen and the token to require.
#[derive(Debug, Clone)]
pub struct Config {
    /// A loopback address; port 0 picks a free one.
    pub addr: SocketAddr,
    pub token: BearerToken,
    pub timeouts: Timeouts,
}

impl Config {
    pub fn new(addr: SocketAddr, token: BearerToken) -> Self {
        Self { addr, token, timeouts: Timeouts::default() }
    }
}

/// A [`Job`]'s progress, settled once: the serving thread starts it, or its connection gives up
/// on it first, never both.
const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const ABANDONED: u8 = 2;

/// One message waiting for the serving thread.
struct Job {
    message: Value,
    reply: SyncSender<Option<Value>>,
    /// When it must have started.
    start_by: Instant,
    state: Arc<AtomicU8>,
}

/// Moves `state` from queued to `to`; `false` when it had already left the queue.
fn settle(state: &AtomicU8, to: u8) -> bool {
    state.compare_exchange(QUEUED, to, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

/// The sessions `initialize` opened, least recently used first.
#[derive(Default)]
struct Sessions(VecDeque<String>);

impl Sessions {
    fn open(&mut self, id: String) {
        while self.0.len() >= MAX_SESSIONS {
            self.0.pop_front();
        }
        self.0.push_back(id);
    }

    /// Whether `id` is open, marking it used.
    fn touch(&mut self, id: &str) -> bool {
        match self.0.iter().position(|open| open == id) {
            Some(at) => {
                if let Some(found) = self.0.remove(at) {
                    self.0.push_back(found);
                }
                true
            }
            None => false,
        }
    }

    fn close(&mut self, id: &str) -> bool {
        let before = self.0.len();
        self.0.retain(|open| open != id);
        self.0.len() != before
    }
}

/// What connection threads share.
struct Shared {
    token: BearerToken,
    port: u16,
    timeouts: Timeouts,
    sessions: Mutex<Sessions>,
    to_server: SyncSender<Job>,
    stopped: Arc<AtomicBool>,
}

/// Stops a running [`HttpServer`] from any thread: no new connections or messages are taken,
/// [`HttpServer::serve`] returns and open connections close at their next request.
#[derive(Clone)]
pub struct Stopper {
    stopped: Arc<AtomicBool>,
    addr: SocketAddr,
    /// Set once the accept thread has closed the listening socket.
    released: Arc<(Mutex<bool>, Condvar)>,
}

/// How long [`Stopper::stop`] waits for the port to be let go.
const RELEASE_WAIT: Duration = Duration::from_secs(2);

impl Stopper {
    /// Stops the server and waits (briefly) until its port is free, so it can be bound again.
    pub fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            // Wake the accept loop so it sees the flag and lets the port go.
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        }
        let (lock, signal) = &*self.released;
        let released = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = signal.wait_timeout_while(released, RELEASE_WAIT, |released| !*released);
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// A bound server: connections are accepted from [`bind`] on; messages wait for [`Self::serve`].
pub struct HttpServer {
    addr: SocketAddr,
    jobs: Receiver<Job>,
    stopper: Stopper,
}

/// Listens on `config.addr`, which must be a loopback address, and starts accepting
/// connections. Fails with `Unsupported` for any other address or when the port is taken.
pub fn bind(config: Config) -> Result<HttpServer, Error> {
    if !config.addr.ip().is_loopback() {
        return Err(Error::new(ErrorKind::Unsupported, "the MCP server listens only on this computer (127.0.0.1 or ::1), never on a network address"));
    }
    let socket = TcpListener::bind(config.addr).map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't listen on {}: {e}", config.addr)))?;
    let addr = socket.local_addr().map_err(|e| Error::new(ErrorKind::Unsupported, e.to_string()))?;
    let (to_server, jobs) = sync_channel::<Job>(QUEUE);
    let stopped = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared {
        token: config.token,
        port: addr.port(),
        timeouts: config.timeouts,
        sessions: Mutex::new(Sessions::default()),
        to_server,
        stopped: Arc::clone(&stopped),
    });
    let open = Arc::new(AtomicUsize::new(0));
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::clone(&released);
    std::thread::Builder::new()
        .name("nodecraft-mcp-http".into())
        .spawn(move || {
            for stream in socket.incoming() {
                if shared.stopped.load(Ordering::SeqCst) {
                    break;
                }
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => {
                        // Out of file descriptors and the like: don't spin while it lasts.
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                };
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    let mut stream = stream;
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                    let _ = write_response(&mut stream, &Response::error(503, "too many connections to the MCP server").closing());
                    continue;
                }
                let (shared, slot) = (Arc::clone(&shared), Arc::clone(&open));
                let spawned = std::thread::Builder::new().name("nodecraft-mcp-http-conn".into()).spawn(move || {
                    serve_connection(stream, &shared);
                    slot.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    open.fetch_sub(1, Ordering::SeqCst);
                }
            }
            drop(socket);
            let (lock, signal) = &*release;
            *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
            signal.notify_all();
        })
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the MCP server: {e}")))?;
    Ok(HttpServer { addr, jobs, stopper: Stopper { stopped, addr, released } })
}

impl HttpServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The address clients connect to, such as `http://127.0.0.1:41116/mcp`.
    pub fn url(&self) -> String {
        url(self.addr)
    }

    pub fn stopper(&self) -> Stopper {
        self.stopper.clone()
    }

    /// Handles messages one at a time, in arrival order, until stopped. `handle` is
    /// [`super::McpServer::handle_message`]: a reply for a request, `None` otherwise.
    pub fn serve(self, handle: &mut dyn FnMut(Value) -> Option<Value>) {
        while !self.stopper.is_stopped() {
            let job = match self.jobs.recv_timeout(Duration::from_millis(200)) {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // Too late, or its connection gave up on it (and said so): running it now could run
            // it twice once the client retries.
            if Instant::now() >= job.start_by {
                settle(&job.state, ABANDONED);
                continue;
            }
            if !settle(&job.state, STARTED) {
                continue;
            }
            let reply = handle(job.message);
            let _ = job.reply.send(reply);
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stopper.stop();
    }
}

/// The endpoint's URL at `addr`.
pub fn url(addr: SocketAddr) -> String {
    match addr.ip() {
        IpAddr::V6(ip) => format!("http://[{ip}]:{}{ENDPOINT}", addr.port()),
        IpAddr::V4(ip) => format!("http://{ip}:{}{ENDPOINT}", addr.port()),
    }
}

/// A reply about to be written.
#[derive(Debug)]
struct Response {
    status: u16,
    body: Vec<u8>,
    content_type: Option<&'static str>,
    headers: Vec<(&'static str, String)>,
    close: bool,
}

impl Response {
    fn json(status: u16, body: &Value) -> Self {
        Self { status, body: body.to_string().into_bytes(), content_type: Some("application/json"), headers: Vec::new(), close: false }
    }

    /// A transport-level refusal: a JSON-RPC error with no id, so any client can read it.
    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": INVALID_REQUEST, "message": message}}))
    }

    fn empty(status: u16) -> Self {
        Self { status, body: Vec::new(), content_type: None, headers: Vec::new(), close: false }
    }

    fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn closing(mut self) -> Self {
        self.close = true;
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        417 => "Expectation Failed",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        505 => "HTTP Version Not Supported",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    if let Some(kind) = response.content_type {
        head.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if response.close {
        head.push_str("Connection: close\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// A parsed request line and headers.
#[derive(Debug)]
struct Head {
    method: String,
    target: String,
    /// HTTP/1.0 closes after each exchange unless kept alive.
    http10: bool,
    /// Names lowercased.
    headers: Vec<(String, String)>,
}

impl Head {
    /// The one value of header `name`; `Err` when it is sent more than once.
    fn one(&self, name: &str) -> Result<Option<&str>, ()> {
        let mut found = self.headers.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str());
        let first = found.next();
        if found.next().is_some() { Err(()) } else { Ok(first) }
    }

    fn keep_alive(&self) -> bool {
        let connection = self.headers.iter().filter(|(n, _)| n == "connection").flat_map(|(_, v)| v.split(',')).map(|t| t.trim().to_ascii_lowercase());
        let tokens: Vec<String> = connection.collect();
        if self.http10 { tokens.iter().any(|t| t == "keep-alive") } else { !tokens.iter().any(|t| t == "close") }
    }
}

/// Why a head couldn't be read.
enum HeadError {
    /// The connection ended or went quiet between requests: just close.
    Gone,
    /// Answer this, then close.
    Refuse(Response),
}

/// Reads one request head into `buf` (bytes after it stay in `buf` for the body). Waits for its
/// first byte up to the idle timeout (the head timeout for a connection's first request), then for
/// the rest up to the head timeout, checking `stopped`
/// at least every second.
fn read_head(stream: &mut TcpStream, buf: &mut Vec<u8>, shared: &Shared, first: bool) -> Result<Head, HeadError> {
    // A new connection must send its first request promptly; a kept-alive one may idle.
    let wait = if first { shared.timeouts.head } else { shared.timeouts.idle };
    let idle_until = Instant::now().checked_add(wait).unwrap_or_else(Instant::now);
    let mut head_until: Option<Instant> = None;
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = find(buf, b"\r\n\r\n") {
            let head = buf.get(..end).unwrap_or_default().to_vec();
            buf.drain(..end.saturating_add(4).min(buf.len()));
            return parse_head(&head).map_err(HeadError::Refuse);
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(HeadError::Refuse(Response::error(431, &format!("the request head is larger than {MAX_HEAD_BYTES} bytes")).closing()));
        }
        if shared.stopped.load(Ordering::SeqCst) {
            return Err(HeadError::Gone);
        }
        if head_until.is_none() && !buf.is_empty() {
            head_until = Instant::now().checked_add(shared.timeouts.head);
        }
        let until = head_until.unwrap_or(idle_until);
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(if buf.is_empty() { HeadError::Gone } else { HeadError::Refuse(Response::error(408, "the request head took too long").closing()) });
        }
        if stream.set_read_timeout(Some(left.min(Duration::from_secs(1)))).is_err() {
            return Err(HeadError::Gone);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(HeadError::Gone),
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(_) => return Err(HeadError::Gone),
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_head(bytes: &[u8]) -> Result<Head, Response> {
    let bad = |m: &str| Response::error(400, m).closing();
    let text = std::str::from_utf8(bytes).map_err(|_| bad("the request head isn't valid UTF-8"))?;
    let mut lines = text.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut parts = request.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(bad("malformed request line"));
    };
    let http10 = match version {
        "HTTP/1.1" => false,
        "HTTP/1.0" => true,
        _ => return Err(Response::error(505, "only HTTP/1.1 is spoken here").closing()),
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(bad("malformed request method"));
    }
    let mut headers = Vec::new();
    for line in lines {
        if headers.len() >= MAX_HEADERS {
            return Err(Response::error(431, &format!("more than {MAX_HEADERS} headers")).closing());
        }
        // Folded continuation lines are obsolete and a classic smuggling vector.
        if line.starts_with([' ', '\t']) {
            return Err(bad("folded header lines aren't accepted"));
        }
        let Some((name, value)) = line.split_once(':') else { return Err(bad("malformed header line")) };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
            return Err(bad("malformed header name"));
        }
        if value.bytes().any(|b| b.is_ascii_control() && b != b'\t') {
            return Err(bad("malformed header value"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim_matches([' ', '\t']).to_owned()));
    }
    Ok(Head { method: method.to_owned(), target: target.to_owned(), http10, headers })
}

/// Whether `host` (a `Host` header, or the authority of an `Origin`) names this computer, with
/// `port` when one is required.
fn loopback_authority(authority: &str, port: Option<u16>) -> bool {
    let (name, given) = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((ip, "")) => (ip, None),
            Some((ip, tail)) => match tail.strip_prefix(':') {
                Some(p) => (ip, Some(p)),
                None => return false,
            },
            None => return false,
        }
    } else {
        let (name, port) = match authority.rsplit_once(':') {
            Some((name, p)) => (name, Some(p)),
            None => (authority, None),
        };
        // An IPv6 address must be bracketed (`[::1]:41116`); `::1:41116` is ambiguous.
        if name.contains(':') {
            return false;
        }
        (name, port)
    };
    let name_ok = name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    let port_ok = match (given, port) {
        (Some(p), Some(want)) => p.parse::<u16>().is_ok_and(|p| p == want),
        (Some(p), None) => p.parse::<u16>().is_ok(),
        // A Host without a port means the scheme's default, which this server never is.
        (None, Some(_)) => false,
        (None, None) => true,
    };
    name_ok && port_ok
}

/// `Origin` values a page on this computer sends.
fn loopback_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    !authority.contains('/') && loopback_authority(authority, None)
}

/// The checks every request passes before its body is read: where it came from, who sent it and
/// what it asks for. `None` when it may proceed.
fn admit(head: &Head, shared: &Shared) -> Option<Response> {
    let forbidden = |m: &str| Some(Response::error(403, m).closing());
    match head.one("host") {
        Ok(Some(host)) if loopback_authority(host, Some(shared.port)) => {}
        Err(()) => return forbidden("more than one Host header"),
        _ => return forbidden("the Host header must name this computer (127.0.0.1, ::1 or localhost) and the server's port"),
    }
    match head.one("origin") {
        Ok(None) => {}
        Ok(Some(origin)) if loopback_origin(origin) => {}
        _ => return forbidden("requests from web pages are refused; only programs on this computer may connect"),
    }
    let path = head.target.split('?').next().unwrap_or_default();
    if path != ENDPOINT {
        return Some(Response::error(404, &format!("the MCP endpoint is {ENDPOINT}")).closing());
    }
    let presented = match head.one("authorization") {
        Ok(Some(value)) => value.split_once(' ').filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer")).map(|(_, token)| token.trim()),
        _ => None,
    };
    if !presented.is_some_and(|token| shared.token.matches(token)) {
        return Some(
            Response::error(401, "send `Authorization: Bearer <token>` with the token NodeCraft showed")
                .header("WWW-Authenticate", "Bearer realm=\"nodecraft-mcp\"")
                .closing(),
        );
    }
    if let Ok(Some(version)) = head.one("mcp-protocol-version")
        && !PROTOCOL_VERSIONS.contains(&version)
    {
        return Some(Response::error(400, &format!("unsupported MCP-Protocol-Version; this server speaks {}", PROTOCOL_VERSIONS.join(", "))).closing());
    }
    if head.one("mcp-protocol-version").is_err() || head.one("mcp-session-id").is_err() {
        return Some(Response::error(400, "repeated MCP headers").closing());
    }
    None
}

/// Serves one connection: requests in turn until it closes, idles out or breaks the protocol.
fn serve_connection(mut stream: TcpStream, shared: &Shared) {
    if stream.set_write_timeout(Some(shared.timeouts.write)).is_err() {
        return;
    }
    let mut buf = Vec::new();
    let mut first = true;
    loop {
        let head = match read_head(&mut stream, &mut buf, shared, first) {
            Ok(head) => head,
            Err(HeadError::Gone) => return,
            Err(HeadError::Refuse(response)) => {
                if write_response(&mut stream, &response).is_ok() {
                    close_gently(&mut stream);
                }
                return;
            }
        };
        first = false;
        let keep = head.keep_alive();
        let mut response = exchange(&mut stream, &mut buf, &head, shared);
        if !keep {
            response.close = true;
        }
        if write_response(&mut stream, &response).is_err() {
            return;
        }
        if response.close {
            close_gently(&mut stream);
            return;
        }
    }
}

/// Ends a connection after a reply without losing it: closing with unread request bytes makes
/// the system reset the connection, and the client may never see the reply. Bounded: a short
/// wait and at most 64 KiB read.
fn close_gently(stream: &mut TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut sink = [0u8; 8192];
    for _ in 0..8 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// Answers one admitted-or-not request whose head has been read.
fn exchange(stream: &mut TcpStream, buf: &mut Vec<u8>, head: &Head, shared: &Shared) -> Response {
    if let Some(refusal) = admit(head, shared) {
        return refusal;
    }
    // Only POST bodies are read: a request of another method that carries one would leave it in
    // the connection, to be taken for the next request.
    let has_body = head.headers.iter().any(|(n, v)| n == "transfer-encoding" || (n == "content-length" && v.trim() != "0"));
    if head.method != "POST" && has_body {
        return Response::error(400, "only POST requests carry a body").closing();
    }
    match head.method.as_str() {
        "POST" => post(stream, buf, head, shared),
        "DELETE" => match head.one("mcp-session-id") {
            Ok(Some(id)) if lock(&shared.sessions).close(id) => Response::empty(204),
            Ok(Some(_)) => Response::error(404, "no such MCP session"),
            _ => Response::error(400, "DELETE needs the Mcp-Session-Id to end"),
        },
        // This server doesn't stream: GET is allowed to be refused (MCP Streamable HTTP).
        _ => Response::error(405, "POST one JSON-RPC message, or DELETE a session").header("Allow", "POST, DELETE").closing(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn post(stream: &mut TcpStream, buf: &mut Vec<u8>, head: &Head, shared: &Shared) -> Response {
    let is_json = |v: &str| v.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"));
    match head.one("content-type") {
        Ok(Some(kind)) if is_json(kind) => {}
        _ => return Response::error(415, "send the message as Content-Type: application/json").closing(),
    }
    if let Ok(Some(accept)) = head.one("accept") {
        let accepts = accept
            .split(',')
            .map(|t| t.split(';').next().unwrap_or_default().trim().to_ascii_lowercase())
            .any(|t| matches!(t.as_str(), "application/json" | "application/*" | "*/*"));
        if !accepts {
            return Response::error(406, "replies are application/json; include it in Accept").closing();
        }
    }
    if head.headers.iter().any(|(n, _)| n == "transfer-encoding") {
        return Response::error(501, "chunked bodies aren't accepted; send Content-Length").closing();
    }
    let length = match head.one("content-length") {
        Ok(Some(v)) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => match v.parse::<usize>() {
            Ok(n) if n <= MAX_MESSAGE_BYTES => n,
            _ => return Response::error(413, &format!("messages are at most {MAX_MESSAGE_BYTES} bytes")).closing(),
        },
        Ok(None) => return Response::error(411, "send Content-Length").closing(),
        _ => return Response::error(400, "malformed Content-Length").closing(),
    };
    match head.one("expect") {
        Ok(None) => {}
        // The client waits for a go-ahead before sending the body; all checks have passed.
        Ok(Some(expect)) if expect.eq_ignore_ascii_case("100-continue") => {
            if stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").and_then(|()| stream.flush()).is_err() {
                return Response::error(400, "the connection failed").closing();
            }
        }
        _ => return Response::error(417, "only `Expect: 100-continue` is understood").closing(),
    }
    let body = match read_body(stream, buf, length, shared.timeouts.body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let message: Value = match std::str::from_utf8(&body).ok().map(serde_json::from_str::<Value>) {
        Some(Ok(message)) => message,
        Some(Err(_)) => return Response::json(400, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": PARSE_ERROR, "message": "not valid JSON"}})),
        None => return Response::json(400, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": PARSE_ERROR, "message": "the message isn't valid UTF-8"}})),
    };
    let initialize = message.get("method").and_then(Value::as_str) == Some("initialize") && message.get("id").is_some();
    let session = head.one("mcp-session-id").ok().flatten();
    if !initialize {
        match session {
            None => return Response::error(400, "send the Mcp-Session-Id that initialize returned"),
            Some(id) if !lock(&shared.sessions).touch(id) => return Response::error(404, "this MCP session has ended; initialize again"),
            Some(_) => {}
        }
    }
    let reply = match ask(shared, stream, message) {
        Ok(reply) => reply,
        Err(response) => return response,
    };
    let Some(reply) = reply else { return Response::empty(202) };
    let mut response = Response::json(200, &reply);
    if initialize && reply.get("result").is_some() {
        match crate::control::Token::generate() {
            Ok(id) => {
                let id = id.to_hex();
                lock(&shared.sessions).open(id.clone());
                response = response.header("Mcp-Session-Id", id);
            }
            Err(_) => return Response::error(503, "no random source for a session id"),
        }
    }
    response
}

/// The body: what followed the head in `buf`, then the rest from the stream, before `timeout`.
fn read_body(stream: &mut TcpStream, buf: &mut Vec<u8>, length: usize, timeout: Duration) -> Result<Vec<u8>, Response> {
    let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
    let mut body = Vec::with_capacity(length.min(64 * 1024));
    let take = length.min(buf.len());
    body.extend(buf.drain(..take));
    let mut chunk = [0u8; 16 * 1024];
    while body.len() < length {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            return Err(Response::error(408, "the request body took too long").closing());
        }
        let want = length.saturating_sub(body.len()).min(chunk.len());
        match stream.read(chunk.get_mut(..want).unwrap_or_default()) {
            Ok(0) => return Err(Response::error(400, "the body ended before Content-Length").closing()),
            Ok(n) => body.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Response::error(408, "the request body took too long").closing()),
        }
    }
    Ok(body)
}

/// Hands `message` to the serving thread and waits for its reply, watching `stream`: if the
/// client disconnects, or the message isn't started within the queue timeout, it is withdrawn
/// and never runs. Once started it runs to the end, even if nobody is left to read the reply.
fn ask(shared: &Shared, stream: &TcpStream, message: Value) -> Result<Option<Value>, Response> {
    if shared.stopped.load(Ordering::SeqCst) {
        return Err(Response::error(503, "the MCP server is stopping").closing());
    }
    let (reply, answer) = sync_channel(1);
    let now = Instant::now();
    let start_by = now.checked_add(shared.timeouts.queue).unwrap_or(now);
    let finish_by = now.checked_add(shared.timeouts.answer).unwrap_or(now);
    let state = Arc::new(AtomicU8::new(QUEUED));
    if shared.to_server.try_send(Job { message, reply, start_by, state: Arc::clone(&state) }).is_err() {
        return Err(Response::error(503, "the server is still answering earlier messages; try again"));
    }
    loop {
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(reply) => return Ok(reply),
            Err(RecvTimeoutError::Disconnected) => {
                // Dropped unstarted (too late, or stopping) or the server went away.
                return Err(if state.load(Ordering::SeqCst) == ABANDONED {
                    Response::error(503, NOT_STARTED)
                } else {
                    Response::error(503, "the MCP server stopped").closing()
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if peer_closed(stream) {
            settle(&state, ABANDONED);
            return Err(Response::error(400, "the client went away").closing());
        }
        let now = Instant::now();
        if now >= start_by && settle(&state, ABANDONED) {
            return Err(Response::error(503, NOT_STARTED));
        }
        if now >= finish_by {
            return Err(Response::error(503, "the server didn't answer in time; the message started and may still finish").closing());
        }
    }
}

const NOT_STARTED: &str = "the server was busy and didn't start this message; nothing ran, try again";

/// Whether the client has closed its side (or the connection broke), without consuming anything
/// it sent.
fn peer_closed(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let mut probe = [0u8; 1];
    let closed = match stream.peek(&mut probe) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted),
    };
    if stream.set_nonblocking(false).is_err() {
        return true;
    }
    closed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_origins_must_be_this_computer() {
        for ok in ["127.0.0.1:4000", "localhost:4000", "LOCALHOST:4000", "[::1]:4000", "127.1.2.3:4000"] {
            assert!(loopback_authority(ok, Some(4000)), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "127.0.0.1:4001",
            "evil.example:4000",
            "localhost.evil.example:4000",
            "[::1]",
            "[::1]x:4000",
            "[::2]:4000",
            "",
            ":4000",
            "0.0.0.0:4000",
            "127.0.0.1:99999",
            "::1:4000",
            "::1",
        ] {
            assert!(!loopback_authority(bad, Some(4000)), "{bad}");
        }
        for ok in ["http://localhost:3000", "http://127.0.0.1", "https://[::1]:8443"] {
            assert!(loopback_origin(ok), "{ok}");
        }
        for bad in ["null", "http://evil.example", "http://localhost.evil.example", "file://", "http://localhost/x", "ftp://localhost"] {
            assert!(!loopback_origin(bad), "{bad}");
        }
    }

    #[test]
    fn tokens_are_checked_without_echoing_them() {
        assert!(BearerToken::new("short").is_err());
        assert!(BearerToken::new(&"a".repeat(MAX_TOKEN_BYTES + 1)).is_err());
        let secret = format!("{}\"", "a".repeat(40));
        let err = BearerToken::new(&secret).unwrap_err();
        assert!(!err.message.contains("aaaa"));
        let token = BearerToken::new(&"x".repeat(40)).unwrap();
        assert!(token.matches(&"x".repeat(40)));
        assert!(!token.matches(&"x".repeat(39)));
        assert!(!token.matches(&format!("{}y", "x".repeat(39))));
        assert_eq!(format!("{token:?}"), "BearerToken(…)");
        assert_eq!(token.masked(), "…xxxx");
        assert_eq!(BearerToken::generate().unwrap().expose().len(), 64);
    }

    #[test]
    fn heads_reject_smuggling_shapes() {
        assert!(parse_head(b"POST /mcp HTTP/1.1\r\nHost: a").is_ok());
        for bad in [
            &b"POST /mcp HTTP/1.1\r\n folded: x"[..],
            b"POST /mcp HTTP/2\r\nHost: a",
            b"POST  /mcp HTTP/1.1",
            b"post /mcp HTTP/1.1",
            b"POST /mcp HTTP/1.1\r\nBad Name: x",
            b"POST /mcp HTTP/1.1\r\nX: a\x00b",
            b"POST /mcp HTTP/1.1\r\nnocolon",
            b"\xff\xfe",
        ] {
            assert!(parse_head(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let many: String = (0..=MAX_HEADERS).map(|i| format!("\r\nX-{i}: y")).collect();
        assert!(parse_head(format!("POST /mcp HTTP/1.1{many}").as_bytes()).is_err());
    }

    #[test]
    fn sessions_are_bounded_and_least_recently_used_go_first() {
        let mut sessions = Sessions::default();
        for i in 0..MAX_SESSIONS {
            sessions.open(format!("s{i}"));
        }
        assert!(sessions.touch("s0"));
        sessions.open("new".into());
        assert!(sessions.touch("s0"), "recently used stays");
        assert!(!sessions.touch("s1"), "least recently used went");
        assert!(sessions.close("new"));
        assert!(!sessions.close("new"));
    }
}
