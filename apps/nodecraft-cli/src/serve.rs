//! `serve`: a project's techniques as a local HTTP API (#117).
//!
//! The server loads the project once and keeps it as an immutable snapshot; every run gets a
//! fresh in-memory session made from it, so runs can't see each other and the project file is
//! never written. A run places the technique on a scratch canvas, fills its form, is quoted and
//! held to the server's cost limit, then runs exactly as `nodecraft-cli run` runs. Runs execute
//! one at a time on a worker thread, in the order they were queued.
//!
//! Every request needs the bearer token from the token file (owner-only, beside the app's own
//! control token); the server listens on 127.0.0.1 unless told otherwise. Requests are read by
//! the bounded parser in [`http`]: sizes, header counts, connections and waits are all capped.
mod api;
mod http;
mod webhook;
mod worker;

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use nodecraft_automation::control::{Token, TokenFile};
use nodecraft_types::{Error, ErrorKind};

pub use webhook::{Settings as Webhooks, key as webhook_key, parse_allow as parse_webhook_allow};
pub use worker::Loaded;

/// The keychain account the webhook signing secret is kept under.
pub const WEBHOOK_ACCOUNT: &str = webhook::ACCOUNT;

/// Connections served at once; more are answered 503 and closed.
const MAX_CONNECTIONS: usize = 8;
/// Runs waiting for the worker; more are answered 429.
const MAX_QUEUED: usize = 32;
/// Each wait for the client's next bytes, and for it to take ours; no longer than the head's
/// deadline, so that deadline holds.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// What a served run may cost, unless a request asks for less.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// The most one run may cost, in micros per currency. A currency not listed may not be spent.
    pub max_cost: BTreeMap<String, u64>,
    /// Whether a run whose price isn't fully known may start.
    pub allow_unpriced: bool,
}

/// How the server was asked to run.
pub struct Config {
    pub listen: SocketAddr,
    /// Where the token file goes; `None` for `serve.json` in the app's private control folder.
    pub token_file: Option<PathBuf>,
    pub policy: Policy,
    /// Where run-completion webhooks may go; `None` sends none.
    pub webhooks: Option<Webhooks>,
}

/// A running server: its address and token. Dropping it doesn't stop it; [`Self::wait`] blocks
/// for as long as it serves.
pub struct Server {
    pub addr: SocketAddr,
    /// Where the token was written.
    pub token_file: PathBuf,
    #[cfg_attr(not(test), expect(dead_code, reason = "clients read it from the token file; tests use it directly"))]
    pub token: Token,
    accept: std::thread::JoinHandle<()>,
}

impl Server {
    pub fn wait(self) {
        let _ = self.accept.join();
    }
}

/// `--max-cost USD=1.50`: a currency and a decimal amount with at most six places.
pub fn parse_max_cost(text: &str) -> Result<(String, u64), String> {
    let refused = || format!("`--max-cost` takes CURRENCY=AMOUNT, like USD=1.50, not `{text}`");
    let (currency, amount) = text.split_once('=').ok_or_else(refused)?;
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(refused());
    }
    Ok((currency.to_owned(), micros(amount).ok_or_else(refused)?))
}

/// A non-negative decimal amount in millionths, or `None` if it isn't one or doesn't fit.
pub fn micros(amount: &str) -> Option<u64> {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || whole.len() > 12 || fraction.len() > 6 || !digits(whole) || !digits(fraction) || (amount.contains('.') && fraction.is_empty()) {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let fraction: u64 = if fraction.is_empty() { 0 } else { format!("{fraction:0<6}").parse().ok()? };
    whole.checked_mul(1_000_000)?.checked_add(fraction)
}

/// Loads the project on the worker thread, then listens. Returns once the server is taking
/// requests; the token file is written by then.
pub fn start(config: Config, load: impl FnOnce() -> Result<Loaded, Error> + Send + 'static) -> Result<Server, Error> {
    let token = Token::generate()?;
    let (jobs, queue) = mpsc::sync_channel(MAX_QUEUED);
    let runs = Arc::new(worker::Runs::default());
    let webhooks = config.webhooks.map(Arc::new);
    let hooks = webhooks.as_ref().map(|settings| webhook::spawn(Arc::clone(settings), Arc::clone(&runs))).transpose()?;
    let catalog = worker::spawn(load, queue, Arc::clone(&runs), hooks)?;
    let listener = TcpListener::bind(config.listen).map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't listen on {}: {e}", config.listen)))?;
    let addr = listener.local_addr().map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't read the listening address: {e}")))?;
    let path = match config.token_file {
        Some(path) => path,
        None => default_token_file()?,
    };
    let file = TokenFile { port: addr.port(), token: token.to_hex(), pid: std::process::id() };
    file.write(&path)?;
    let state = Arc::new(api::State::new(token.clone(), catalog, runs, jobs, config.policy, webhooks));
    let accept = std::thread::Builder::new()
        .name("nodecraft-serve".into())
        .spawn(move || accept_loop(&listener, &state))
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the server thread: {e}")))?;
    Ok(Server { addr, token_file: path, token, accept })
}

/// `serve.json` in the app's private control folder.
fn default_token_file() -> Result<PathBuf, Error> {
    let settings = nodecraft_engine::settings::settings_dir()
        .ok_or_else(|| Error::new(ErrorKind::Unsupported, "no settings folder for the token file; pass `--token-file <path>`"))?;
    Ok(nodecraft_automation::control::private_dir(&settings)?.join("serve.json"))
}

/// Counts a connection while it is served.
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(listener: &TcpListener, state: &Arc<api::State>) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            // Out of file handles, say: wait a little rather than spin.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let busy = http::Response::json(503, &serde_json::json!({"error": {"kind": "busy", "message": "too many connections; try again"}}));
            let _ = http::write(&mut stream, busy.header("Retry-After", "1".into()));
            continue;
        }
        let slot = Slot(Arc::clone(&open));
        let state = Arc::clone(state);
        let spawned = std::thread::Builder::new().name("nodecraft-serve-conn".into()).spawn(move || {
            let _slot = slot;
            connection(stream, &state);
        });
        // A thread that can't start drops its slot with the closure.
        drop(spawned);
    }
}

fn connection(stream: TcpStream, state: &api::State) {
    let Ok(mut out) = stream.try_clone() else { return };
    let mut on_continue = || {
        let _ = http::write_continue(&mut out);
    };
    // The token is checked once the headers are in, before any body is read or invited.
    let admit = |request: &http::Request| if state.authorized(request) { Ok(()) } else { Err(http::Refusal::UNAUTHORIZED) };
    let response = match http::read(&stream, http::Limits::DEFAULT, admit, &mut on_continue) {
        Ok(request) => state.answer(&request),
        Err(refusal) => http::refusal(&refusal),
    };
    let _ = http::write(&mut out, response);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Opens `path` for serving: decoded once; every run starts from this snapshot.
pub fn load(path: &Path) -> Result<Loaded, Error> {
    let snapshot = crate::project::snapshot(path)?;
    let maker = || -> Result<Box<crate::quote::Maker<'static>>, Error> {
        Ok(Box::new(crate::providers::keychain_maker(nodecraft_engine::settings::peek().providers)?))
    };
    Ok(Loaded { snapshot, maker: Box::new(maker) })
}

#[cfg(test)]
mod tests;
