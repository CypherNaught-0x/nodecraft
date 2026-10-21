//! Signed run-completion webhooks (#632).
//!
//! A run request may name a callback URL; it must match one of the server's `--webhook-allow`
//! entries. When the run ends, a delivery thread POSTs the run, as `GET /runs/{id}` shows it, to
//! that URL, signed with HMAC-SHA256 the way Standard Webhooks signs (`webhook-id`,
//! `webhook-timestamp`, `webhook-signature: v1,<base64>` over `id.timestamp.body`). The secret is
//! read from the keychain for every delivery, so it can be rotated without a restart.
//!
//! Delivery is best effort and bounded: a few attempts with growing waits, each with a timeout,
//! no redirects, and addresses checked (and pinned) before each attempt so a name can't be pointed
//! at a private network. A failed delivery never fails the run; `GET /runs/{id}` stays the truth.
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use nodecraft_engine::secrets::{Account, SecretStore};
use nodecraft_types::{Error, ErrorKind};
use sha2::Sha256;
use url::Url;

use super::worker::{HookStatus, Runs};

/// The keychain account the signing secret lives under.
pub const ACCOUNT: &str = "serve-webhook";
/// The payload's version; bumped when its shape changes incompatibly.
pub const PAYLOAD_VERSION: u64 = 1;
/// Shortest secret taken, so signatures can't be guessed.
pub const MIN_SECRET: usize = 32;
/// Longest callback URL taken.
const MAX_URL: usize = 2048;
/// Deliveries waiting at most; a run ending beyond this records a failed delivery.
const MAX_PENDING: usize = 256;
/// The waits between attempts; one more attempt than waits.
const SCHEDULE: [Duration; 5] =
    [Duration::from_secs(5), Duration::from_secs(30), Duration::from_secs(120), Duration::from_secs(600), Duration::from_secs(1800)];
/// Each attempt's limit, from connecting to the receiver's answer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where webhooks may go and how they are signed, set when the server starts.
pub struct Settings {
    /// Allowed destinations: a scheme, host and port, and a path prefix.
    pub allow: Vec<Url>,
    /// Whether loopback, private and link-local addresses may be called.
    pub allow_private: bool,
    pub store: Arc<dyn SecretStore>,
    /// The waits between attempts.
    pub schedule: Vec<Duration>,
    pub timeout: Duration,
}

impl Settings {
    pub fn new(allow: Vec<Url>, allow_private: bool, store: Arc<dyn SecretStore>) -> Self {
        Self { allow, allow_private, store, schedule: SCHEDULE.to_vec(), timeout: TIMEOUT }
    }

    /// The callback URL a request names, if an allow entry covers it.
    pub fn allows(&self, text: &str) -> Result<Url, String> {
        if text.len() > MAX_URL {
            return Err(format!("the webhook URL is longer than {MAX_URL} characters"));
        }
        let url = Url::parse(text).map_err(|_| "the webhook is not a URL".to_owned())?;
        if !["http", "https"].contains(&url.scheme()) || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err("the webhook must be an http(s) URL without credentials or a fragment".into());
        }
        if self.allow.iter().any(|entry| covers(entry, &url)) {
            Ok(url)
        } else {
            Err("the webhook URL is not allowed by the server's --webhook-allow entries".into())
        }
    }
}

/// `--webhook-allow https://hooks.example.com/nodecraft`: an http(s) origin and a path prefix.
pub fn parse_allow(text: &str) -> Result<Url, String> {
    let refused = || format!("`--webhook-allow` takes an http(s) URL like https://hooks.example.com/path, not `{text}`");
    let url = Url::parse(text).map_err(|_| refused())?;
    let plain = url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    if !["http", "https"].contains(&url.scheme()) || url.host_str().is_none() || !plain {
        return Err(refused());
    }
    Ok(url)
}

/// Whether `entry` allows `url`: the same scheme, host and port, and a path under the entry's.
fn covers(entry: &Url, url: &Url) -> bool {
    let same = entry.scheme() == url.scheme() && entry.host_str() == url.host_str() && entry.port_or_known_default() == url.port_or_known_default();
    let prefix = entry.path();
    let path = url.path();
    same && (prefix == "/" || path == prefix || path.strip_prefix(prefix).is_some_and(|rest| prefix.ends_with('/') || rest.starts_with('/')))
}

/// One run's notice, waiting to go out.
pub struct Delivery {
    pub run: String,
    pub url: Url,
    /// The event's id: the same on every attempt, so a receiver can drop repeats.
    pub event: String,
    pub body: Vec<u8>,
}

/// The payload for a finished run, `described` as `GET /runs/{id}` shows it.
pub fn payload(event: &str, described: serde_json::Value) -> Vec<u8> {
    let payload = serde_json::json!({"type": "run.completed", "version": PAYLOAD_VERSION, "event": event, "run": described});
    serde_json::to_vec(&payload).unwrap_or_default()
}

/// Starts the delivery thread. Deliveries are offered with [`SyncSender::try_send`]; a full queue
/// means the delivery is dropped (and recorded as failed by the caller).
pub fn spawn(settings: Arc<Settings>, runs: Arc<Runs>) -> Result<SyncSender<Delivery>, Error> {
    let (sender, queue) = sync_channel(MAX_PENDING);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the webhook runtime: {e}")))?;
    std::thread::Builder::new()
        .name("nodecraft-serve-webhooks".into())
        .spawn(move || deliver(&settings, &runtime, &queue, &runs))
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the webhook thread: {e}")))?;
    Ok(sender)
}

/// Whether a delivery couldn't be queued; the run records it.
pub fn offer(sender: &SyncSender<Delivery>, delivery: Delivery) -> Result<(), &'static str> {
    match sender.try_send(delivery) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err("too many webhook deliveries are waiting"),
        Err(TrySendError::Disconnected(_)) => Err("the webhook thread has stopped"),
    }
}

/// A delivery and when its next attempt is due.
struct Waiting {
    due: Instant,
    attempts: usize,
    delivery: Delivery,
}

fn deliver(settings: &Settings, runtime: &tokio::runtime::Runtime, queue: &Receiver<Delivery>, runs: &Runs) {
    let mut waiting: Vec<Waiting> = Vec::new();
    loop {
        // Waits for a new delivery until the next attempt is due. A closed queue means the server
        // is gone, and nothing will read the runs any more.
        let received = match waiting.iter().map(|w| w.due).min() {
            Some(due) => match queue.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(delivery) => Some(delivery),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match queue.recv() {
                Ok(delivery) => Some(delivery),
                Err(_) => return,
            },
        };
        if let Some(delivery) = received {
            if waiting.len() >= MAX_PENDING {
                runs.lock().hook(&delivery.run, HookStatus::Failed, 0, Some("too many webhook deliveries are waiting".into()));
            } else {
                waiting.push(Waiting { due: Instant::now(), attempts: 0, delivery });
            }
        }
        while let Some(position) = waiting.iter().position(|w| w.due <= Instant::now()) {
            let mut due = waiting.swap_remove(position);
            due.attempts = due.attempts.saturating_add(1);
            let outcome = attempt(settings, runtime, &due.delivery);
            let retry = settings.schedule.get(due.attempts.saturating_sub(1)).copied();
            let status = match (&outcome, retry) {
                (Ok(()), _) => HookStatus::Delivered,
                (Err(_), Some(_)) => HookStatus::Pending,
                (Err(_), None) => HookStatus::Failed,
            };
            runs.lock().hook(&due.delivery.run, status, due.attempts, outcome.err());
            if let (HookStatus::Pending, Some(wait)) = (status, retry) {
                due.due = Instant::now().checked_add(wait).unwrap_or_else(Instant::now);
                waiting.push(due);
            }
        }
    }
}

/// One attempt: sign with the current secret and time, call a checked address, want a 2xx.
fn attempt(settings: &Settings, runtime: &tokio::runtime::Runtime, delivery: &Delivery) -> Result<(), String> {
    let account = Account::new(ACCOUNT).map_err(|e| e.to_string())?;
    let secret = settings.store.get(&account).map_err(|e| e.to_string())?.ok_or("no webhook secret is set; run `nodecraft-cli webhook-secret`")?;
    let key = key(secret.expose()).ok_or("the webhook secret starts with `whsec_` but the rest isn't base64")?;
    let host = delivery.url.host_str().ok_or("the webhook URL has no host")?;
    let port = delivery.url.port_or_known_default().ok_or("the webhook URL has no port")?;
    let address = address(host, port, settings.allow_private)?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_string();
    let signature = sign(&key, &delivery.event, &timestamp, &delivery.body)?;
    let mut client = reqwest::Client::builder()
        .user_agent(nodecraft_providers::http::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(settings.timeout)
        .no_proxy();
    // Pin the checked address, so a second lookup can't send the request somewhere else.
    if delivery.url.domain().is_some() {
        client = client.resolve(host, address);
    }
    let client = client.build().map_err(|e| format!("can't make the webhook client: {}", clip(&e.without_url().to_string())))?;
    let request = client
        .post(delivery.url.clone())
        .header("content-type", "application/json")
        .header("webhook-id", &delivery.event)
        .header("webhook-timestamp", &timestamp)
        .header("webhook-signature", format!("v1,{signature}"))
        .body(delivery.body.clone());
    let status = runtime.block_on(async { request.send().await.map(|response| response.status().as_u16()) });
    match status {
        Ok(200..=299) => Ok(()),
        Ok(status) => Err(format!("the receiver answered {status}")),
        // Without the URL: it may carry a token of the receiver's.
        Err(e) => Err(format!("the receiver couldn't be reached: {}", clip(&e.without_url().to_string()))),
    }
}

/// The signing key: a Standard Webhooks `whsec_<base64>` secret's bytes, or the secret's own.
pub fn key(secret: &str) -> Option<Vec<u8>> {
    match secret.strip_prefix("whsec_") {
        Some(encoded) => super::api::base64(encoded).filter(|key| !key.is_empty()),
        None => Some(secret.as_bytes().to_vec()),
    }
}

/// `base64(HMAC-SHA256(key, "{id}.{timestamp}.{body}"))`.
pub fn sign(key: &[u8], id: &str, timestamp: &str, body: &[u8]) -> Result<String, String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| "the webhook secret can't be used as a key".to_owned())?;
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(encode(&mac.finalize().into_bytes()))
}

/// The first address `host` resolves to that may be called.
fn address(host: &str, port: u16, allow_private: bool) -> Result<SocketAddr, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let found: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|_| "the webhook host can't be resolved".to_owned())?.collect();
    found.into_iter().find(|a| allow_private || public(a.ip())).ok_or_else(|| {
        "the webhook host resolves only to loopback, private or link-local addresses; start the server with --webhook-allow-private to allow them".into()
    })
}

/// Whether an address is on the public internet: not loopback, private, link-local, shared,
/// documentation, multicast or unspecified.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            let shared = a == 100 && (64..128).contains(&b);
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || shared
                || a == 0
                || a >= 240)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public(IpAddr::V4(v4)),
            None => {
                let [first, second, ..] = v6.segments();
                let unique_local = first & 0xfe00 == 0xfc00;
                let link_local = first & 0xffc0 == 0xfe80;
                let documentation = first == 0x2001 && second == 0x0db8;
                !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || unique_local || link_local || documentation)
            }
        },
    }
}

fn clip(text: &str) -> String {
    text.chars().take(200).collect()
}

/// Standard base64 with padding.
pub fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |n: u32| ALPHABET.get((n & 63) as usize).map_or('=', |&b| char::from(b));
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let (a, b, c) = (chunk.first().copied(), chunk.get(1).copied(), chunk.get(2).copied());
        let n = (u32::from(a.unwrap_or(0)) << 16) | (u32::from(b.unwrap_or(0)) << 8) | u32::from(c.unwrap_or(0));
        out.push(symbol(n >> 18));
        out.push(symbol(n >> 12));
        out.push(if b.is_some() { symbol(n >> 6) } else { '=' });
        out.push(if c.is_some() { symbol(n) } else { '=' });
    }
    out
}

/// How a receiver checks a delivery: the signature in constant time, and the timestamp within
/// `tolerance` of `now` so an old delivery can't be replayed. The docs describe the same steps.
#[cfg(test)]
pub fn verify(key: &[u8], id: &str, timestamp: &str, signatures: &str, body: &[u8], now: u64, tolerance: u64) -> Result<(), &'static str> {
    let sent: u64 = timestamp.parse().map_err(|_| "bad timestamp")?;
    if now.abs_diff(sent) > tolerance {
        return Err("too old or too new");
    }
    for signature in signatures.split(' ') {
        let Some(encoded) = signature.strip_prefix("v1,") else { continue };
        let Some(given) = super::api::base64(encoded) else { continue };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else { return Err("bad key") };
        mac.update(format!("{id}.{timestamp}.").as_bytes());
        mac.update(body);
        if mac.verify_slice(&given).is_ok() {
            return Ok(());
        }
    }
    Err("no valid signature")
}
