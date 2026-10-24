//! Result caching commands (#91): `run.reuse` applies cached results to the nodes that have one,
//! and `node.cache_set` turns caching on or off for a node. See [`crate::cache`].
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use nodecraft_exec::staleness::CACHE_CONFIG;
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value as Wire};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReuseParams {
    canvas: CanvasId,
    /// The nodes to look at; all on the canvas when absent.
    #[serde(default)]
    nodes: Option<Vec<NodeId>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheParams {
    canvas: CanvasId,
    id: NodeId,
    /// `null` returns the node to its kind's default.
    enabled: Option<bool>,
}

fn reuse_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas"], "properties": {
        "canvas": {"type": "string"}, "nodes": {"type": "array", "maxItems": 10000, "items": {"type": "string"}}
    }})
}

fn cache_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "enabled"], "properties": {
        "canvas": {"type": "string"}, "id": {"type": "string"}, "enabled": {"type": ["boolean", "null"]}
    }})
}

fn always(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}

fn reuse(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: ReuseParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    if p.nodes.as_ref().is_some_and(|n| n.len() > 10_000) {
        return Err(Error::new(ErrorKind::InvalidInput, "look at 10,000 nodes at most"));
    }
    let nodes: Vec<NodeId> = p.nodes.unwrap_or_else(|| canvas.nodes().keys().copied().collect());
    let hits = crate::cache::hits(c.document, p.canvas, c.kinds, nodes, crate::cache::fresh_id)?;
    let reused: Vec<NodeId> = hits.iter().map(crate::cache::Hit::node).collect();
    if hits.is_empty() {
        return Ok(Effect::None { result: json!({"reused": reused}) });
    }
    let changes = crate::cache::changes(p.canvas, canvas, &hits)?;
    Ok(Effect::Edit { changes: Changeset::new(changes), result: json!({"reused": reused}), gesture: None })
}

fn cache_set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: CacheParams = params(value)?;
    let node = c
        .document
        .canvases()
        .get(&p.canvas)
        .and_then(|canvas| canvas.nodes().get(&p.id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    let mut changed = node.clone();
    match p.enabled {
        Some(on) => changed.config.insert(CACHE_CONFIG.into(), Wire::Bool(on)),
        None => changed.config.remove(CACHE_CONFIG),
    };
    if changed.config == node.config {
        return Ok(Effect::None { result: json!({"enabled": crate::cache::enabled(node, c.kinds)}) });
    }
    let enabled = crate::cache::enabled(&changed, c.kinds);
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas: p.canvas, node: changed }]), result: json!({"enabled": enabled}), gesture: None })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "run.reuse",
            label: "Reuse cached results",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: reuse_schema,
            params_doc: "{canvas, nodes?: [NodeId]}; gives every node with caching on whose inputs match a result already made (its own, or another node's of the same kind) that result, as one undo step. Free: nothing runs. run.quote and run.start do this themselves for the nodes they would run. Returns {reused: [NodeId]}.",
            enabled: always,
            run: reuse,
        },
        CommandSpec {
            id: "node.cache_set",
            label: "Reuse identical results",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: cache_schema,
            params_doc: "{canvas, id, enabled: bool|null}; whether the node reuses identical results; null returns it to its kind's default (on for every kind that doesn't call a model, off for model calls). Doesn't make results stale. Returns {enabled}.",
            enabled: always,
            run: cache_set,
        },
    ]
}

#[cfg(test)]
mod tests;
