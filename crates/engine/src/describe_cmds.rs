//! `node.describe` (#162): a node kind's descriptor contract, or a stored node's contract with
//! its resolved parameters and the settings its active result was made with, so the CLI, MCP and
//! agents see what the inspector sees. Read-only.
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DescribeParams {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    canvas: Option<CanvasId>,
    #[serde(default)]
    id: Option<NodeId>,
}

enum Target<'a> {
    Kind(&'a str),
    Node(&'a nodecraft_graph::Node),
}

fn target<'a>(c: &'a Context<'_>, p: &'a DescribeParams) -> Result<Target<'a>, Error> {
    match (&p.kind, p.canvas, p.id) {
        (Some(kind), None, None) => {
            c.kinds.get(kind)?;
            Ok(Target::Kind(kind))
        }
        (None, Some(canvas), Some(id)) => {
            let canvas = c.document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
            canvas.nodes().get(&id).map(Target::Node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))
        }
        _ => Err(Error::new(ErrorKind::InvalidInput, "describe either a kind, or a node by canvas and id")),
    }
}

fn describe(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: DescribeParams = params(value)?;
    let target = target(c, &p)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let result = match target {
        Target::Kind(kind) => json!({"contract": c.kinds.contract(kind)?}),
        Target::Node(node) => {
            let contract = c.kinds.get(&node.kind)?.instance_contract(node)?;
            let resolution = contract.resolve_with(&node.params, true, crate::option_cmds::lookup(c.resources))?;
            // What the active result was made with, when a provider made it.
            let settings = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)).map_or(&[][..], |g| g.settings.as_slice());
            json!({"contract": contract, "resolution": resolution, "settings": settings})
        }
    };
    Ok(Effect::None { result })
}

fn enabled_describe(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: DescribeParams = params(value)?;
    target(c, &p).map(|_| ())
}

fn describe_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [],
        "properties": {"kind": {"type": "string"}, "canvas": {"type": "string", "format": "uuid"}, "id": {"type": "string", "format": "uuid"}},
        "oneOf": [{"required": ["kind"]}, {"required": ["canvas", "id"]}]})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.describe",
        label: "Describe node",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: describe_schema,
        params_doc: "{kind: string} | {canvas: CanvasId, id: NodeId} -> {contract, resolution?, settings?}; the descriptor contract (ports with roles and wire bounds, modes, parameters with rules and checks; docs/descriptor-contract.md) of a kind, or of a stored node plus its resolved parameters (effective value and source, visible, enabled, issues) and the settings each provider job of its active result was submitted with (provider, instance, model, params, inputs, item; empty when none were recorded; can be as large as the node). Read-only.",
        enabled: enabled_describe,
        run: describe,
    }]
}

#[cfg(test)]
mod tests;
