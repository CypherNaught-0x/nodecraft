//! Installing a newer release (#540): the signed update feed and the two commands that ask the
//! host to download and to restart into an update.
//!
//! Every release carries a feed, `nodecraft-update.json`, listing each platform's update payload
//! by file name, size and BLAKE3 hash, and an Ed25519 signature of it, `nodecraft-update.json.sig`
//! (`cargo xtask update-feed` writes both). [`download`] reads the feed for the release
//! `app.check_updates` offered, refuses it unless one of the keys built into the app signed it,
//! it names that same release and the release is newer than this build, then downloads this
//! platform's payload and refuses it unless its size and hash are the feed's. Nothing that
//! hasn't been verified reaches the disk. The private key is a release secret; without a public
//! key in the build (`packaging/update-key.pub`), installing is unsupported and the banner only
//! notifies.
//!
//! `app.install_update` asks the host's [`INSTALL_SERVICE`] to download and stage the update;
//! `app.restart_to_update` asks its [`APPLY_SERVICE`] to swap it in and restart, which the host
//! refuses while a generation runs. Neither is offered to agents: installing software and
//! restarting the window are the person's to ask for. Handlers do no I/O.
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use crate::session::Session;
use crate::update_check::{self, Available};
use ed25519_dalek::{Signature, VerifyingKey};
use nodecraft_providers::http::{Http, HttpMethod, HttpRequest};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The host service that downloads and stages an update.
pub const INSTALL_SERVICE: &str = "updates.install";
/// The host service that swaps a staged update in and restarts.
pub const APPLY_SERVICE: &str = "updates.apply";

/// Release assets are downloaded from here, followed by the tag and the file name.
pub const DOWNLOADS: &str = "https://github.com/CypherNaught-0x/NodeCraft/releases/download/";
/// The feed's file name in every release.
pub const FEED_FILE: &str = "nodecraft-update.json";
/// Its signature's file name.
pub const SIGNATURE_FILE: &str = "nodecraft-update.json.sig";

/// What a feed signature signs: this, then the feed's bytes, so a signature made for anything
/// else with the same key never passes as a feed's.
pub const SIGNED_PREFIX: &[u8] = b"NodeCraft update feed v1\n";

/// The feed format this build reads.
pub const FEED_FORMAT: u32 = 1;

const MAX_FEED_BYTES: usize = 64 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
/// The largest payload a feed may name.
pub const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILE_NAME_BYTES: usize = 128;
const MAX_TARGETS: usize = 32;
const MAX_KEYS: usize = 8;
const FEED_TIMEOUT_MS: u64 = 30_000;
const PAYLOAD_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// The public keys release feeds are signed with, one hex-encoded Ed25519 key per line.
const BUILT_IN_KEYS: &str = include_str!("../../../packaging/update-key.pub");

/// A release's signed update feed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub format: u32,
    /// The release, as its tag names it without the `v`.
    pub version: String,
    /// Each platform's payload, by [`target`] name.
    pub targets: BTreeMap<String, Payload>,
}

/// One platform's update payload, a file in the same release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub file: String,
    pub size: u64,
    /// Lowercase hex.
    pub blake3: String,
}

/// A verified payload, ready to stage.
#[derive(Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub version: String,
    pub file: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Downloaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloaded").field("version", &self.version).field("file", &self.file).field("len", &self.bytes.len()).finish()
    }
}

/// The payload this build installs: `macos-universal` (a zipped app bundle), `linux-<arch>` (an
/// AppImage) or `windows-<arch>` (an MSI). `None` where releases don't ship one.
pub fn target() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("macos-universal")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("linux-aarch64")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x64")
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some("windows-arm64")
    } else {
        None
    }
}

/// The keys built into this app; empty when it was built without one, so it can't install.
pub fn built_in_keys() -> Vec<VerifyingKey> {
    // A malformed line in the committed file is a build mistake the tests catch; here it is
    // skipped, and a build with no usable key only notifies.
    parse_keys(BUILT_IN_KEYS).unwrap_or_default()
}

/// Public keys, one 64-digit hex Ed25519 key per line; blank lines and `#` comments are skipped.
pub fn parse_keys(text: &str) -> Result<Vec<VerifyingKey>, Error> {
    let mut keys = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let bytes: [u8; 32] =
            hex(line).and_then(|bytes| bytes.try_into().ok()).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "an update key isn't 64 hex digits"))?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| Error::new(ErrorKind::InvalidInput, "an update key isn't an Ed25519 public key"))?;
        if key.is_weak() {
            return Err(Error::new(ErrorKind::InvalidInput, "an update key is a weak Ed25519 key"));
        }
        if keys.len() == MAX_KEYS {
            return Err(Error::new(ErrorKind::InvalidInput, format!("more than {MAX_KEYS} update keys")));
        }
        keys.push(key);
    }
    Ok(keys)
}

/// Lowercase or uppercase hex digits to bytes.
fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |b: u8| char::from(b).to_digit(16).and_then(|d| u8::try_from(d).ok());
    text.as_bytes().chunks(2).map(|pair| Some(digit(*pair.first()?)? << 4 | digit(*pair.get(1)?)?)).collect()
}

/// The feed in `bytes`, when one of `keys` signed it (`signature`: hex, as the `.sig` file holds
/// it) and it is a feed this build reads for release `version`.
pub fn verify_feed(bytes: &[u8], signature: &[u8], keys: &[VerifyingKey], version: &str) -> Result<Feed, Error> {
    let refused = |why: &str| Error::new(ErrorKind::InvalidInput, format!("the update for NodeCraft {version} was refused: {why}"));
    let signature = std::str::from_utf8(signature).ok().map(str::trim).and_then(hex).and_then(|bytes| <[u8; 64]>::try_from(bytes).ok());
    let signature = Signature::from_bytes(&signature.ok_or_else(|| refused("its signature file is damaged"))?);
    let mut message = Vec::with_capacity(SIGNED_PREFIX.len().saturating_add(bytes.len()));
    message.extend_from_slice(SIGNED_PREFIX);
    message.extend_from_slice(bytes);
    if !keys.iter().any(|key| key.verify_strict(&message, &signature).is_ok()) {
        return Err(refused("its signature doesn't match NodeCraft's release key"));
    }
    let feed: Feed = serde_json::from_slice(bytes).map_err(|_| refused("its feed can't be read"))?;
    if feed.format != FEED_FORMAT {
        return Err(refused("its feed is in a format this version can't read; download it from the releases page"));
    }
    // A feed signed for another release, replayed under this one's tag, is refused.
    if feed.version != version {
        return Err(refused("its feed is for a different release"));
    }
    if feed.targets.len() > MAX_TARGETS {
        return Err(refused("its feed lists too many platforms"));
    }
    Ok(feed)
}

/// The payload for `target` in a verified feed, checked before anything is fetched.
fn payload<'a>(feed: &'a Feed, target: &str) -> Result<&'a Payload, Error> {
    let payload = feed.targets.get(target).ok_or_else(|| {
        Error::new(ErrorKind::Unsupported, format!("NodeCraft {} has no update for this platform; download it from the releases page", feed.version))
    })?;
    let file = &payload.file;
    // The name goes into a url and onto the disk: plain characters only, no path.
    let plain = !file.is_empty()
        && file.len() <= MAX_FILE_NAME_BYTES
        && !file.starts_with('.')
        && file.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if !plain {
        return Err(Error::new(ErrorKind::InvalidInput, "the update feed names an unusable file"));
    }
    if payload.size == 0 || payload.size > MAX_PAYLOAD_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "the update feed gives an impossible size"));
    }
    if payload.blake3.len() != 64 || hex(&payload.blake3).is_none() {
        return Err(Error::new(ErrorKind::InvalidInput, "the update feed gives an unusable hash"));
    }
    Ok(payload)
}

/// Downloads and verifies `release`'s payload for `target` from `base` (normally [`DOWNLOADS`];
/// a test feed's address under `NODECRAFT_UPDATE_FEED`). `current` is this build's version: a
/// release that isn't newer is refused, so a signed old feed can't roll the app back.
pub async fn download(http: &dyn Http, base: &str, release: &Available, current: &str, target: &str, keys: &[VerifyingKey]) -> Result<Downloaded, Error> {
    if keys.is_empty() {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "this build of NodeCraft has no update key, so it can't install updates; download them from the releases page",
        ));
    }
    if !update_check::is_newer(&release.version, current) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("NodeCraft {} isn't newer than this version ({current})", release.version)));
    }
    let tag = release.tag().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "that release isn't one of NodeCraft's"))?;
    let at = |file: &str| format!("{}/{tag}/{file}", base.trim_end_matches('/'));
    let feed_bytes = get(http, &at(FEED_FILE), MAX_FEED_BYTES, FEED_TIMEOUT_MS).await?;
    let signature = get(http, &at(SIGNATURE_FILE), MAX_SIGNATURE_BYTES, FEED_TIMEOUT_MS).await?;
    let feed = verify_feed(&feed_bytes, &signature, keys, &release.version)?;
    let payload = payload(&feed, target)?;
    let limit = usize::try_from(payload.size).map_err(|_| Error::new(ErrorKind::InvalidInput, "the update is too large for this computer"))?;
    let bytes = get(http, &at(&payload.file), limit, PAYLOAD_TIMEOUT_MS).await?;
    verify_payload(payload, &bytes)?;
    Ok(Downloaded { version: feed.version.clone(), file: payload.file.clone(), bytes })
}

/// Refuses a payload whose size or hash isn't the signed feed's.
fn verify_payload(payload: &Payload, bytes: &[u8]) -> Result<(), Error> {
    let refused = |why: &str| Error::new(ErrorKind::InvalidInput, format!("the downloaded update was refused: {why}; nothing was installed"));
    if u64::try_from(bytes.len()).ok() != Some(payload.size) {
        return Err(refused("its size isn't the one the release signed"));
    }
    if blake3::hash(bytes).to_hex().as_str() != payload.blake3.to_ascii_lowercase() {
        return Err(refused("its contents aren't the ones the release signed"));
    }
    Ok(())
}

async fn get(http: &dyn Http, url: &str, limit: usize, timeout_ms: u64) -> Result<Vec<u8>, Error> {
    let request = HttpRequest {
        method: HttpMethod::Get,
        url: url.into(),
        headers: Vec::new(),
        secret_headers: Vec::new(),
        body: Vec::new(),
        timeout_ms,
        max_response_bytes: limit,
    };
    let response = http.send(request).await.map_err(|error| Error::new(ErrorKind::Network, format!("the update couldn't be downloaded: {}", error.message)))?;
    match response.status {
        200 => Ok(response.body),
        404 => Err(Error::new(ErrorKind::NotFound, "that release has no update to install; download it from the releases page")),
        status => Err(Error::new(ErrorKind::Network, format!("the update couldn't be downloaded: the server answered {status}"))),
    }
}

/// Where an update stands, which `app.update_status` reports as `install`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Install {
    /// Downloading and verifying `version`.
    Downloading { version: String },
    /// `version` is verified and staged; `app.restart_to_update` swaps it in.
    Ready { version: String },
    /// The last attempt failed; nothing was installed.
    Failed { version: String, error: String },
}

/// The current install, a session resource the host records.
struct LastInstall(Install);

/// Records where the update stands for `app.update_status`.
pub fn record(session: &mut Session, install: Option<Install>) {
    session.swap_resource(install.map(LastInstall));
}

/// The install `app.update_status` reports.
pub(crate) fn last(c: &Context<'_>) -> Option<Value> {
    let last = c.resources.get::<LastInstall>()?;
    serde_json::to_value(&last.0).ok()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn enabled_service(c: &Context<'_>, value: &Value, service: &str) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    if !c.services.contains(service) {
        return Err(Error::new(ErrorKind::Unsupported, "this host doesn't install updates; the desktop app does"));
    }
    Ok(())
}

fn enabled_install(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    enabled_service(c, value, INSTALL_SERVICE)
}

fn install(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_install(c, value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    Ok(Effect::Service { capability: INSTALL_SERVICE, request: json!({}) })
}

fn enabled_apply(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    enabled_service(c, value, APPLY_SERVICE)
}

fn apply(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_apply(c, value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    Ok(Effect::Service { capability: APPLY_SERVICE, request: json!({}) })
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "properties": {}, "required": []})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "app.install_update",
            label: "Install Update",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {pending: version}; downloads the release the last app.check_updates offered, verifies its signed feed and payload, and stages it; app.update_status reports install.state downloading, ready or failed. Nothing is replaced until app.restart_to_update. Refused when nothing is offered, when this copy is updated by a package manager or isn't an installed app, and when the build has no update key.",
            enabled: enabled_install,
            run: install,
        },
        CommandSpec {
            id: "app.restart_to_update",
            label: "Restart to Update",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {restarting: version}; replaces this copy with the staged update and restarts the window, reopening the project; unsaved changes are offered back by crash recovery. Refused (busy) while a generation runs or waits, and when no update is ready.",
            enabled: enabled_apply,
            run: apply,
        },
    ]
}

#[cfg(test)]
mod tests;
