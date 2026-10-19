//! Performance capture commands (#151): start, stop and clear the session's recorder, and read
//! its report. They change no document and add no undo step; the report holds no document
//! contents (see `perf`).
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::Error;
use serde::Deserialize;
use serde_json::{Value, json};

/// What a capture command asks the session to do with its recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfAction {
    Start,
    Stop,
    Clear,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn empty(value: &Value) -> Result<(), Error> {
    if value.is_null() {
        return Ok(());
    }
    params::<Empty>(value).map(|_| ())
}

fn start(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    empty(value)?;
    Ok(Effect::Perf { action: PerfAction::Start })
}
fn stop(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    empty(value)?;
    Ok(Effect::Perf { action: PerfAction::Stop })
}
fn clear(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    empty(value)?;
    Ok(Effect::Perf { action: PerfAction::Clear })
}
fn report(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    empty(value)?;
    // Availability checks (the palette's, every frame) don't build the report.
    if c.phase == crate::commands::Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let result = serde_json::to_value(c.perf.report()).map_err(|_| Error::new(nodecraft_types::ErrorKind::Internal, "the report couldn't be built"))?;
    Ok(Effect::None { result })
}
fn enabled_any(_: &Context<'_>, value: &Value) -> Result<(), Error> {
    empty(value)
}
fn enabled_start(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    empty(value)?;
    if c.perf.enabled() { Err(Error::new(nodecraft_types::ErrorKind::Conflict, "performance capture is already running")) } else { Ok(()) }
}
fn enabled_stop(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    empty(value)?;
    if c.perf.enabled() { Ok(()) } else { Err(Error::new(nodecraft_types::ErrorKind::Conflict, "performance capture isn't running")) }
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "properties": {}, "required": []})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "perf.start",
            label: "Start performance capture",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{} -> {recording: true}; starts recording frame and command timings (bounded; nothing leaves the machine).",
            enabled: enabled_start,
            run: start,
        },
        CommandSpec {
            id: "perf.stop",
            label: "Stop performance capture",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{} -> {recording: false}; stops recording and keeps what was recorded.",
            enabled: enabled_stop,
            run: stop,
        },
        CommandSpec {
            id: "perf.clear",
            label: "Clear performance capture",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{} -> {cleared: true}; drops every recorded sample (e.g. after warming up).",
            enabled: enabled_any,
            run: clear,
        },
        CommandSpec {
            id: "perf.report",
            label: "Performance report",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{} -> the versioned report (schema nodecraft.perf/1): per metric p50/p95/p99/max in microseconds, sample counts, frames over the 60 Hz budget, the last frame's workload and build/platform labels. Unmeasured values are null. Read-only.",
            enabled: enabled_any,
            run: report,
        },
    ]
}

/// The result a capture command returns once the session has applied it.
pub(crate) fn applied(action: PerfAction, recording: bool) -> Value {
    match action {
        PerfAction::Start | PerfAction::Stop => json!({"recording": recording}),
        PerfAction::Clear => json!({"cleared": true}),
    }
}
