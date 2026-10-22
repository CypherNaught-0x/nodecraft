//! Sharing a session live from the app (#698), through a relay (`nodecraft-cli relay`).
//!
//! - `share.start {relay?}` (human only): shares this project in a new room on `relay`, or on the
//!   relay settings name. The join link that lets others in is shown in the app, on request.
//! - `share.join {link}` (human only): opens the project a join link shares, in a new window.
//! - `share.stop`: stops sharing; the project stays open here as it is.
//! - `share.status` (read-only): whether this session is shared, and how it is going.
//!
//! The host does the work: it provides [`LIVE_SERVICE`] and connects on a thread of its own. A
//! host that can't share (the CLI, the browser for now) doesn't provide it, and the commands
//! say so.

use crate::collab::{JoinLink, MAX_RELAY_BYTES, relay_address};
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host service the `share.start`, `share.join`, `share.stop` and `share.status` commands
/// ask. Requests are `{op: "start", relay?}`, `{op: "join", link}`, `{op: "stop"}` and
/// `{op: "status"}`; the relay is checked and the link read before they reach it.
pub const LIVE_SERVICE: &str = "share.session";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartParams {
    #[serde(default)]
    relay: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinParams {
    link: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn host(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(LIVE_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "this app can't share a project live")) }
}

fn enabled_start(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    if let Some(relay) = params::<StartParams>(value)?.relay {
        relay_address(&relay)?;
    }
    host(c)
}

fn run_start(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_start(c, value)?;
    let relay = params::<StartParams>(value)?.relay.map(|relay| relay_address(&relay)).transpose()?;
    Ok(Effect::Service { capability: LIVE_SERVICE, request: json!({"op": "start", "relay": relay}) })
}

fn enabled_join(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<JoinParams>(value)?.link.parse::<JoinLink>()?;
    host(c)
}

fn run_join(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_join(c, value)?;
    let link = params::<JoinParams>(value)?.link.parse::<JoinLink>()?;
    Ok(Effect::Service { capability: LIVE_SERVICE, request: json!({"op": "join", "link": link.reveal()}) })
}

fn enabled_plain(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<NoParams>(value)?;
    host(c)
}

fn run_stop(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_plain(c, value)?;
    Ok(Effect::Service { capability: LIVE_SERVICE, request: json!({"op": "stop"}) })
}

fn run_status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_plain(c, value)?;
    Ok(Effect::Service { capability: LIVE_SERVICE, request: json!({"op": "status"}) })
}

fn schema(required: &[&str], properties: Value) -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn start_schema() -> Value {
    schema(
        &[],
        json!({"relay": {"type": ["string", "null"], "maxLength": MAX_RELAY_BYTES, "pattern": "^ws://", "description": "The relay to share through; absent for the one in settings."}}),
    )
}

fn join_schema() -> Value {
    schema(
        &["link"],
        json!({"link": {"type": "string", "maxLength": MAX_RELAY_BYTES + 80, "description": "A join link: ws://relay/#room=… or ws://relay/#link=…"}}),
    )
}

fn plain_schema() -> Value {
    schema(&[], json!({}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "share.start",
            label: "Share live",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: start_schema,
            params_doc: "{relay?} -> {sharing: true}. Shares this project live in a new room on `relay` (a ws:// address), or on `[share] relay` in settings. Others join with the join link, which the app shows on request and never returns here. Refused while this session is shared or joined already. Human only.",
            enabled: enabled_start,
            run: run_start,
        },
        CommandSpec {
            id: "share.join",
            label: "Join a shared project",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: join_schema,
            params_doc: "{link} -> {opening: true}. Opens the project a join link shares, in a new window, as its owner (#room=) or in a share link's role (#link=). Human only.",
            enabled: enabled_join,
            run: run_join,
        },
        CommandSpec {
            id: "share.stop",
            label: "Stop sharing live",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: plain_schema,
            params_doc: "{} -> {sharing: false}. Leaves the shared session; the project stays open here as it is, and others keep theirs.",
            enabled: enabled_plain,
            run: run_stop,
        },
        CommandSpec {
            id: "share.status",
            label: "Live sharing status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: plain_schema,
            params_doc: "{} -> {state: off|connecting|catching_up|waiting|live|reconnecting, role?, waiting_for_assets?}. How this session's live sharing is going. Never holds the join link. Read-only.",
            enabled: enabled_plain,
            run: run_status,
        },
    ]
}
