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
}

fn export(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExportParams = params(value)?;
    crate::export::check(&p.path)?;
    let (value, _) = crate::history_cmds::result_value(c, p.canvas, p.id, p.generation, p.port.as_deref(), p.item)?;
    let Value::Media(asset) = value else {
        return Err(Error::new(ErrorKind::Unsupported, "only image, video, audio, 3D and document results export to a file; copy text instead"));
    };
    c.validate_asset(asset)?;
    Ok(Effect::Export { what: crate::export::Exported::Asset(asset.clone()), path: p.path, overwrite: p.overwrite })
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
            "overwrite": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "history.export",
        label: "Export result…",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id: NodeId, generation, port?: output name (default the first), item?: list index (as history.extract), path: absolute file path, overwrite?: bool (default false)} -> {bytes}; writes a media result's bytes to `path` on this machine. Refuses a file that's already there unless `overwrite`, a missing folder, and a full disk, leaving nothing half written. Desktop only.",
        enabled,
        run: export,
    }]
}

#[cfg(test)]
mod tests;
