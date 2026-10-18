//! Input order (#64): the noodles into a list input are read in their `order`; `edge.reorder`
//! sets that order from a list of the input's edges, as one undo step.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Endpoint};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReorderParams {
    canvas: CanvasId,
    /// The list input.
    to: Endpoint,
    /// Every edge into it, first to last.
    edges: Vec<EdgeId>,
}

fn reorder(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: ReorderParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let node = canvas.nodes().get(&p.to.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    let port = node.inputs.iter().find(|port| port.name == p.to.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such input"))?;
    if !port.port_type.many {
        return Err(invalid("only a list input has an order"));
    }
    let mut current: Vec<_> = canvas.edges().values().filter(|edge| edge.to == p.to).collect();
    current.sort_by_key(|edge| edge.order);
    let given: BTreeSet<EdgeId> = p.edges.iter().copied().collect();
    if given.len() != p.edges.len() || given != current.iter().map(|edge| edge.id).collect() {
        return Err(Error::new(ErrorKind::Conflict, "list every connection into this input once"));
    }
    if current.iter().map(|edge| edge.id).eq(p.edges.iter().copied()) {
        return Err(Error::new(ErrorKind::Conflict, "the connections are already in that order"));
    }
    let mut changes = Vec::with_capacity(p.edges.len());
    for (order, id) in p.edges.iter().enumerate() {
        let order = u32::try_from(order).map_err(|_| invalid("too many connections"))?;
        if let Some(edge) = canvas.edges().get(id).filter(|edge| edge.order != order) {
            changes.push(Change::SetEdge { canvas: p.canvas, edge: nodecraft_graph::Edge { order, ..edge.clone() } });
        }
    }
    Ok(Effect::Edit { changes: Changeset::new(changes), result: json!({"edges": p.edges}), gesture: None })
}

fn enabled_reorder(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    c.editable()?;
    if let Effect::Edit { changes, .. } = reorder(c, value)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}

fn reorder_schema() -> Value {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "to", "edges"],
        "properties": {"canvas": uuid, "edges": {"type": "array", "items": uuid, "maxItems": 4096},
            "to": {"type": "object", "required": ["node", "port"], "additionalProperties": false,
                "properties": {"node": uuid, "port": {"type": "string", "maxLength": 256}}}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "edge.reorder",
        label: "Reorder inputs",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: reorder_schema,
        params_doc: "{canvas, to: {node, port}, edges: [EdgeId]} -> {edges}; sets the order of the connections into a list input, first to last. List every connection into it once; the current order is refused as a no-op. One undo step.",
        enabled: enabled_reorder,
        run: reorder,
    }]
}

#[cfg(test)]
mod tests;
