//! Technique nodes on a canvas (#94): place a published version as one node, upgrade a node to
//! another version of its technique, or detach it into the version's own nodes
//! (`docs/decisions/2026-10-20-technique-nodes.md`). A placed node pins its version's interface
//! ([`nodecraft_nodes::technique`]); runs inline the version for planning only
//! ([`nodecraft_exec::technique`]).
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::technique_cmds::derive;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Position, Technique};
use nodecraft_nodes::technique::{self, Interface};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamSchema, ParamValues};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

/// The canvas extent positions may use (as `node.create`).
const MAX_COORDINATE: f32 = 10_000_000.0;

fn canvas<'a>(c: &'a Context<'_>, id: CanvasId) -> Result<&'a Canvas, Error> {
    c.document.canvases().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))
}

/// A published version that can be placed now, and the interface a node placed now pins.
fn placeable<'a>(c: &'a Context<'_>, id: CanvasId) -> Result<(&'a Canvas, &'a Technique, Interface), Error> {
    let version = canvas(c, id)?;
    let info = version.technique_info().ok_or_else(|| invalid("that canvas isn't a published technique"))?;
    Ok((version, info, technique::interface_of(version, c.kinds)?))
}

/// A technique node on `canvas`, and the interface it pins.
fn instance<'a>(c: &'a Context<'_>, canvas_id: CanvasId, id: NodeId) -> Result<(&'a Canvas, &'a Node, Interface), Error> {
    let canvas = canvas(c, canvas_id)?;
    let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if node.kind != technique::ID {
        return Err(invalid("that node isn't a technique"));
    }
    Ok((canvas, node, technique::interface(node)?))
}

// ---- technique.place ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceParams {
    canvas: CanvasId,
    /// The canvas holding the version to place.
    version: CanvasId,
    id: NodeId,
    pos: Position,
}

fn place(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PlaceParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    canvas(c, p.canvas)?;
    let (_, info, interface) = placeable(c, p.version)?;
    let mut node = c.kinds.get(technique::ID)?.instantiate_instance(p.id, interface.config()?, &ParamValues::new())?;
    node.pos = p.pos;
    node.title = info.name.clone();
    let result = json!({"id": p.id, "technique": info.id, "version": info.version});
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- technique.upgrade ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpgradeParams {
    canvas: CanvasId,
    node: NodeId,
    /// The canvas holding the version to switch to: any version of the same technique.
    version: CanvasId,
}

fn upgrade(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpgradeParams = params(value)?;
    let (canvas, node, current) = instance(c, p.canvas, p.node)?;
    let (_, info, interface) = placeable(c, p.version)?;
    if info.id != current.technique {
        return Err(invalid("that version belongs to another technique"));
    }
    // Values for parameters the new version still has, by name; the rest take its defaults.
    let schema = ParamSchema { params: interface.params.clone() };
    // A value the new version's control refuses is dropped too.
    let fits_param = |name: &String, value: &nodecraft_types::Value| {
        schema.params.iter().any(|spec| &spec.name == name) && schema.resolve(&[(name.clone(), value.clone())].into_iter().collect(), false).is_ok()
    };
    let kept: ParamValues = node.params.iter().filter(|(name, value)| fits_param(name, value)).map(|(n, v)| (n.clone(), v.clone())).collect();
    let dropped: Vec<String> = node.params.keys().filter(|name| !kept.contains_key(*name)).cloned().collect();
    let mut upgraded = c.kinds.get(technique::ID)?.instantiate_instance(node.id, interface.config()?, &kept)?;
    upgraded.title = node.title.clone();
    upgraded.pos = node.pos;
    upgraded.history = node.history.clone();
    upgraded.active = node.active;
    upgraded.tag = node.tag;
    upgraded.field = node.field;
    // Wires to a port the new version no longer has, or of another type, are taken off.
    let fits = |ports: &[nodecraft_graph::Port], port: &str, old: &[nodecraft_graph::Port]| {
        let now = ports.iter().find(|p| p.name == port).map(|p| p.port_type);
        now.is_some() && now == old.iter().find(|p| p.name == port).map(|p| p.port_type)
    };
    let mut changes = Vec::new();
    for edge in canvas.edges().values() {
        let keep = if edge.to.node == node.id {
            fits(&upgraded.inputs, &edge.to.port, &node.inputs)
        } else if edge.from.node == node.id {
            fits(&upgraded.outputs, &edge.from.port, &node.outputs)
        } else {
            true
        };
        if !keep {
            changes.push(Change::Disconnect { canvas: p.canvas, id: edge.id });
        }
    }
    let result = json!({"node": node.id, "technique": info.id, "version": info.version, "dropped_params": dropped, "disconnected": changes.len()});
    changes.push(Change::SetNode { canvas: p.canvas, node: upgraded });
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- technique.detach ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachParams {
    canvas: CanvasId,
    node: NodeId,
    seed: NodeId,
}

fn detach(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: DetachParams = params(value)?;
    let (canvas, node, pinned) = instance(c, p.canvas, p.node)?;
    let (version, info, now) = placeable(c, pinned.canvas)?;
    if info.id != pinned.technique || info.version != pinned.version {
        return Err(invalid("this node's technique version isn't in this project"));
    }
    if !now.matches(&pinned) {
        return Err(invalid("this node's technique version no longer matches it; upgrade it first"));
    }
    let ids: BTreeMap<NodeId, NodeId> = version.nodes().keys().enumerate().map(|(n, id)| (*id, NodeId::from_bytes(derive(p.seed, "detach node", n)))).collect();
    let map = |id: NodeId| ids.get(&id).copied().ok_or_else(|| Error::new(ErrorKind::Internal, "a version node has no copy"));

    // What feeds each exposed input, in input order; and where each output's wires go.
    let mut feeds: BTreeMap<(NodeId, &str), Vec<(u32, Endpoint)>> = BTreeMap::new();
    let mut leaving = Vec::new();
    for edge in canvas.edges().values() {
        if edge.to.node == node.id {
            if let Some(exposed) = pinned.inputs.iter().position(|input| input.port == edge.to.port).and_then(|n| info.inputs.get(n)) {
                feeds.entry((exposed.node, exposed.port.as_str())).or_default().push((edge.order, edge.from.clone()));
            }
        } else if edge.from.node == node.id
            && let Some(exposed) = pinned.outputs.iter().position(|output| output.port == edge.from.port).and_then(|n| info.outputs.get(n))
        {
            leaving.push(Edge { id: edge.id, from: Endpoint { node: map(exposed.node)?, port: exposed.port.clone() }, to: edge.to.clone(), order: edge.order });
        }
    }
    for list in feeds.values_mut() {
        list.sort_by_key(|(order, _)| *order);
    }
    // An input node whose every wire leaves a fed port is replaced by what feeds it, unless it is
    // an output too.
    let replaced: BTreeSet<NodeId> = info
        .inputs
        .iter()
        .map(|input| input.node)
        .filter(|id| !info.outputs.iter().any(|output| output.node == *id))
        .filter(|id| {
            let mut wires = version.edges().values().filter(|edge| edge.from.node == *id).peekable();
            wires.peek().is_some() && wires.all(|edge| feeds.contains_key(&(edge.from.node, edge.from.port.as_str())))
        })
        .collect();

    // The version's layout, moved so its top-left corner is where the technique node was.
    let kept = version.nodes().values().filter(|n| !replaced.contains(&n.id));
    let (min_x, min_y) = kept.clone().fold((f32::INFINITY, f32::INFINITY), |(x, y), n| (x.min(n.pos.x), y.min(n.pos.y)));
    let mut changes = vec![Change::RemoveNode { canvas: p.canvas, id: node.id }];
    let mut added = Vec::new();
    for original in kept {
        let mut copy = original.clone();
        copy.id = map(original.id)?;
        let moved = Position { x: node.pos.x + (original.pos.x - min_x), y: node.pos.y + (original.pos.y - min_y) };
        if !(moved.x.is_finite() && moved.y.is_finite() && moved.x.abs() <= MAX_COORDINATE && moved.y.abs() <= MAX_COORDINATE) {
            return Err(invalid("the technique's nodes would land outside the canvas"));
        }
        copy.pos = moved;
        copy.tag = None;
        copy.field = None;
        if canvas.nodes().contains_key(&copy.id) {
            return Err(Error::new(ErrorKind::Conflict, "a detached node's id is already taken; use another seed"));
        }
        added.push(copy);
    }
    for (exposed, spec) in info.params.iter().zip(&pinned.params) {
        let target = map(exposed.node)?;
        if let (Some(value), Some(copy)) = (node.params.get(&spec.name), added.iter_mut().find(|copy| copy.id == target)) {
            copy.params.insert(exposed.param.clone(), value.clone());
        }
    }
    let result_nodes: Vec<NodeId> = added.iter().map(|copy| copy.id).collect();
    changes.extend(added.into_iter().map(|node| Change::AddNode { canvas: p.canvas, node }));

    // The version's wires, those leaving a fed input replaced by one per feed, each input's
    // wires numbered again in order; then the wires that left the technique node.
    let mut wires: Vec<(Endpoint, u32, usize, Endpoint)> = Vec::new();
    for edge in version.edges().values() {
        let to = Endpoint { node: map(edge.to.node)?, port: edge.to.port.clone() };
        match feeds.get(&(edge.from.node, edge.from.port.as_str())) {
            Some(list) => wires.extend(list.iter().enumerate().map(|(n, (_, from))| (to.clone(), edge.order, n, from.clone()))),
            None if replaced.contains(&edge.from.node) => {}
            None => wires.push((to, edge.order, 0, Endpoint { node: map(edge.from.node)?, port: edge.from.port.clone() })),
        }
    }
    wires.sort_by(|a, b| (&a.0.node, &a.0.port, a.1, a.2).cmp(&(&b.0.node, &b.0.port, b.1, b.2)));
    let mut previous: Option<Endpoint> = None;
    let mut order = 0u32;
    let mut taken: BTreeSet<EdgeId> = canvas.edges().keys().copied().collect();
    for (n, (to, _, _, from)) in wires.into_iter().enumerate() {
        order = if previous.as_ref() == Some(&to) { order.saturating_add(1) } else { 0 };
        previous = Some(to.clone());
        let id = EdgeId::from_bytes(derive(p.seed, "detach edge", n));
        if !taken.insert(id) {
            return Err(Error::new(ErrorKind::Conflict, "a detached wire's id is already taken; use another seed"));
        }
        changes.push(Change::Connect { canvas: p.canvas, edge: Edge { id, from, to, order } });
    }
    // Removing the technique node took its wires off; the outgoing ones come back from the copies.
    changes.extend(leaving.into_iter().map(|edge| Change::Connect { canvas: p.canvas, edge }));
    let result = json!({"nodes": result_nodes, "technique": info.id, "version": info.version});
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- registration ----

/// Enabled when it would succeed and the document can be edited.
fn enabled(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    run(c, value)?;
    c.editable()
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

macro_rules! spec {
    ($id:literal, $label:literal, $schema:expr, $doc:literal, $run:ident) => {
        CommandSpec {
            id: $id,
            label: $label,
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || $schema,
            params_doc: $doc,
            enabled: |c, v| enabled(c, v, $run),
            run: $run,
        }
    };
}

fn coordinate() -> Json {
    json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "technique.place",
            "Place a technique",
            schema(
                &["canvas", "version", "id", "pos"],
                json!({"canvas": uuid(), "version": uuid(), "id": uuid(),
                    "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate(), "y": coordinate()}}})
            ),
            "{canvas, version: CanvasId of a published version, id: new NodeId, pos: {x, y}} -> {id, technique, version}; one node running that version: its inputs and outputs are the technique's, its parameters the ones the technique exposes. It keeps that version until upgraded.",
            place
        ),
        spec!(
            "technique.upgrade",
            "Upgrade a technique node",
            schema(&["canvas", "node", "version"], json!({"canvas": uuid(), "node": uuid(), "version": uuid()})),
            "{canvas, node, version: CanvasId of another version of the same technique} -> {node, technique, version, dropped_params: [name], disconnected}; switches the node to that version. Parameters it still has keep their values; wires to ports it no longer has, or of another type, are taken off.",
            upgrade
        ),
        spec!(
            "technique.detach",
            "Detach a technique node",
            schema(&["canvas", "node", "seed"], json!({"canvas": uuid(), "node": uuid(), "seed": uuid()})),
            "{canvas, node, seed: new UUID} -> {nodes: [NodeId], technique, version}; replaces the node with a copy of its version's nodes, wired as it was and with its parameter values, as one undo step. Input nodes a wire fed are replaced by what fed them. New ids derive from `seed`.",
            detach
        ),
    ]
}

#[cfg(test)]
mod tests;
