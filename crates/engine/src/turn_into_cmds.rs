//! "Turn into" (#60): drop a noodle on empty canvas and pick a kind; the new node and its
//! connection arrive together, as one undo step. [`kinds_accepting`] is what the picker offers,
//! and `node.create_connected` is the command it (or an agent) runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Edge, Endpoint, Node, Port, Position};
use nodecraft_nodes::{Category, NodeDescriptor, Registry, router};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamValues, PortType, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

/// Whether an output of type `output` may feed an input of type `input` (the graph's rule: the
/// same modality, and a list only into a list).
pub fn compatible(output: PortType, input: PortType) -> bool {
    output.modality == input.modality && (!output.many || input.many)
}

/// The config a new node of `kind` gets when it is created to take `output`: a router carries
/// the noodle's modality; every other kind starts from its defaults.
fn config_for(kind: &str, output: PortType) -> BTreeMap<String, Value> {
    if kind == router::ID { router::config(output.modality) } else { BTreeMap::new() }
}

/// A new node of `kind` set up to take `output`, and the input it takes it on.
fn instance_for(kinds: &Registry, kind: &str, id: NodeId, output: PortType) -> Result<(Node, String), Error> {
    let node = kinds.get(kind)?.instantiate_instance(id, config_for(kind, output), &ParamValues::new())?;
    let input = node.inputs.iter().find(|input| compatible(output, input.port_type)).map(|input| input.name.clone());
    match input {
        Some(input) => Ok((node, input)),
        None => Err(Error::new(ErrorKind::Conflict, "that kind of node has no input for this connection")),
    }
}

/// Kinds a noodle carrying `output` can turn into, in registry order: those whose mode accepts
/// it (the planner's predicate, per instance for kinds whose config changes it) and that, once
/// created, have an input for it. Sources take no inputs, so they never appear. It instantiates
/// each candidate, so callers compute it once per menu, not per frame.
pub fn kinds_accepting(kinds: &Registry, output: PortType) -> Vec<&NodeDescriptor> {
    let probe = NodeId::from_bytes([0; 16]);
    let accepts = |descriptor: &NodeDescriptor| {
        let Ok((node, _)) = instance_for(kinds, &descriptor.id, probe, output) else {
            return false;
        };
        kinds.get(&descriptor.id).is_ok_and(|kind| matches!(kind.instance_mode(&node, &[output]), Ok(Some(_))))
    };
    kinds.compatible(&[output]).filter(|descriptor| descriptor.category != Category::Source).filter(|descriptor| accepts(descriptor)).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateConnectedParams {
    canvas: CanvasId,
    /// The new node's id; the caller supplies it so replay stays deterministic.
    id: NodeId,
    kind: String,
    pos: Position,
    /// The output the noodle comes from.
    from: Endpoint,
    /// The new connection's id.
    edge: EdgeId,
}

fn create_connected(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CreateConnectedParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&p.from.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "the source node does not exist"))?;
    let output: &Port =
        source.outputs.iter().find(|port| port.name == p.from.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "the source node has no such output"))?;
    let (mut node, input) = instance_for(c.kinds, &p.kind, p.id, output.port_type)?;
    node.pos = p.pos;
    let edge = Edge { id: p.edge, from: p.from, to: Endpoint { node: p.id, port: input.clone() }, order: 0 };
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }, Change::Connect { canvas: p.canvas, edge }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "input": input}), gesture: None })
}

fn enabled_create_connected(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    create_connected(c, value).map(|_| ())
}

fn create_connected_schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "kind", "pos", "from", "edge"],
        "properties": {"canvas": uuid, "id": uuid, "kind": {"type": "string", "maxLength": 128}, "edge": uuid,
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "from": {"type": "object", "required": ["node", "port"], "additionalProperties": false,
                "properties": {"node": uuid, "port": {"type": "string", "maxLength": 256}}}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.create_connected",
        label: "Turn into",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: create_connected_schema,
        params_doc: "{canvas, id: new NodeId, kind, pos: {x, y}, from: {node, port}, edge: new EdgeId} -> {id, input}; adds a node of `kind` and connects the output `from` to its first input that accepts it, as one undo step. A router takes the output's modality. The caller supplies the new ids.",
        enabled: enabled_create_connected,
        run: create_connected,
    }]
}

#[cfg(test)]
mod tests;
