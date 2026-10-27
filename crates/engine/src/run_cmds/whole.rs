//! `run.changed` and `run.workflow` (#965): a run of the whole canvas, quoted first. Each is
//! `run.quote` in one mode (`pending`, `workflow`) under its own stable id, so the palette, the
//! global ⌘/Ctrl+Enter, the CLI and agents reach it by name. Neither starts anything: the run
//! starts only through `run.start` with the same mode and the quote's token and totals, so
//! paid work is always priced and confirmed first.
use super::{enabled_quote, run_quote};
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{CanvasId, Error};
use serde::Deserialize;
use serde_json::{Value, json};

/// The `run.start` mode a Run changed quote starts with.
pub const CHANGED_MODE: &str = "pending";
/// The `run.start` mode a Run workflow quote starts with.
pub const WORKFLOW_MODE: &str = "workflow";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WholeParams {
    canvas: CanvasId,
}

/// The `run.quote` request `value` stands for, in `mode`.
fn quote_request(value: &Value, mode: &str) -> Result<Value, Error> {
    let p: WholeParams = params(value)?;
    Ok(json!({"canvas": p.canvas, "mode": mode}))
}

fn quote(c: &Context<'_>, value: &Value, mode: &str) -> Result<Effect, Error> {
    let mut effect = run_quote(c, &quote_request(value, mode)?)?;
    // Says how to start what was quoted.
    if let Effect::None { result: Value::Object(map) } = &mut effect {
        map.insert("mode".into(), json!(mode));
    }
    Ok(effect)
}

fn run_changed(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    quote(c, value, CHANGED_MODE)
}

fn run_workflow(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    quote(c, value, WORKFLOW_MODE)
}

fn enabled_changed(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    enabled_quote(c, &quote_request(value, CHANGED_MODE)?)
}

fn enabled_workflow(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    enabled_quote(c, &quote_request(value, WORKFLOW_MODE)?)
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas"], "properties": {"canvas": {"type": "string", "format": "uuid"}}})
}

pub(super) fn specs() -> [CommandSpec; 2] {
    [
        CommandSpec {
            id: "run.changed",
            label: "Run changed",
            shortcut: Some("Command+Enter"),
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{canvas} -> run.quote's result for mode `pending`, plus {mode: \"pending\"}. Quotes every generation, action and technique node on the canvas whose result is stale, part-made or missing, with the upstream it needs, in dependency order; notes, decks, routers, batches and sources have nothing to run. Starts nothing: start it with run.start {canvas, mode: \"pending\", accept}. A quote with no steps and nothing reused means everything is up to date. Read-only.",
            enabled: enabled_changed,
            run: run_changed,
        },
        CommandSpec {
            id: "run.workflow",
            label: "Run workflow",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{canvas} -> run.quote's result for mode `workflow`, plus {mode: \"workflow\"}. Quotes every generation and technique node on the canvas, fresh or not, with everything run.changed would run; a fresh action keeps its result (it recomputes by itself when its inputs change). Starts nothing: start it with run.start {canvas, mode: \"workflow\", accept}. Read-only.",
            enabled: enabled_workflow,
            run: run_workflow,
        },
    ]
}
