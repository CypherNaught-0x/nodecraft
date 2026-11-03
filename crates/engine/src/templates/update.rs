//! One explicit, bounded template update (#954): `templates.update` reads the configured
//! repository's index, then downloads at most [`MAX_PER_UPDATE`] listed archives this session
//! doesn't already hold, each checked against its index digest before it is stored or shown.
//! `templates.status` reports the last outcome and the cache; `templates.clear_cache` removes the
//! downloaded ones. Nothing here runs at startup or without a command. Handlers do no I/O: the host
//! runs [`run`] off the UI thread and settles it with [`super::fetch::settle`].
use super::{
    Entry, MAX_BYTES, MAX_INDEX_BYTES,
    catalog::{self, Identity},
    fetch::{Request, SERVICE, Source, admit, enabled},
    invalid, parse_index,
};
use crate::{
    Session,
    commands::{CommandSpec, Context, Effect, Phase, params},
};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Most archives one update downloads; the rest are reported as skipped for the next one.
pub const MAX_PER_UPDATE: usize = 16;
/// What an index signature signs, before the index bytes.
pub const SIGNATURE_CONTEXT: &[u8] = b"NodeCraft templates index v1\n";
/// Largest identity list a host request carries (bundled + indexed + cached, with room).
const MAX_IDENTITIES: usize = 512;

/// A template an update could not take, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub id: String,
    pub version: u32,
    pub error: String,
}

/// What the network half of an update brought back, verified but not yet installed.
pub struct Updated {
    pub index: Vec<u8>,
    pub signed: bool,
    pub archives: Vec<(Entry, Vec<u8>)>,
    pub failed: Vec<Failure>,
    pub skipped: usize,
}

/// The outcome of one update, which `templates.status` reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub id: u64,
    /// Templates the index lists.
    pub listed: usize,
    /// Archives verified and stored by this update.
    pub downloaded: Vec<Identity>,
    /// Archives rejected (a digest mismatch, a damaged graph, an oversize body); the last good
    /// copy of each stays available.
    pub failed: Vec<Failure>,
    /// Changed archives left for the next update by [`MAX_PER_UPDATE`] or a lost connection.
    pub skipped: usize,
    /// Whether the index carried a verified signature.
    pub signed: bool,
    /// Why the update as a whole failed; nothing changed then.
    pub error: Option<String>,
}

#[derive(Default)]
struct Status {
    updates: u64,
    pending: bool,
    last: Option<Outcome>,
    source: Option<String>,
}

fn status_mut(session: &mut Session) -> Status {
    session.swap_resource::<Status>(None).unwrap_or_default()
}

/// Records where the host reads templates from, for `templates.status`.
pub fn configure(session: &mut Session, source: &Source) {
    let mut status = status_mut(session);
    status.source = Some(source.base().to_owned());
    session.swap_resource(Some(status));
}

/// The host started an update request; `templates.status` reports it pending until settled.
pub fn started(session: &mut Session) -> u64 {
    let mut status = status_mut(session);
    status.updates = status.updates.saturating_add(1);
    status.pending = true;
    let id = status.updates;
    session.swap_resource(Some(status));
    id
}

/// The last update's outcome.
pub fn last(session: &Session) -> Option<Outcome> {
    session.resource::<Status>().and_then(|status| status.last.clone())
}

fn record(session: &mut Session, mut outcome: Outcome) -> Outcome {
    let mut status = status_mut(session);
    if !status.pending {
        status.updates = status.updates.saturating_add(1);
    }
    status.pending = false;
    outcome.id = status.updates;
    status.last = Some(outcome.clone());
    session.swap_resource(Some(status));
    outcome
}

/// A pending update's request failed as a whole (no network, a timeout, a bad index).
pub(super) fn failed(session: &mut Session, error: &Error) {
    if session.resource::<Status>().is_some_and(|status| status.pending) {
        record(
            session,
            Outcome { id: 0, listed: 0, downloaded: Vec::new(), failed: Vec::new(), skipped: 0, signed: false, error: Some(error.message.clone()) },
        );
    }
}

/// Checks `index.json.sig` when the source carries keys. Returns whether the index was signed.
pub(super) async fn verify_signature(http: &dyn Http, source: &Source, index: &[u8]) -> Result<bool, Error> {
    if !source.signed() {
        return Ok(false);
    }
    let refused = || Error::new(ErrorKind::InvalidInput, "The template index signature doesn't verify; keeping the previous templates");
    let signature = source.get(http, "index.json.sig", 128).await?;
    let signature: [u8; 64] = signature.as_slice().try_into().map_err(|_| refused())?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature);
    let mut message = Vec::with_capacity(SIGNATURE_CONTEXT.len().saturating_add(index.len()));
    message.extend_from_slice(SIGNATURE_CONTEXT);
    message.extend_from_slice(index);
    if !source.keys().iter().any(|key| key.verify_strict(&message, &signature).is_ok()) {
        return Err(refused());
    }
    Ok(true)
}

/// The network half of an update. Fails as a whole only when the index can't be read or trusted;
/// one bad archive is reported in [`Updated::failed`] and the rest continue.
pub async fn run(http: &dyn Http, source: &Source, known: &[Identity], have: &[Identity]) -> Result<Updated, Error> {
    if known.len() > MAX_IDENTITIES || have.len() > MAX_IDENTITIES {
        return Err(invalid());
    }
    let index = source.get(http, "index.json", MAX_INDEX_BYTES).await?;
    let signed = verify_signature(http, source, &index).await?;
    let entries = parse_index(&index)?;
    for entry in &entries {
        if known.iter().chain(have).any(|old| old.id == entry.id && old.version == entry.version && old.blake3 != entry.blake3) {
            return Err(Error::new(ErrorKind::Conflict, "A published template version changed its content; keeping the previous catalog"));
        }
    }
    let wanted: Vec<&Entry> = entries.iter().filter(|entry| !have.iter().any(|old| old.id == entry.id && old.version >= entry.version)).collect();
    let mut skipped = wanted.len().saturating_sub(MAX_PER_UPDATE);
    let mut archives = Vec::new();
    let mut failed = Vec::new();
    let mut offline = false;
    for entry in wanted.into_iter().take(MAX_PER_UPDATE) {
        if offline {
            skipped = skipped.saturating_add(1);
            continue;
        }
        let fetched = source.get(http, &entry.file, MAX_BYTES).await.and_then(|bytes| admit(entry, &bytes).map(|()| bytes));
        match fetched {
            Ok(bytes) => archives.push((entry.clone(), bytes)),
            Err(error) => {
                // A lost connection would fail every remaining request too, each after a timeout.
                offline = matches!(error.kind, ErrorKind::Network | ErrorKind::Timeout);
                failed.push(Failure { id: entry.id.clone(), version: entry.version, error: error.message });
            }
        }
    }
    Ok(Updated { index, signed, archives, failed, skipped })
}

/// Installs a fetched update: the index first (refused as a whole if it contradicts what is
/// held), then each archive, persisted when the host keeps a cache.
pub(super) fn apply(session: &mut Session, updated: Updated) -> Result<(), Error> {
    let Updated { index, signed, archives, mut failed, skipped } = updated;
    let listed = parse_index(&index).map_or(0, |entries| entries.len());
    if let Err(error) = catalog::install_index(session, &index) {
        record(session, Outcome { id: 0, listed, downloaded: Vec::new(), failed, skipped, signed, error: Some(error.message.clone()) });
        return Err(error);
    }
    let mut downloaded = Vec::new();
    for (entry, bytes) in archives {
        let identity = Identity::from(&entry);
        let (id, version) = (entry.id.clone(), entry.version);
        match catalog::install_archive(session, entry, bytes) {
            Ok(()) => downloaded.push(identity),
            Err(error) => failed.push(Failure { id, version, error: error.message }),
        }
    }
    record(session, Outcome { id: 0, listed, downloaded, failed, skipped, signed, error: None });
    Ok(())
}

/// The words a host shows when an update settles.
pub fn notice(outcome: &Outcome) -> String {
    if let Some(error) = &outcome.error {
        return format!("Templates were not updated: {error}");
    }
    let mut words = match outcome.downloaded.len() {
        0 => "Templates are up to date.".to_owned(),
        1 => "1 template was downloaded and verified.".to_owned(),
        n => format!("{n} templates were downloaded and verified."),
    };
    if !outcome.failed.is_empty() {
        words.push_str(&format!(" {} rejected (the previous copy is kept): {}", outcome.failed.len(), outcome.failed.first().map_or("", |f| f.error.as_str())));
    }
    if outcome.skipped > 0 {
        words.push_str(&format!(" {} more left for the next update.", outcome.skipped));
    }
    words
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn update(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: Empty = params(value)?;
    enabled(c, value)?;
    let listing = catalog::listing(c.resources)?;
    let known = listing.templates.iter().map(Identity::from).collect();
    let request = Request::Update { known, have: listing.available };
    Ok(Effect::Service { capability: SERVICE, request: serde_json::to_value(request).map_err(|_| invalid())? })
}
fn clear_cache(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: Empty = params(value)?;
    enabled(c, value)?;
    Ok(Effect::Service { capability: SERVICE, request: serde_json::to_value(Request::ClearCache).map_err(|_| invalid())? })
}
fn enabled_service(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: Empty = params(value)?;
    enabled(c, value)
}
fn enabled_status(_: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: Empty = params(value)?;
    Ok(())
}
fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: Empty = params(value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let held = c.resources.get::<Status>();
    let listing = catalog::listing(c.resources)?;
    Ok(Effect::None {
        result: json!({
            "supported": c.services.contains(SERVICE),
            "source": held.and_then(|status| status.source.clone()),
            "pending": held.is_some_and(|status| status.pending),
            "last": held.and_then(|status| status.last.clone()),
            "bundled": super::entries()?.len(),
            "available": listing.available.len(),
            "cache": super::disk::report(c.resources),
        }),
    })
}
fn empty_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":[],"properties":{},"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "templates.update",
            label: "Update starter templates",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {pending:id}; reads the configured templates repository's index over HTTPS, then downloads at most 16 listed templates this session lacks, each checked against its index digest before it is stored or shown. A mismatch is rejected and the previous copy kept. Read the outcome with templates.status. Never edits the project, runs a provider or asks for keys. Unsupported without a host fetch service (CLI).",
            enabled: enabled_service,
            run: update,
        },
        CommandSpec {
            id: "templates.status",
            label: "Starter template status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {supported, source, pending, last:{id,listed,downloaded:[{id,version,blake3}],failed:[{id,version,error}],skipped,signed,error}|null, bundled, available, cache:{layout,persistent,location,files,bytes,problems:[string]}}; read-only, no network.",
            enabled: enabled_status,
            run: status,
        },
        CommandSpec {
            id: "templates.clear_cache",
            label: "Clear downloaded templates",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {pending:id}; removes every downloaded template from the cache in the app data folder and this session. Bundled templates and templates already added to a project stay. No network.",
            enabled: enabled_service,
            run: clear_cache,
        },
    ]
}
