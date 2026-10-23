//! `node.shader_set` (#90): gives a custom shader node its WGSL and declared parameters,
//! validated first (see `nodecraft_gpu::shader`), as one undo step. Values for parameters it no
//! longer declares are dropped; the rest are kept.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset};
use nodecraft_nodes::shader::{CONFIG, FROM_CONFIG, ID, Pin, StoredParam, StoredShader, config_value, pin_value};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShaderParams {
    canvas: CanvasId,
    id: NodeId,
    source: String,
    #[serde(default)]
    params: Vec<StoredParam>,
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "source"], "properties": {
        "canvas": {"type": "string"}, "id": {"type": "string"},
        "source": {"type": "string", "maxLength": nodecraft_gpu::shader::MAX_SOURCE, "description": "WGSL defining fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>; each parameter reads as name(); pixel(x, y) and size() are provided; no loops, bindings or globals."},
        "params": {"type": "array", "maxItems": nodecraft_gpu::shader::MAX_PARAMS, "items": {"type": "object", "additionalProperties": false, "required": ["name", "label", "min", "max", "default"], "properties": {
            "name": {"type": "string"}, "label": {"type": "string"}, "min": {"type": "number"}, "max": {"type": "number"}, "default": {"type": "number"}}}}
    }})
}

/// The gpu crate's form of `shader`.
pub(crate) fn spec(shader: &StoredShader) -> nodecraft_gpu::shader::ShaderSpec {
    nodecraft_gpu::shader::ShaderSpec {
        source: shader.source.clone(),
        params: shader
            .params
            .iter()
            .map(|p| nodecraft_gpu::shader::ShaderParam {
                name: p.name.clone(),
                label: p.label.clone(),
                min: p.min as f32,
                max: p.max as f32,
                default: p.default as f32,
            })
            .collect(),
    }
}

fn always(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}

fn set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: ShaderParams = params(value)?;
    set_shader(c, p.canvas, p.id, StoredShader { source: p.source, params: p.params }, None)
}

/// Validates `shader` and stores it on the custom shader node `id`, as one undo step, with the
/// library version it came from when `from` names one. Without one, the version it came from is
/// kept: the card shows it was edited since.
pub(crate) fn set_shader(c: &Context<'_>, canvas: CanvasId, id: NodeId, shader: StoredShader, from: Option<&Pin>) -> Result<Effect, Error> {
    let node = c
        .document
        .canvases()
        .get(&canvas)
        .and_then(|canvas| canvas.nodes().get(&id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    if node.kind != ID {
        return Err(Error::new(ErrorKind::InvalidInput, "only a custom shader node takes a shader"));
    }
    // The person's code is checked before it is stored; the message is theirs to act on.
    nodecraft_gpu::shader::validate(&spec(&shader))
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the shader isn't valid: {}", e.0.chars().take(4000).collect::<String>())))?;
    let mut changed = node.clone();
    changed.config.insert(CONFIG.into(), config_value(&shader)?);
    if let Some(from) = from {
        changed.config.insert(FROM_CONFIG.into(), pin_value(from)?);
    }
    // Kept values move into their new range; ones for undeclared parameters go.
    changed.params.retain(|name, _| shader.params.iter().any(|d| d.name == *name));
    for declared in &shader.params {
        if let Some(nodecraft_types::Value::Number(n)) = changed.params.get(&declared.name) {
            let clamped = n.get().clamp(declared.min, declared.max);
            changed.params.insert(declared.name.clone(), nodecraft_types::Value::Number(nodecraft_types::Number::new(clamped)?));
        }
    }
    if changed == *node {
        return Ok(Effect::None { result: json!({"changed": false}) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas, node: changed }]), result: json!({"changed": true}), gesture: None })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.shader_set",
        label: "Set custom shader",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id, source, params?: [{name, label, min, max, default}]}; a custom shader node's WGSL and float parameters, validated (one effect function, no loops, bindings or globals). Returns {changed}.",
        enabled: always,
        run: set,
    }]
}

#[cfg(test)]
mod tests;
