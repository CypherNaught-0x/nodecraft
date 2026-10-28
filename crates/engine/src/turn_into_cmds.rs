//! "Turn into" (#60): drop a noodle on empty canvas and pick a kind; the new node and its
//! connection arrive together, as one undo step. [`kinds_accepting`] is what the picker offers,
//! and `node.create_connected` is the command it (or an agent) runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Edge, Endpoint, Node, Port, Position};
use nodecraft_nodes::{Category, NodeDescriptor, Registry, batch, router};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamValues, PortType, Value};
use serde::{Deserialize, Deserializer};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

mod feeding;
pub use feeding::{instance_feeding, kinds_feeding};
pub use nodecraft_exec::routing::output_type;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

/// Whether an output of type `output` may feed an input of type `input` (the graph's rule: the
/// same modality, and a list only into a list).
pub fn compatible(output: PortType, input: PortType) -> bool {
    output.modality == input.modality && (!output.many || input.many)
}

/// The config a new node of `kind` gets when it is created to take `output`: a router or a
/// batch carries the noodle's modality; every other kind starts from its defaults.
fn config_for(kind: &str, output: PortType) -> BTreeMap<String, Value> {
    match kind {
        router::ID => router::config(output.modality),
        batch::ID => batch::config(output.modality),
        _ => BTreeMap::new(),
    }
}

/// A new node of `kind` set up to take `output`, and the input it takes it on.
pub fn instance_for(kinds: &Registry, kind: &str, id: NodeId, output: PortType) -> Result<(Node, String), Error> {
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
    /// Exactly one existing end: an output to consume, or an input to feed.
    #[serde(default, deserialize_with = "endpoint")]
    from: Option<Endpoint>,
    #[serde(default, deserialize_with = "endpoint")]
    to: Option<Endpoint>,
    /// The new connection's id.
    edge: EdgeId,
}

fn endpoint<'de, D: Deserializer<'de>>(value: D) -> Result<Option<Endpoint>, D::Error> {
    Endpoint::deserialize(value).map(Some)
}

fn create_connected(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CreateConnectedParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    if p.from.is_some() == p.to.is_some() {
        return Err(invalid("provide exactly one existing output `from` or input `to`"));
    }
    if p.to.is_some() {
        return feeding::create_connected(c, p);
    }
    let from = p.from.ok_or_else(|| invalid("an existing output is required"))?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&from.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "the source node does not exist"))?;
    let _output: &Port =
        source.outputs.iter().find(|port| port.name == from.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "the source node has no such output"))?;
    let output_type = output_type(canvas, c.kinds, &from)?;
    let (mut node, input) = instance_for(c.kinds, &p.kind, p.id, output_type)?;
    node.pos = p.pos;
    let edge = Edge { id: p.edge, from, to: Endpoint { node: p.id, port: input.clone() }, order: 0 };
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
    let endpoint = json!({"type": "object", "required": ["node", "port"], "additionalProperties": false,
        "properties": {"node": uuid, "port": {"type": "string", "maxLength": 256}}});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "kind", "pos", "edge"],
        "oneOf": [{"required": ["from"]}, {"required": ["to"]}],
        "properties": {"canvas": uuid, "id": uuid, "kind": {"type": "string", "maxLength": 128}, "edge": uuid,
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "from": endpoint, "to": endpoint}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.create_connected",
        label: "Turn into",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: create_connected_schema,
        params_doc: "{canvas, id: new NodeId, kind, pos: {x, y}, edge: new EdgeId, from?: {node, port}, to?: {node, port}}; exactly one of from/to. `from` creates a node consuming an existing output and returns {id, input}; `to` creates a node feeding an existing input and returns {id, output}. Uses the first compatible port, as one undo step. A router or batch carries the connection's modality. A fed single input is refused; a list input appends. The caller supplies fresh ids.",
        enabled: enabled_create_connected,
        run: create_connected,
    }]
}

#[cfg(test)]
mod feeding_tests;
#[cfg(test)]
mod tests;
