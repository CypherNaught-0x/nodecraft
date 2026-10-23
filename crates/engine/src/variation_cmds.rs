//! `node.variations_set` (#81): how many variations one run of a generation node makes. See
//! [`nodecraft_exec::variations`].
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_exec::variations::{MAX_VARIATIONS, VARIATIONS_CONFIG, requested};
use nodecraft_graph::{Change, Changeset};
use nodecraft_nodes::Category;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Unsigned, Value as Wire};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VariationsParams {
    canvas: CanvasId,
    id: NodeId,
    /// `null` or absent returns the node to one result a run.
    count: Option<u64>,
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id"], "properties": {
        "canvas": {"type": "string"}, "id": {"type": "string"}, "count": {"type": ["integer", "null"], "minimum": 1, "maximum": MAX_VARIATIONS}
    }})
}

fn always(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}

fn variations_set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: VariationsParams = params(value)?;
    let node = c
        .document
        .canvases()
        .get(&p.canvas)
        .and_then(|canvas| canvas.nodes().get(&p.id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    if !c.kinds.get(&node.kind).is_ok_and(|k| k.descriptor().category == Category::Generation) {
        return Err(Error::new(ErrorKind::InvalidInput, "only a generation node makes variations"));
    }
    let mut changed = node.clone();
    match p.count {
        Some(n) if n == 0 || n > MAX_VARIATIONS as u64 => return Err(Error::new(ErrorKind::InvalidInput, format!("make 1 to {MAX_VARIATIONS} variations"))),
        Some(n) if n > 1 => changed.config.insert(VARIATIONS_CONFIG.into(), Wire::Unsigned(Unsigned(n))),
        // One is the default: nothing is stored for it.
        _ => changed.config.remove(VARIATIONS_CONFIG),
    };
    let count = requested(&changed);
    if changed.config == node.config {
        return Ok(Effect::None { result: json!({"count": count}) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas: p.canvas, node: changed }]), result: json!({"count": count}), gesture: None })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.variations_set",
        label: "Set variations",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id, count?: 1..20|null}; how many variations one run of a generation node makes, each its own paid request (quoted as such) recorded as a result of its own; null means one. Doesn't make results stale. Returns {count}.",
        enabled: always,
        run: variations_set,
    }]
}

#[cfg(test)]
mod tests;
