//! Fixed public-origin, index-first template transport. No keys, dialogs or provider runs.
use super::{
    Entry, MAX_BYTES, MAX_INDEX_BYTES,
    catalog::{self, Identity},
    invalid, parse_index, validate_archive, validate_nodes,
};
use crate::{
    Session,
    commands::{CommandSpec, Context, Effect, params},
};
use nodecraft_providers::http::{Http, HttpRequest};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SERVICE: &str = "templates.fetch";
pub const ORIGIN: &str = "https://raw.githubusercontent.com/CypherNaught-0x/nodecraft-templates/main/";
/// Every template request's timeout.
pub const TIMEOUT_MS: u64 = 10_000;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Index,
    Archive {
        entry: Entry,
    },
    /// `templates.update` (#954): the index, then the listed archives not already held.
    Update {
        /// Every listed identity, so a rewritten published version is refused before downloading.
        known: Vec<Identity>,
        /// The identities already available (bundled or cached).
        have: Vec<Identity>,
    },
    /// `templates.clear_cache`: no network; the host settles it on the session.
    ClearCache,
}
pub enum Fetched {
    Index(Vec<u8>),
    Archive { entry: Entry, bytes: Vec<u8> },
    Update(Box<super::update::Updated>),
    ClearCache,
}

/// Where templates are read from: the configured templates repository. Commands never take a
/// URL; only the host builds a source, and only over HTTPS (or, for tests and debug builds, a
/// loopback fixture server).
#[derive(Clone)]
pub struct Source {
    base: String,
    timeout_ms: u64,
    keys: Vec<ed25519_dalek::VerifyingKey>,
}
impl Source {
    /// The official repository. It publishes no index signing key yet (see the #954 decision).
    pub fn official() -> Self {
        Self { base: ORIGIN.to_owned(), timeout_ms: TIMEOUT_MS, keys: Vec::new() }
    }
    /// A local fixture server, `http://127.0.0.1:<port>/`: integration tests and debug builds.
    pub fn loopback(base: &str) -> Result<Self, Error> {
        let refused = || Error::new(ErrorKind::InvalidInput, "A template fixture source must be http://127.0.0.1:<port>/");
        let port = base.strip_prefix("http://127.0.0.1:").and_then(|rest| rest.strip_suffix('/')).ok_or_else(refused)?;
        if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refused());
        }
        Ok(Self { base: base.to_owned(), timeout_ms: TIMEOUT_MS, keys: Vec::new() })
    }
    /// A shorter timeout, between 1 ms and [`TIMEOUT_MS`].
    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms.clamp(1, TIMEOUT_MS);
        self
    }
    /// Require `index.json.sig` to verify against one of `keys`.
    pub fn with_keys(mut self, keys: Vec<ed25519_dalek::VerifyingKey>) -> Self {
        self.keys = keys;
        self
    }
    pub fn base(&self) -> &str {
        &self.base
    }
    pub(super) fn signed(&self) -> bool {
        !self.keys.is_empty()
    }
    /// The keys the index must be signed with (none for the official repository today).
    pub fn keys(&self) -> &[ed25519_dalek::VerifyingKey] {
        &self.keys
    }
    /// One bounded GET of `path` (already validated) under the source.
    pub(super) async fn get(&self, http: &dyn Http, path: &str, cap: usize) -> Result<Vec<u8>, Error> {
        let request_http = HttpRequest::get(format!("{}{path}", self.base)).timeout_ms(self.timeout_ms).max_response_bytes(cap);
        #[cfg(not(target_arch = "wasm32"))]
        let reply = http.send_no_redirect(request_http).await;
        // Browser Fetch owns redirect policy. These are public fixed-origin GETs without credentials;
        // the index schema and exact selected content digest remain mandatory after transport.
        #[cfg(target_arch = "wasm32")]
        let reply = http.send(request_http).await;
        let reply = reply.map_err(|error| match error.kind {
            nodecraft_providers::ErrorKind::Timeout => {
                Error::new(ErrorKind::Timeout, "The template repository didn't answer in time; bundled and previously retrieved templates remain available")
            }
            nodecraft_providers::ErrorKind::Malformed => {
                Error::new(ErrorKind::InvalidInput, "The template repository sent more than the size limit; keeping the previous templates")
            }
            _ => Error::new(ErrorKind::Network, "The template repository could not be read; bundled and previously retrieved templates remain available"),
        })?;
        if reply.status == 404 {
            return Err(Error::new(ErrorKind::NotFound, "The template repository has no such file; keeping the previous templates"));
        }
        if reply.status != 200 || reply.body.len() > cap {
            return Err(Error::new(
                ErrorKind::Network,
                "The template repository refused the request or exceeded its size limit; keeping the previous templates",
            ));
        }
        Ok(reply.body)
    }
}

/// Reads from the official repository.
pub async fn fetch(http: &dyn Http, request: Request) -> Result<Fetched, Error> {
    fetch_from(http, &Source::official(), request).await
}

pub async fn fetch_from(http: &dyn Http, source: &Source, request: Request) -> Result<Fetched, Error> {
    match request {
        Request::Index => {
            let body = source.get(http, "index.json", MAX_INDEX_BYTES).await?;
            // A single-step refresh stores no signature; in signed mode the cached copy then fails
            // verification at the next start and is ignored, never trusted.
            let _signature = super::update::verify_signature(http, source, &body).await?;
            parse_index(&body)?;
            Ok(Fetched::Index(body))
        }
        Request::Archive { entry } => {
            // Even a host that calls this directly cannot introduce a URL/path through metadata.
            parse_index(&serde_json::to_vec(&json!({"schema_version":1,"templates":[entry]})).map_err(|_| invalid())?)?;
            let body = source.get(http, &entry.file, MAX_BYTES).await?;
            admit(&entry, &body)?;
            Ok(Fetched::Archive { entry, bytes: body })
        }
        Request::Update { known, have } => Ok(Fetched::Update(Box::new(super::update::run(http, source, &known, &have).await?))),
        Request::ClearCache => Ok(Fetched::ClearCache),
    }
}

/// Every check an archive passes before it is stored or shown: its index digest first, then its
/// graph, assets and node kinds.
pub(super) fn admit(entry: &Entry, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "The downloaded template is larger than its limit; it was rejected"));
    }
    if blake3::hash(bytes).to_hex().as_str() != entry.blake3 {
        return Err(Error::new(ErrorKind::InvalidInput, "The downloaded template doesn't match its index digest; it was rejected and the previous copy kept"));
    }
    let snapshot = validate_archive(entry, bytes)?;
    let (kinds, _) = crate::generation_catalog::default_registry()?;
    validate_nodes(&snapshot, &kinds)
}

pub fn finish(session: &mut Session, fetched: Fetched) -> Result<(), Error> {
    match fetched {
        Fetched::Index(bytes) => catalog::install_index(session, &bytes),
        Fetched::Archive { entry, bytes } => catalog::install_archive(session, entry, bytes),
        Fetched::Update(updated) => super::update::apply(session, *updated),
        Fetched::ClearCache => super::disk::clear(session),
    }
}

/// Settles a host request's outcome on the session: installs what was fetched, or records why an
/// update failed for `templates.status`.
pub fn settle(session: &mut Session, result: Result<Fetched, Error>) -> Result<(), Error> {
    match result {
        Ok(fetched) => finish(session, fetched),
        Err(error) => {
            super::update::failed(session, &error);
            Err(error)
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
pub(super) fn enabled(c: &Context<'_>, _: &Value) -> Result<(), Error> {
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "This host cannot retrieve templates; use the bundled offline set or a desktop/browser host"));
    }
    Ok(())
}
fn refresh(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: Empty = params(value)?;
    enabled(c, value)?;
    Ok(Effect::Service { capability: SERVICE, request: serde_json::to_value(Request::Index).map_err(|_| invalid())? })
}
fn retrieve(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let identity: Identity = params(value)?;
    if identity.id.len() > 64 || identity.blake3.len() != 64 || identity.version == 0 {
        return Err(invalid());
    }
    enabled(c, value)?;
    let listing = catalog::listing(c.resources)?;
    let entry = listing
        .templates
        .into_iter()
        .find(|entry| Identity::from(entry) == identity)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "This template selection is no longer listed; refresh the list"))?;
    if listing.available.contains(&identity) {
        return Ok(Effect::None { result: json!({"available":true}) });
    }
    Ok(Effect::Service { capability: SERVICE, request: serde_json::to_value(Request::Archive { entry }).map_err(|_| invalid())? })
}
fn refresh_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":[],"properties":{},"additionalProperties":false})
}
fn retrieve_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["id","version","blake3"],"properties":{"id":{"type":"string","minLength":1,"maxLength":64},"version":{"type":"integer","minimum":1,"maximum":4294967295u64},"blake3":{"type":"string","minLength":64,"maxLength":64}},"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    let mut specs = vec![
        CommandSpec {
            id: "template.refresh",
            label: "Refresh starter templates",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: refresh_schema,
            params_doc: "{} -> {pending:id}; retrieve only the official bounded template index asynchronously. Keeps bundled/previously verified templates on failure; never changes the project or starts a run.",
            enabled,
            run: refresh,
        },
        CommandSpec {
            id: "template.retrieve",
            label: "Retrieve starter template",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: retrieve_schema,
            params_doc: "{id,version,blake3} -> {pending:id} or {available:true}; retrieve one exact listed archive, verify its digest/graph/assets/kinds and cache it for this Session. Add separately through template.add. No dialogs, credentials, paid requests or project edits.",
            enabled,
            run: retrieve,
        },
    ];
    specs.extend(super::update::specs());
    specs
}
