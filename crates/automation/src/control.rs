//! The control channel (#115): drive the running app from another process over local JSON.
//!
//! `nodecraft --control` listens on 127.0.0.1 (a random port) and writes a token file (see
//! [`TokenFile`]) that only the user can read. A client connects, sends one request per line and
//! gets one reply per line:
//!
//! ```text
//! → {"id": 1, "method": "hello", "params": {"token": "…"}}
//! ← {"id": 1, "result": {"app": "nodecraft", "version": "…", "protocol": 1}}
//! → {"id": 2, "method": "command.run", "params": {"id": "node.add", "params": {…}}}
//! ← {"id": 2, "result": {"result": {…}, "revision": 3}}
//! ```
//!
//! Every connection must start with `hello` and the token; anything else closes it. Methods:
//! `commands.list`, `command.run`, `state.get`, `canvas.inspect` (the same bounded pages as the
//! MCP server) and `screenshot` (a PNG of the window). Commands go through the session the window
//! shows, so the user sees each change, and the same automation classification as MCP applies
//! (people-only commands are refused).
//!
//! Connection threads only parse and authenticate; every request is answered on the UI thread,
//! which owns the session ([`Call`], [`answer`]). A screenshot is answered a frame later.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nodecraft_engine::Session;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Map, Value, json};

use crate::mcp::{self, Line, MAX_MESSAGE_BYTES};

/// The protocol version `hello` reports.
pub const PROTOCOL: u32 = 1;
/// Clients connected at once; more are turned away.
pub const MAX_CONNECTIONS: usize = 4;
/// Requests waiting for the UI thread, across all connections.
const QUEUE: usize = 64;
/// How long a request waits for the UI thread (a screenshot needs a frame) before it fails.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a new connection may take to say `hello`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an authenticated connection may stay silent, and a reply may take to send.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest screenshot sent, in bytes of PNG.
pub const MAX_SCREENSHOT_BYTES: usize = 64 * 1024 * 1024;

/// The shared secret a client must present. 32 random bytes, shown as hex.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 32]);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(…)")
    }
}

impl Token {
    /// A token from the operating system's random source.
    pub fn generate() -> Result<Self, Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| Error::new(ErrorKind::Unsupported, format!("no random source for the control token: {e}")))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Whether `presented` is this token, compared in constant time.
    pub fn matches(&self, presented: &str) -> bool {
        let expected = self.to_hex();
        if presented.len() != expected.len() {
            return false;
        }
        presented.bytes().zip(expected.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
    }
}

/// What the app writes for clients: where to connect and the token. Written with owner-only
/// permissions; delete it when the app exits. A client should check that `pid` is alive: a file
/// left by a killed app names a port nobody listens on.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenFile {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

impl std::fmt::Debug for TokenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenFile").field("port", &self.port).field("token", &"…").field("pid", &self.pid).finish()
    }
}

fn io_error(path: &std::path::Path, e: std::io::Error) -> Error {
    Error::new(ErrorKind::Unsupported, format!("can't write {}: {e}", path.display()))
}

/// The private folder for the token file inside `settings`: `control/`, created owner-only (and
/// made so if it exists, which only its owner can do).
pub fn private_dir(settings: &std::path::Path) -> Result<std::path::PathBuf, Error> {
    let dir = settings.join("control");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir).map_err(|e| io_error(&dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| io_error(&dir, e))?;
    }
    Ok(dir)
}

impl TokenFile {
    /// Writes the file at `path`, readable and writable by the owner only. It is written to a new
    /// sibling file and renamed into place, so an existing file or a symlink at `path` is replaced,
    /// never written through, and a reader never sees a half-written file. On Unix, a folder that
    /// other users can write is refused.
    pub fn write(&self, path: &std::path::Path) -> Result<(), Error> {
        let text = serde_json::to_string(self).map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?;
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir).map_err(|e| io_error(dir, e))?.permissions().mode();
            if mode & 0o077 != 0 {
                return Err(Error::new(ErrorKind::Unsupported, format!("{} is open to other users; the control token won't be kept there", dir.display())));
            }
        }
        let temp = dir.join(format!(".control-{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&temp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(&temp).and_then(|mut file| file.write_all(text.as_bytes()).and_then(|()| file.sync_all()));
        if let Err(e) = written.and_then(|()| std::fs::rename(&temp, path)) {
            let _ = std::fs::remove_file(&temp);
            return Err(io_error(path, e));
        }
        Ok(())
    }

    /// Removes the file at `path` if it is still this one (another instance may have replaced it).
    pub fn remove_if_ours(&self, path: &std::path::Path) {
        let ours = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<TokenFile>(&t).ok()).is_some_and(|f| &f == self);
        if ours {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A request the UI thread answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    ListCommands(Map<String, Value>),
    RunCommand(Map<String, Value>),
    State,
    Inspect(Map<String, Value>),
    Screenshot,
}

/// A request waiting for the UI thread, with the way back to its connection.
pub struct Call {
    pub request: Request,
    reply: SyncSender<Result<Value, Error>>,
    deadline: Instant,
}

impl Call {
    /// The client stopped waiting (and was told it timed out): don't run it, or a retry would run
    /// a command twice.
    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// Sends the answer back (the client may have gone; that's fine).
    pub fn reply(self, answer: Result<Value, Error>) {
        let _ = self.reply.send(answer);
    }
}

/// Answers every request except [`Request::Screenshot`] (which needs the window: the host takes
/// one and calls [`screenshot_answer`]). `None` for a screenshot.
pub fn answer(session: &mut Session, request: &Request) -> Option<Result<Value, Error>> {
    Some(match request {
        Request::ListCommands(args) => mcp::command_list(session, args),
        Request::RunCommand(args) => mcp::command_run(session, args.clone()),
        Request::Inspect(args) => mcp::canvas_inspect(session, args),
        Request::State => mcp::canvas_inspect(session, &Map::new()).map(|mut overview| {
            if let Some(o) = overview.as_object_mut() {
                o.insert("can_undo".into(), json!(session.can_undo()));
                o.insert("can_redo".into(), json!(session.can_redo()));
            }
            overview
        }),
        Request::Screenshot => return None,
    })
}

/// A screenshot reply from the window's RGBA pixels.
pub fn screenshot_answer(width: u32, height: u32, rgba: &[u8]) -> Result<Value, Error> {
    let png =
        nodecraft_media::encode_png(width, height, rgba, &nodecraft_media::Limits::default()).map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?;
    if png.len() > MAX_SCREENSHOT_BYTES {
        return Err(Error::new(ErrorKind::Unsupported, "the screenshot is too large to send"));
    }
    Ok(json!({"mime": "image/png", "width": width, "height": height, "data": base64(&png)}))
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() { char::from(T.get(((n >> shift) & 63) as usize).copied().unwrap_or(b'A')) } else { '=' });
        }
    }
    out
}

/// What a connection does with one line.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Send this reply.
    Reply(Value),
    /// Ask the UI thread, then reply under this id.
    Ask(Value, Request),
    /// Send this reply and close the connection.
    Close(Value),
}

fn error_reply(id: Value, kind: &str, message: &str) -> Value {
    json!({"id": id, "error": {"kind": kind, "message": message}})
}

fn error_value(id: Value, e: &Error) -> Value {
    let kind = serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| "internal".into());
    json!({"id": id, "error": {"kind": kind, "message": e.message}})
}

/// One connection's protocol state: unauthenticated until a correct `hello`.
pub struct Connection {
    token: Token,
    authed: bool,
}

impl Connection {
    pub fn new(token: Token) -> Self {
        Self { token, authed: false }
    }

    pub fn is_authed(&self) -> bool {
        self.authed
    }

    /// Parses one request line.
    pub fn step(&mut self, line: &str) -> Step {
        let Ok(Value::Object(mut message)) = serde_json::from_str::<Value>(line) else {
            let reply = error_reply(Value::Null, "invalid_input", "expected one JSON object per line");
            return if self.authed { Step::Reply(reply) } else { Step::Close(reply) };
        };
        let id = match message.remove("id") {
            Some(id @ (Value::String(_) | Value::Number(_))) => id,
            _ => Value::Null,
        };
        let method = message.remove("method").and_then(|m| m.as_str().map(str::to_owned)).unwrap_or_default();
        let params = match message.remove("params") {
            Some(Value::Object(p)) => p,
            None | Some(Value::Null) => Map::new(),
            Some(_) if !self.authed => return Step::Close(error_reply(id, "authentication", "start with hello and the token from the token file")),
            Some(_) => return Step::Reply(error_reply(id, "invalid_input", "params must be an object")),
        };
        if !self.authed {
            let presented = params.get("token").and_then(Value::as_str).unwrap_or_default();
            if method == "hello" && self.token.matches(presented) {
                self.authed = true;
                return Step::Reply(json!({"id": id, "result": {"app": "nodecraft", "version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL}}));
            }
            return Step::Close(error_reply(id, "authentication", "start with hello and the token from the token file"));
        }
        let request = match method.as_str() {
            "hello" => return Step::Reply(json!({"id": id, "result": {"app": "nodecraft", "version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL}})),
            "commands.list" => Request::ListCommands(params),
            "command.run" => Request::RunCommand(params),
            "state.get" => Request::State,
            "canvas.inspect" => Request::Inspect(params),
            "screenshot" => Request::Screenshot,
            other => return Step::Reply(error_reply(id, "not_found", &format!("unknown method {}", other.chars().take(64).collect::<String>()))),
        };
        Step::Ask(id, request)
    }
}

/// The listening side, held by the app.
pub struct Listener {
    pub addr: SocketAddr,
    /// Requests for the UI thread; drain them every frame.
    pub calls: Receiver<Call>,
}

/// Timeouts a listener applies (tests shorten them).
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub handshake: Duration,
    pub idle: Duration,
    pub write: Duration,
    pub answer: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { handshake: HANDSHAKE_TIMEOUT, idle: IDLE_TIMEOUT, write: WRITE_TIMEOUT, answer: ANSWER_TIMEOUT }
    }
}

/// Listens on 127.0.0.1 (a random port). `wake` is called when a request arrives, so the app can
/// repaint and answer it.
pub fn listen(token: Token, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Listener, Error> {
    listen_with(token, wake, Timeouts::default())
}

pub fn listen_with(token: Token, wake: Arc<dyn Fn() + Send + Sync>, timeouts: Timeouts) -> Result<Listener, Error> {
    let socket = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't listen for control: {e}")))?;
    let addr = socket.local_addr().map_err(|e| Error::new(ErrorKind::Unsupported, e.to_string()))?;
    let (to_ui, calls) = sync_channel::<Call>(QUEUE);
    let open = Arc::new(AtomicUsize::new(0));
    let refusing = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("nodecraft-control".into())
        .spawn(move || {
            for stream in socket.incoming() {
                let Ok(stream) = stream else { continue };
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    // Off the accept thread, so silent refused clients can't delay real ones; a few at
                    // once at most, beyond that the refusal is just closed.
                    if refusing.fetch_add(1, Ordering::SeqCst) < MAX_REFUSALS {
                        let held = Arc::clone(&refusing);
                        let spawned = std::thread::Builder::new().name("nodecraft-control-busy".into()).spawn(move || {
                            refuse_busy(stream);
                            held.fetch_sub(1, Ordering::SeqCst);
                        });
                        if spawned.is_err() {
                            refusing.fetch_sub(1, Ordering::SeqCst);
                        }
                    } else {
                        refusing.fetch_sub(1, Ordering::SeqCst);
                    }
                    continue;
                }
                let (token, to_ui, wake, slot) = (token.clone(), to_ui.clone(), Arc::clone(&wake), Arc::clone(&open));
                let spawned = std::thread::Builder::new().name("nodecraft-control-conn".into()).spawn(move || {
                    serve_connection(stream, token, &to_ui, wake.as_ref(), timeouts);
                    slot.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    // The thread never ran, so give its slot back.
                    open.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the control listener: {e}")))?;
    Ok(Listener { addr, calls })
}

/// Longest `hello` line accepted.
const MAX_HELLO_BYTES: usize = 4096;

/// Reads the `hello` line before `deadline`, at most [`MAX_HELLO_BYTES`]: the read timeout is set
/// to the time left before every read.
fn read_hello(reader: &mut BufReader<TcpStream>, buf: &mut Vec<u8>, deadline: Instant) -> std::io::Result<Line> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake took too long"));
        }
        reader.get_ref().set_read_timeout(Some(left))?;
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() { Line::Eof } else { Line::Read });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        if buf.len().saturating_add(take) > MAX_HELLO_BYTES {
            return Ok(Line::TooLong);
        }
        buf.extend_from_slice(chunk.get(..take).unwrap_or_default());
        reader.consume(if done { take.saturating_add(1) } else { take });
        if done {
            return Ok(Line::Read);
        }
    }
}

/// Refusals answered at once (each waits up to 200 ms for the client's hello).
const MAX_REFUSALS: usize = 4;

/// Turns a connection away as busy. The client's `hello` is drained briefly after the reply, so
/// closing doesn't reset the connection and lose the reply (a reset is what an unread `hello`
/// causes on macOS and Windows). Bounded: a short timeout and one small read.
fn refuse_busy(mut stream: TcpStream) {
    let _ = writeln!(stream, "{}", error_reply(Value::Null, "busy", "too many control connections"));
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut sink = [0u8; 4096];
    let _ = std::io::Read::read(&mut stream, &mut sink);
}

fn serve_connection(stream: TcpStream, token: Token, to_ui: &SyncSender<Call>, wake: &(dyn Fn() + Send + Sync), timeouts: Timeouts) {
    // A client must say hello quickly; a silent or slow one can't hold a slot.
    if stream.set_read_timeout(Some(timeouts.handshake)).is_err() || stream.set_write_timeout(Some(timeouts.write)).is_err() {
        return;
    }
    let Ok(reader) = stream.try_clone() else { return };
    let mut reader = BufReader::new(reader);
    let writer = Mutex::new(stream);
    let send = |value: Value| -> bool {
        let mut w = writer.lock().unwrap_or_else(PoisonError::into_inner);
        writeln!(w, "{value}").and_then(|()| w.flush()).is_ok()
    };
    let mut connection = Connection::new(token);
    let mut line = Vec::new();
    // The whole handshake has one deadline: a client trickling bytes can't stretch it.
    let handshake_ends = Instant::now().checked_add(timeouts.handshake).unwrap_or_else(Instant::now);
    loop {
        line.clear();
        let authed = connection.is_authed();
        // Before hello, anything but a well-formed hello ends the connection.
        let fail = |message: &str| -> bool { send(error_reply(Value::Null, if authed { "invalid_input" } else { "authentication" }, message)) && authed };
        let read = if authed { mcp::read_line(&mut reader as &mut dyn BufRead, &mut line) } else { read_hello(&mut reader, &mut line, handshake_ends) };
        let text = match read {
            Ok(Line::Eof) | Err(_) => return,
            Ok(Line::TooLong) => {
                let limit = if authed { MAX_MESSAGE_BYTES } else { MAX_HELLO_BYTES };
                if !fail(&format!("message is larger than {limit} bytes")) {
                    return;
                }
                continue;
            }
            Ok(Line::Read) => match std::str::from_utf8(&line) {
                Ok(t) if t.trim().is_empty() && authed => continue,
                Ok(t) if t.trim().is_empty() => {
                    fail("start with hello and the token from the token file");
                    return;
                }
                Ok(t) => t.trim().to_owned(),
                Err(_) => {
                    if !fail("the message isn't valid UTF-8") {
                        return;
                    }
                    continue;
                }
            },
        };
        let was_authed = connection.is_authed();
        let step = connection.step(&text);
        if !was_authed && connection.is_authed() && reader.get_ref().set_read_timeout(Some(timeouts.idle)).is_err() {
            return;
        }
        let reply = match step {
            Step::Reply(v) => v,
            Step::Close(v) => {
                send(v);
                return;
            }
            Step::Ask(id, request) => {
                let (reply, answer) = sync_channel(1);
                match to_ui.try_send(Call { request, reply, deadline: Instant::now().checked_add(timeouts.answer).unwrap_or_else(Instant::now) }) {
                    Ok(()) => {
                        wake();
                        match answer.recv_timeout(timeouts.answer) {
                            Ok(Ok(result)) => json!({"id": id, "result": result}),
                            Ok(Err(e)) => error_value(id, &e),
                            Err(RecvTimeoutError::Timeout) => error_reply(id, "timeout", "the app didn't answer in time"),
                            Err(RecvTimeoutError::Disconnected) => error_reply(id, "internal", "the app dropped the request"),
                        }
                    }
                    Err(_) => error_reply(id, "busy", "the app is still answering earlier requests; try again"),
                }
            }
        };
        if !send(reply) {
            return;
        }
    }
}

#[cfg(test)]
mod tests;

/// A control-channel client: what `nodecraft-cli mcp --bridge` uses to reach the app.
///
/// It reconnects (re-reading the token file, if it came from one) once the app may have closed
/// the connection as idle, and after a lost connection on the next call. A call that failed is
/// never resent, so nothing runs twice.
pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next: u64,
    broken: bool,
    /// How to connect again.
    origin: Origin,
    last_used: Instant,
    reconnect_after: Duration,
}

#[derive(Clone)]
enum Origin {
    File(std::path::PathBuf),
    Addr(SocketAddr, String),
}

/// Reconnect before a call once the app may have closed the connection as idle.
pub const RECONNECT_AFTER: Duration = Duration::from_secs(540);
/// Handshakes tried while the app is still freeing the old connection's slot.
pub const RECONNECT_ATTEMPTS: u32 = 6;
/// How long the app may take to answer `hello`.
const HELLO_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest reply the client reads: a screenshot in base64 plus room.
const MAX_REPLY_BYTES: usize = MAX_SCREENSHOT_BYTES / 3 * 4 + 1024 * 1024;

fn network(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Network, message)
}

impl Client {
    /// Connects to the app named by the token file at `path` and says hello.
    pub fn from_token_file(path: &std::path::Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(ErrorKind::NotFound, format!("no control token at {} ({e}); start the app with `nodecraft --control`", path.display())))?;
        let file: TokenFile =
            serde_json::from_str(&text).map_err(|_| Error::new(ErrorKind::InvalidInput, format!("{} isn't a control token file", path.display())))?;
        let mut client = Self::connect(SocketAddr::from(([127, 0, 0, 1], file.port)), &file.token)?;
        client.origin = Origin::File(path.to_path_buf());
        Ok(client)
    }

    /// Connects to `addr` and says hello with `token`.
    pub fn connect(addr: SocketAddr, token: &str) -> Result<Self, Error> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .map_err(|e| network(format!("can't reach the app on {addr} ({e}); is it still running with --control?")))?;
        // The app answers every request within its answer timeout, so this only fires if it
        // hangs. A long reply is read in pieces, each with this timeout.
        let wait = ANSWER_TIMEOUT.saturating_add(Duration::from_secs(5));
        // The handshake gets a short reply timeout of its own; requests get the long one.
        stream.set_read_timeout(Some(HELLO_REPLY_TIMEOUT)).and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT))).map_err(|e| network(e.to_string()))?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| network(e.to_string()))?);
        let mut client = Self {
            reader,
            writer: stream,
            next: 1,
            broken: false,
            origin: Origin::Addr(addr, token.to_owned()),
            last_used: Instant::now(),
            reconnect_after: RECONNECT_AFTER,
        };
        let mut hello = Map::new();
        hello.insert("token".into(), json!(token));
        let reply = client.exchange("hello", hello)?;
        if reply.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL)) {
            return Err(Error::new(ErrorKind::Unsupported, "the app speaks a different control protocol; use a matching nodecraft-cli"));
        }
        client.writer.set_read_timeout(Some(wait)).map_err(|e| network(e.to_string()))?;
        Ok(client)
    }

    /// How long the connection may sit unused before the next call replaces it (default
    /// [`RECONNECT_AFTER`], under the app's idle timeout). Hosts with a shorter idle timeout, and
    /// tests, lower it; `Duration::ZERO` reconnects before every call.
    pub fn set_reconnect_after(&mut self, after: Duration) {
        self.reconnect_after = after;
    }

    /// Replaces the connection. The old one is closed first, so the replacement doesn't need a
    /// spare slot when the app is at its connection limit. The app frees the slot a moment later,
    /// so a handshake refused as busy, or cut off before its reply (a refused connection can be
    /// reset), is retried up to [`RECONNECT_ATTEMPTS`] times with backoff (775 ms of waits in
    /// all). Only `hello` has been sent on those connections, never the caller's request. Worst
    /// case, each attempt waits out the 5 s connect and 5 s handshake timeouts. An app that has
    /// exited (connection refused) is retried too, so such a call takes about 0.8 s to fail.
    fn reconnect(&mut self) -> Result<(), Error> {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        // Until a replacement works, the next call tries again.
        self.broken = true;
        let mut wait = Duration::from_millis(25);
        let mut attempt = 0;
        let fresh = loop {
            attempt += 1;
            let tried = match self.origin.clone() {
                Origin::File(path) => Self::from_token_file(&path),
                Origin::Addr(addr, token) => Self::connect(addr, &token),
            };
            match tried {
                Err(e) if matches!(e.kind, ErrorKind::Busy | ErrorKind::Network) && attempt < RECONNECT_ATTEMPTS => {
                    std::thread::sleep(wait);
                    wait = wait.saturating_mul(2);
                }
                other => break other?,
            }
        };
        let after = self.reconnect_after;
        *self = fresh;
        self.reconnect_after = after;
        Ok(())
    }

    /// Sends one request and returns its result, or the app's error. A connection the app may
    /// have closed as idle, or one lost on an earlier call, is replaced first.
    pub fn call(&mut self, method: &str, params: Map<String, Value>) -> Result<Value, Error> {
        if self.broken || self.last_used.elapsed() >= self.reconnect_after {
            self.reconnect().map_err(|e| Error::new(e.kind, format!("lost the connection to the app and couldn't reconnect: {}", e.message)))?;
        }
        self.last_used = Instant::now();
        self.exchange(method, params)
    }

    /// One request and its reply on the current connection.
    fn exchange(&mut self, method: &str, params: Map<String, Value>) -> Result<Value, Error> {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        let message = json!({"id": id, "method": method, "params": params});
        let sent = writeln!(self.writer, "{message}").and_then(|()| self.writer.flush());
        if let Err(e) = sent {
            self.broken = true;
            return Err(network(format!("the app closed the control channel ({e}); the next call reconnects")));
        }
        let mut line = Vec::new();
        let read = mcp::read_line_capped(&mut self.reader, &mut line, MAX_REPLY_BYTES);
        let reply = match read {
            Ok(Line::Read) => serde_json::from_slice::<Value>(&line).map_err(|_| network("the app sent a malformed reply")),
            Ok(Line::TooLong) => Err(network("the app's reply was too large")),
            Ok(Line::Eof) => Err(network("the app closed the control channel; the next call reconnects")),
            Err(e) => Err(network(format!("no reply from the app ({e})"))),
        };
        // A screenshot reply is large: don't keep the raw line alongside the parsed one.
        drop(line);
        let mut reply = match reply {
            Ok(r) => r,
            Err(e) => {
                // A lost or out-of-step reply leaves the stream unusable.
                self.broken = true;
                return Err(e);
            }
        };
        if reply.get("id").and_then(Value::as_u64) != Some(id) && reply.get("id") != Some(&Value::Null) {
            self.broken = true;
            return Err(network("the app's reply doesn't match the request"));
        }
        if let Some(error) = reply.get("error") {
            let kind = error.get("kind").cloned().and_then(|k| serde_json::from_value::<ErrorKind>(k).ok()).unwrap_or(ErrorKind::Internal);
            let message = error.get("message").and_then(Value::as_str).unwrap_or("the app refused the request");
            return Err(Error::new(kind, message));
        }
        match reply.as_object_mut().and_then(|o| o.remove("result")) {
            Some(result) => Ok(result),
            None => {
                self.broken = true;
                Err(network("the app's reply has neither a result nor an error"))
            }
        }
    }
}
