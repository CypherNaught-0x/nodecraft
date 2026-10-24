//! Paid results a run couldn't keep (#369), held by the run host until someone decides.
//!
//! - `run.held` (read-only): the held results, oldest first, with why each couldn't be kept.
//! - `run.held_retry {id}`: records a held result on its card again, as its step would have; for a
//!   result whose step was its alone.
//! - `run.held_place {id}`: puts its outputs on its canvas as source nodes, in order, as one undo
//!   step.
//! - `run.held_discard {id}` (human only): lets it go for good.
//!
//! None of them calls the provider or charges again. Retry and place are carried out by the host
//! on its next frame; the result stays held until they work, and a failure is reported on the
//! card. Host contract: provide [`HELD_SERVICE`] (the desktop's
//! [`crate::run_host::RunHost::register`] does).

use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host service that keeps held results.
pub const HELD_SERVICE: &str = "run.held_results";
/// Most results held at once before new jobs are refused.
pub const MAX_HELD: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdParams {
    id: u64,
}

fn host(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(HELD_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "this host holds no run results")) }
}

fn run_list(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_list(c, value)?;
    Ok(Effect::Service { capability: HELD_SERVICE, request: json!({"op": "list"}) })
}

fn enabled_list(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<ListParams>(value)?;
    host(c)
}

fn enabled_id(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<IdParams>(value)?;
    host(c)
}

fn request(c: &Context<'_>, value: &Value, op: &str) -> Result<Effect, Error> {
    let p = params::<IdParams>(value)?;
    host(c)?;
    Ok(Effect::Service { capability: HELD_SERVICE, request: json!({"op": op, "id": p.id}) })
}

fn run_retry(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    request(c, value, "retry")
}

fn run_place(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    request(c, value, "place")
}

fn run_discard(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    request(c, value, "discard")
}

fn list_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

fn id_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["id"],
        "properties": {"id": {"type": "integer", "minimum": 0}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "run.held",
            label: "List held run results",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {held: [{id, canvas, node, provider, model, reason, outputs, retry, pending, saved}], max}. Paid results a run couldn't keep (they couldn't be converted, fitted to the node's ports or recorded), oldest first. `retry` says whether run.held_retry can record it; `pending` is retry|place while the host carries that out. While `max` are held, new jobs are refused before they are sent. `saved` says whether it survives a restart: the desktop saves held results, except one too large to keep or that couldn't be written. Read-only.",
            enabled: enabled_list,
            run: run_list,
        },
        CommandSpec {
            id: "run.held_retry",
            label: "Retry recording a held result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: id_schema,
            params_doc: "{id} -> {id, pending: \"retry\"}. Records a held result on its card again, as its step would have, without calling the provider. Only for a result whose step was its alone (`retry` in run.held). The host does it on its next frame; the result stays held until it works, and a failure is reported on the card.",
            enabled: enabled_id,
            run: run_retry,
        },
        CommandSpec {
            id: "run.held_place",
            label: "Put a held result on the canvas",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: id_schema,
            params_doc: "{id} -> {id, pending: \"place\"}. Puts a held result's outputs on its canvas as source nodes, in order, to the right of its node, as one undo step, without calling the provider. The host does it on its next frame; the result stays held until it works.",
            enabled: enabled_id,
            run: run_place,
        },
        CommandSpec {
            id: "run.held_discard",
            label: "Discard a held result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: id_schema,
            params_doc: "{id} -> {id, discarded: true}. Lets a held result go for good; its charge stands. Refused while it is being retried or placed. Human only.",
            enabled: enabled_id,
            run: run_discard,
        },
    ]
}
