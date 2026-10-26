//! `history.export` (#557): writes one media result's bytes to a file the caller names. The
//! command never opens a dialog; frontends ask where first, then dispatch it with the path.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportParams {
    canvas: CanvasId,
    id: NodeId,
    generation: GenerationId,
    /// The output port to export; the node's first output when omitted.
    #[serde(default)]
    port: Option<String>,
    /// Which item of a list output to export.
    #[serde(default)]
    item: Option<usize>,
    path: PathBuf,
    /// Replace a file already at `path`; without it the export refuses to.
    #[serde(default)]
    overwrite: bool,
    /// The UI opts in; headless callers retain the synchronous result by default.
    #[serde(default)]
    background: bool,
}

fn export(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExportParams = params(value)?;
    crate::export::check(&p.path)?;
    let (value, _) = crate::history_cmds::result_value(c, p.canvas, p.id, p.generation, p.port.as_deref(), p.item)?;
    let Value::Media(asset) = value else {
        return Err(Error::new(ErrorKind::Unsupported, "only image, video, audio, 3D and document results export to a file; copy text instead"));
    };
    // Availability/admission must not authenticate an indexed body on the UI thread. The
    // writer verifies the exact reference before creating output.
    if c.assets.descriptor(&asset.hash).is_none() {
        return Err(Error::new(ErrorKind::NotFound, "the result's retained asset is unavailable"));
    }
    Ok(Effect::Export { what: crate::export::Exported::Asset(asset.clone()), path: p.path, overwrite: p.overwrite, background: p.background })
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    export(c, value).map(|_| ())
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "generation", "path"],
        "properties": {"canvas": uuid, "id": uuid, "generation": uuid, "port": {"type": "string", "maxLength": 256},
            "item": {"type": "integer", "minimum": 0},
            "path": {"type": "string", "minLength": 1, "maxLength": crate::export::MAX_PATH_BYTES},
            "overwrite": {"type": "boolean"}, "background": {"type":"boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "history.export",
            label: "Export result…",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{canvas, id: NodeId, generation, port?: output name, item?: list index, path: absolute file path, overwrite?: bool, background?: bool} -> {bytes} or {pending: id}; background requires an export runner and verifies/copies off the UI thread. export.status retains the terminal result; export.cancel accepts cancellation before finishing atomic publication. Defaults are synchronous and no replacement. Desktop only.",
            enabled,
            run: export,
        },
        CommandSpec {
            id: "export.status",
            label: "Export status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"required":[],"properties":{}}),
            params_doc: "{} -> {export: null | {id, phase, cancellable, result}}; path-free status of the latest background export; result.state is exported (bytes), cancelled or failed (error).",
            enabled: |_, value| {
                let _: Empty = params(value)?;
                Ok(())
            },
            run: |c, value| {
                let _: Empty = params(value)?;
                Ok(Effect::None { result: json!({"export":c.exports.status()}) })
            },
        },
        CommandSpec {
            id: "export.cancel",
            label: "Cancel export",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"required":["id"],"properties":{"id":{"type":"integer","minimum":1}}}),
            params_doc: "{id: pending export id} -> {cancelling:true}; accepted cancellation leaves no destination file and preserves any existing target. Finishing atomic publication cannot be cancelled.",
            enabled: |c, value| {
                let p: Cancel = params(value)?;
                c.exports.cancellable(p.id)
            },
            run: |_, value| {
                let p: Cancel = params(value)?;
                Ok(Effect::CancelExport { id: p.id })
            },
        },
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel {
    id: u64,
}

#[cfg(test)]
mod tests;
