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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Index,
    Archive { entry: Entry },
}
pub enum Fetched {
    Index(Vec<u8>),
    Archive { entry: Entry, bytes: Vec<u8> },
}

pub async fn fetch(http: &dyn Http, request: Request) -> Result<Fetched, Error> {
    let (file, cap) = match &request {
        Request::Index => ("index.json".to_owned(), MAX_INDEX_BYTES),
        Request::Archive { entry } => {
            // Even a host that calls this directly cannot introduce a URL/path through metadata.
            parse_index(&serde_json::to_vec(&json!({"schema_version":1,"templates":[entry]})).map_err(|_| invalid())?)?;
            (entry.file.clone(), MAX_BYTES)
        }
    };
    let request_http = HttpRequest::get(format!("{ORIGIN}{file}")).timeout_ms(10_000).max_response_bytes(cap);
    #[cfg(not(target_arch = "wasm32"))]
    let reply = http.send_no_redirect(request_http).await;
    // Browser Fetch owns redirect policy. These are public fixed-origin GETs without credentials;
    // the index schema and exact selected content digest remain mandatory after transport.
    #[cfg(target_arch = "wasm32")]
    let reply = http.send(request_http).await;
    let reply = reply.map_err(|_| {
        Error::new(ErrorKind::Network, "The template repository could not be read; bundled and previously retrieved templates remain available")
    })?;
    if reply.status != 200 || reply.body.len() > cap {
        return Err(Error::new(ErrorKind::Network, "The template repository refused the request or exceeded its size limit; keeping the previous templates"));
    }
    match request {
        Request::Index => {
            parse_index(&reply.body)?;
            Ok(Fetched::Index(reply.body))
        }
        Request::Archive { entry } => {
            let snapshot = validate_archive(&entry, &reply.body)?;
            let (kinds, _) = crate::generation_catalog::default_registry()?;
            validate_nodes(&snapshot, &kinds)?;
            Ok(Fetched::Archive { entry, bytes: reply.body })
        }
    }
}

pub fn finish(session: &mut Session, fetched: Fetched) -> Result<(), Error> {
    match fetched {
        Fetched::Index(bytes) => catalog::install_index(session, &bytes),
        Fetched::Archive { entry, bytes } => catalog::install_archive(session, entry, bytes),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn enabled(c: &Context<'_>, _: &Value) -> Result<(), Error> {
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
    vec![
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
    ]
}
