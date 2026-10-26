//! Saved 3D viewer presentation (#928), independent of model/capture run inputs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset};
use nodecraft_nodes::mesh::view::{MAX_PITCH, State, supports};
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    canvas: CanvasId,
    id: NodeId,
    view: State,
}

fn set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: Params = params(value)?;
    let values = p.view.params()?;
    let node = c.document.canvases().get(&p.canvas).and_then(|canvas| canvas.nodes().get(&p.id));
    let node = node.ok_or_else(|| Error::new(ErrorKind::NotFound, "3D card does not exist"))?;
    if !supports(&node.kind) || !node.outputs.iter().any(|port| port.port_type.modality == Modality::Mesh && !port.port_type.many) {
        return Err(invalid("this card does not support a saved 3D view"));
    }
    let changes = Changeset::new(
        values
            .into_iter()
            .filter(|(name, value)| node.params.get(name) != Some(value))
            .map(|(name, value)| Change::SetParam { canvas: p.canvas, id: p.id, name, value: Some(value) })
            .collect(),
    );
    c.preview_changes(&changes)?;
    if changes.changes.is_empty() {
        return Ok(Effect::None { result: json!({"view": p.view}) });
    }
    Ok(Effect::Edit { changes, result: json!({"view": p.view}), gesture: None })
}

fn enabled(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    set(c, value).map(|_| ())
}

fn schema() -> Value {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "view"], "properties": {"canvas": uuid, "id": uuid,
            "view": {"type": "object", "additionalProperties": false, "required": ["yaw", "pitch", "zoom", "style"],
                "properties": {
                    "yaw": {"type": "number", "minimum": 0, "maximum": 360, "description": "Degrees around the model."},
                    "pitch": {"type": "number", "minimum": -MAX_PITCH, "maximum": MAX_PITCH, "description": "Degrees above the model."},
                    "zoom": {"type": "number", "minimum": 0.25, "maximum": 16},
                    "style": {"enum": ["shaded", "unlit", "wireframe"]}}}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "mesh.view_set",
        label: "Set 3D view",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id, view: {yaw, pitch, zoom, style: shaded|unlit|wireframe}} -> {view}; saves a built-in 3D source, Generate 3D or Convert 3D card's viewer as one undo step. Angles are degrees; zoom 1 fits the model. Does not change the asset, generation inputs, run identity, pricing or existing captures.",
        enabled,
        run: set,
    }]
}

#[cfg(test)]
mod tests;
