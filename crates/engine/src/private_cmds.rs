//! Private canvases (#121): `canvas.set_private {canvas, private}` keeps a canvas to this
//! session when the project is shared live, or shares it again. A private canvas never goes
//! into the shared document, nor do assets only it uses; making a shared canvas private takes
//! it away from everyone else. The flag is saved with the project, so it stays private when
//! shared again.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{CanvasId, Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPrivate {
    canvas: CanvasId,
    private: bool,
}

fn set_private(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    let p: SetPrivate = params(v)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no canvas has that id"))?;
    let result = json!({"canvas": p.canvas, "private": p.private});
    if canvas.is_private() == p.private {
        return Ok(Effect::None { result });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetCanvasPrivate { canvas: p.canvas, private: p.private }]), result, gesture: None })
}

fn enabled(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    if let Effect::Edit { changes, .. } = set_private(c, v)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "private"],
        "properties": {"canvas": {"type": "string", "format": "uuid"}, "private": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "canvas.set_private",
        label: "Make canvas private",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, private} -> {canvas, private}. A private canvas stays with this session when the project is shared live: nobody in the room sees it or the media only it uses. Making a shared canvas private takes it away from the others; making it shared again gives it back. Only a role that edits may change it.",
        enabled,
        run: set_private,
    }]
}
