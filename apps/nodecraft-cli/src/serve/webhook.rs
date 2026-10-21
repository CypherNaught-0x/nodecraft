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
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use nodecraft_engine::secrets::{Account, SecretStore};
use nodecraft_types::{Error, ErrorKind};
use sha2::Sha256;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use url::Url;

use super::worker::{HookStatus, Runs};

/// The keychain account the signing secret lives under.
pub const ACCOUNT: &str = "serve-webhook";
/// The payload's version; bumped when its shape changes incompatibly.
pub const PAYLOAD_VERSION: u64 = 1;
/// Shortest plain secret taken, so signatures can't be guessed.
pub const MIN_SECRET: usize = 32;
/// Fewest key bytes a `whsec_` secret may decode to.
pub const MIN_KEY: usize = 24;
/// Longest callback URL taken.
const MAX_URL: usize = 2048;
/// Deliveries in progress (sending or waiting to retry) at most; a run ending beyond this
/// records a failed delivery.
pub const MAX_PENDING: usize = 256;
/// Deliveries handed over but not yet taken up, at most.
const MAX_QUEUED: usize = 32;
/// Attempts under way at once.
const MAX_SENDING: usize = 16;
/// The largest payload sent; a bigger one leaves out output text and values.
pub const MAX_PAYLOAD: usize = 256 * 1024;
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

/// The payload for a finished run, `described` as `GET /runs/{id}` shows it. Past
/// [`MAX_PAYLOAD`] bytes the outputs' text and values are left out, then everything but the
/// run's summary, marked `"truncated": true`; the receiver fetches the rest from the server.
pub fn payload(event: &str, mut described: serde_json::Value) -> Vec<u8> {
    let wrap = |run: &serde_json::Value| {
        let payload = serde_json::json!({"type": "run.completed", "version": PAYLOAD_VERSION, "event": event, "run": run});
        serde_json::to_vec(&payload).unwrap_or_default()
    };
    let body = wrap(&described);
    if body.len() <= MAX_PAYLOAD {
        return body;
    }
    if let Some(outputs) = described.get_mut("outputs").and_then(serde_json::Value::as_array_mut) {
        for output in outputs.iter_mut().filter_map(serde_json::Value::as_object_mut) {
            output.remove("text");
            output.remove("value");
        }
    }
    described["truncated"] = serde_json::json!(true);
    let body = wrap(&described);
    if body.len() <= MAX_PAYLOAD {
        return body;
    }
    let mut summary = serde_json::Map::new();
    for field in ["id", "status", "technique", "version", "cost", "error", "truncated"] {
        if let Some(value) = described.get(field) {
            summary.insert(field.to_owned(), value.clone());
        }
    }
    wrap(&serde_json::Value::Object(summary))
}

/// Starts the delivery thread. Deliveries are offered with [`offer`]; a full queue means the
/// delivery is dropped (and recorded as failed by the caller).
pub fn spawn(settings: Arc<Settings>, runs: Arc<Runs>) -> Result<Sender<Delivery>, Error> {
    let (sender, queue) = channel(MAX_QUEUED);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the webhook runtime: {e}")))?;
    std::thread::Builder::new()
        .name("nodecraft-serve-webhooks".into())
        .spawn(move || runtime.block_on(deliver(settings, queue, runs)))
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the webhook thread: {e}")))?;
    Ok(sender)
}

/// Whether a delivery couldn't be queued; the run records it.
pub fn offer(sender: &Sender<Delivery>, delivery: Delivery) -> Result<(), &'static str> {
    match sender.try_send(delivery) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err("too many webhook deliveries are waiting"),
        Err(TrySendError::Closed(_)) => Err("the webhook thread has stopped"),
    }
}

/// Takes deliveries until the server is gone, each in its own task: one slow receiver doesn't
/// hold up the others. At most [`MAX_PENDING`] are in progress, and [`MAX_SENDING`] attempts
/// are under way at once.
async fn deliver(settings: Arc<Settings>, mut queue: Receiver<Delivery>, runs: Arc<Runs>) {
    let pending = Arc::new(AtomicUsize::new(0));
    let sending = Arc::new(Semaphore::new(MAX_SENDING));
    while let Some(delivery) = queue.recv().await {
        if pending.fetch_add(1, Ordering::SeqCst) >= MAX_PENDING {
            pending.fetch_sub(1, Ordering::SeqCst);
            runs.lock().hook(&delivery.run, HookStatus::Failed, 0, Some("too many webhook deliveries are waiting".into()));
            continue;
        }
        let (settings, runs, pending, sending) = (Arc::clone(&settings), Arc::clone(&runs), Arc::clone(&pending), Arc::clone(&sending));
        tokio::spawn(async move {
            retry(&settings, &runs, &sending, &delivery).await;
            pending.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Attempts one delivery until it gets through or its attempts run out, recording each.
async fn retry(settings: &Arc<Settings>, runs: &Runs, sending: &Semaphore, delivery: &Delivery) {
    let mut attempts = 0usize;
    loop {
        attempts = attempts.saturating_add(1);
        let outcome = match sending.acquire().await {
            Ok(_permit) => attempt(settings, delivery).await,
            Err(_) => Err("the webhook thread has stopped".into()),
        };
        let wait = settings.schedule.get(attempts.saturating_sub(1)).copied();
        let status = match (&outcome, wait) {
            (Ok(()), _) => HookStatus::Delivered,
            (Err(_), Some(_)) => HookStatus::Pending,
            (Err(_), None) => HookStatus::Failed,
        };
        runs.lock().hook(&delivery.run, status, attempts, outcome.err());
        match (status, wait) {
            (HookStatus::Pending, Some(wait)) => tokio::time::sleep(wait).await,
            _ => return,
        }
    }
}

/// One attempt: sign with the current secret and time, call a checked address, want a 2xx, all
/// within the attempt's timeout.
async fn attempt(settings: &Arc<Settings>, delivery: &Delivery) -> Result<(), String> {
    // The keychain may wait for the user to unlock it: off the delivery thread.
    let store = Arc::clone(settings);
    let secret = tokio::task::spawn_blocking(move || Account::new(ACCOUNT).and_then(|account| store.store.get(&account)))
        .await
        .map_err(|_| "the keychain read stopped".to_owned())?
        .map_err(|e| e.to_string())?
        .ok_or("no webhook secret is set; run `nodecraft-cli webhook-secret`")?;
    let key = key(secret.expose())?;
    let seconds = settings.timeout.as_secs();
    tokio::time::timeout(settings.timeout, send(settings, delivery, &key))
        .await
        .unwrap_or_else(|_| Err(format!("the receiver didn't answer within {seconds} s")))
}

async fn send(settings: &Settings, delivery: &Delivery, key: &[u8]) -> Result<(), String> {
    let host = delivery.url.host_str().ok_or("the webhook URL has no host")?;
    let port = delivery.url.port_or_known_default().ok_or("the webhook URL has no port")?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let found: Vec<SocketAddr> = tokio::net::lookup_host((bare, port)).await.map_err(|_| "the webhook host can't be resolved".to_owned())?.collect();
    let address = found.into_iter().find(|a| settings.allow_private || public(a.ip())).ok_or_else(|| {
        "the webhook host resolves only to loopback, private or link-local addresses; start the server with --webhook-allow-private to allow them".to_owned()
    })?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_string();
    let signature = sign(key, &delivery.event, &timestamp, &delivery.body)?;
    let mut client = reqwest::Client::builder()
        .user_agent(nodecraft_providers::http::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(settings.timeout)
        .no_proxy();
    // Pin the checked address, so a second lookup can't send the request somewhere else.
    if delivery.url.domain().is_some() {
        client = client.resolve(host, address);
    }
    let client = client.build().map_err(|e| format!("can't make the webhook client: {}", described(&e)))?;
    let response = client
        .post(delivery.url.clone())
        .header("content-type", "application/json")
        .header("webhook-id", &delivery.event)
        .header("webhook-timestamp", &timestamp)
        .header("webhook-signature", format!("v1,{signature}"))
        .body(delivery.body.clone())
        .send()
        .await;
    match response.map(|response| response.status().as_u16()) {
        Ok(200..=299) => Ok(()),
        Ok(status) => Err(format!("the receiver answered {status}")),
        Err(e) if e.is_timeout() => Err("the receiver didn't answer in time".into()),
        Err(e) if e.is_connect() => Err(format!("couldn't connect to the receiver: {}", described(&e))),
        Err(e) => Err(format!("the receiver couldn't be reached: {}", described(&e))),
    }
}

/// A reqwest error and its causes, without the URL: it may carry a token of the receiver's.
/// The causes (connection, TLS, IO) don't name it.
fn described(error: &reqwest::Error) -> String {
    let mut text = String::new();
    let mut cause = std::error::Error::source(error);
    while let Some(next) = cause {
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&next.to_string());
        cause = next.source();
    }
    if text.is_empty() {
        text = if error.is_builder() { "a bad request" } else { "the request failed" }.to_owned();
    }
    clip(&text)
}

/// The signing key: a Standard Webhooks `whsec_<base64>` secret's bytes, or the secret's own;
/// at least [`MIN_KEY`] bytes of the first or [`MIN_SECRET`] characters of the second.
pub fn key(secret: &str) -> Result<Vec<u8>, String> {
    match secret.strip_prefix("whsec_") {
        Some(encoded) => match super::api::base64(encoded) {
            Some(key) if key.len() >= MIN_KEY => Ok(key),
            Some(_) => Err(format!("a `whsec_` webhook secret must hold at least {MIN_KEY} bytes of key")),
            None => Err("the webhook secret starts with `whsec_` but the rest isn't base64".into()),
        },
        None if secret.len() >= MIN_SECRET => Ok(secret.as_bytes().to_vec()),
        None => Err(format!("the webhook secret must be at least {MIN_SECRET} characters")),
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

/// Whether an address is on the public internet: not loopback, private, link-local, shared,
/// documentation, multicast or unspecified.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            let shared = a == 100 && (64..128).contains(&b);
            let protocol = a == 192 && b == 0 && c == 0;
            let benchmarking = a == 198 && (b == 18 || b == 19);
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || shared
                || protocol
                || benchmarking
                || a == 0
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let [first, second, ..] = segments;
            let [.., hi, lo] = segments;
            // The IPv4 address an IPv6 one carries: mapped, compatible (`::a.b.c.d`), NAT64, 6to4.
            let embedded = |high: u16, low: u16| IpAddr::V4(std::net::Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)));
            let [_, _, third, ..] = segments;
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public(IpAddr::V4(v4));
            }
            let compatible = segments.iter().take(6).all(|&s| s == 0) && !v6.is_loopback() && !v6.is_unspecified();
            let nat64 = first == 0x0064 && second == 0xff9b;
            if compatible || nat64 {
                return public(embedded(hi, lo));
            }
            if first == 0x2002 {
                return public(embedded(second, third));
            }
            let unique_local = first & 0xfe00 == 0xfc00;
            let link_local = first & 0xffc0 == 0xfe80;
            let site_local = first & 0xffc0 == 0xfec0;
            let documentation = first == 0x2001 && second == 0x0db8;
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || unique_local || link_local || site_local || documentation)
        }
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
