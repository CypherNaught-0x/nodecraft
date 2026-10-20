//! App mode (#95): a technique node shown as a form for people who never see the graph. The form
//! is generated from the node's pinned interface ([`nodecraft_nodes::technique`]); filling an
//! input creates, updates or removes the "form sources" feeding it: source nodes wired only to
//! that input, laid out left of the node, so the graph stays the one truth and a form edit is an
//! ordinary undo step. Parameters go through `node.param_set`, runs through `run.quote` and
//! `run.start`, results through `history.*`.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::technique_cmds::derive;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Position};
use nodecraft_nodes::technique::{self, Exposed, Interface};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, PortType, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

/// The canvas extent positions may use (as `node.create`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// Most values one form input takes.
const MAX_VALUES: usize = 64;
/// Where form sources sit, relative to the technique node: one column to its left.
const SOURCE_OFFSET_X: f32 = -360.0;
const SOURCE_STEP_Y: f32 = 160.0;

fn canvas<'a>(c: &'a Context<'_>, id: CanvasId) -> Result<&'a Canvas, Error> {
    c.document.canvases().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))
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

/// The source kind a form fills an input of this type with, and its value parameter.
fn source_kind(port_type: PortType) -> Result<(&'static str, &'static str), Error> {
    match port_type.modality {
        Modality::Text => Ok(("source.text", "text")),
        Modality::Image => Ok(("source.image", "asset")),
        Modality::Video => Ok(("source.video", "asset")),
        Modality::Audio => Ok(("source.audio", "asset")),
        Modality::Mesh | Modality::Document => Err(Error::new(ErrorKind::Unsupported, "a form can't fill this kind of input yet")),
    }
}

/// What feeds one input of the node: the wires into its port in input order, split into those
/// from form sources and the rest.
struct Feeds<'a> {
    form: Vec<(&'a Edge, &'a Node)>,
    other: Vec<&'a Edge>,
}

fn feeds<'a>(canvas: &'a Canvas, node: NodeId, input: &Exposed) -> Feeds<'a> {
    let mut wires: Vec<&Edge> = canvas.edges().values().filter(|edge| edge.to.node == node && edge.to.port == input.port).collect();
    wires.sort_by_key(|edge| (edge.order, edge.id));
    let kind = source_kind(input.port_type).ok().map(|(kind, _)| kind);
    let mut form = Vec::new();
    let mut other = Vec::new();
    for edge in wires {
        // A form source is a source of the input's kind, holding a value, whose every wire leads
        // to this input. An empty one is someone's work in progress, not the form's.
        let source = canvas.nodes().get(&edge.from.node).filter(|source| {
            Some(source.kind.as_str()) == kind
                && source_value(source).is_some()
                && canvas.edges().values().filter(|wire| wire.from.node == source.id).all(|wire| wire.to.node == node && wire.to.port == input.port)
        });
        match source {
            Some(source) => form.push((edge, source)),
            None => other.push(edge),
        }
    }
    Feeds { form, other }
}

// ---- app.form ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormParams {
    canvas: CanvasId,
    id: NodeId,
}

/// Why the node can't run its version now, as the planner would say it; `None` when it can.
fn blocked(c: &Context<'_>, interface: &Interface) -> Option<&'static str> {
    let Some((version, info)) = c.document.canvases().get(&interface.canvas).and_then(|version| Some((version, version.technique_info()?))) else {
        return Some("its technique version isn't in this project");
    };
    if info.id != interface.technique || info.version != interface.version {
        return Some("its technique version isn't in this project");
    }
    if info.retired {
        return Some("its technique version was removed; upgrade it to another version");
    }
    if !technique::interface_of(version, c.kinds).is_ok_and(|now| now.matches(interface)) {
        return Some("its technique version no longer matches it; upgrade it");
    }
    None
}

/// The value the version's own input node gives when nothing feeds the input: a source's value,
/// or the first value of its result.
fn example(c: &Context<'_>, interface: &Interface, index: usize) -> Option<Value> {
    let version = c.document.canvases().get(&interface.canvas)?;
    let input = version.technique_info()?.inputs.get(index)?;
    let node = version.nodes().get(&input.node)?;
    let param = source_kind(*node.outputs.first().map(|port| &port.port_type)?).ok().filter(|(kind, _)| *kind == node.kind).map(|(_, param)| param);
    match param {
        Some(param) => node.params.get(param).cloned(),
        None => node.active.and_then(|active| node.history.iter().find(|g| g.id == active)).and_then(|g| g.outputs.first().cloned()),
    }
}

fn source_value(node: &Node) -> Option<&Value> {
    let (_, param) = source_kind(node.outputs.first()?.port_type).ok()?;
    node.params.get(param)
}

fn form(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: FormParams = params(value)?;
    let (canvas, node, interface) = instance(c, p.canvas, p.id)?;
    let info = c.document.canvases().get(&interface.canvas).and_then(Canvas::technique_info);
    let inputs: Vec<Json> = interface
        .inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let feeds = feeds(canvas, node.id, input);
            let feed = if !feeds.other.is_empty() {
                "wired"
            } else if !feeds.form.is_empty() {
                "form"
            } else {
                "example"
            };
            let values: Vec<&Value> = feeds.form.iter().filter_map(|(_, source)| source_value(source)).collect();
            let sources: Vec<NodeId> = feeds.form.iter().map(|(_, source)| source.id).collect();
            let from: Vec<NodeId> = feeds.other.iter().map(|edge| edge.from.node).collect();
            json!({"port": input.port, "name": input.name, "port_type": input.port_type, "fillable": source_kind(input.port_type).is_ok(),
                "feed": feed, "values": values, "sources": sources, "from": from, "example": example(c, &interface, index)})
        })
        .collect();
    let contract = c.kinds.get(&node.kind)?.instance_contract(node)?;
    let resolution = contract.resolve_with(&node.params, true, crate::option_cmds::lookup(c.resources))?;
    let params: Vec<Json> = interface
        .params
        .iter()
        .map(|spec| {
            let value = node.params.get(&spec.name).or(spec.default.as_ref());
            json!({"spec": spec, "value": value})
        })
        .collect();
    let blocked = blocked(c, &interface);
    let issues: Vec<&str> = resolution.issues.iter().map(|issue| issue.message.as_str()).collect();
    let result = json!({
        "node": node.id, "title": node.title, "technique": interface.technique, "version": interface.version, "name": interface.name,
        "description": info.map_or("", |info| info.description.as_str()),
        "inputs": inputs, "params": params, "outputs": interface.outputs,
        "ready": blocked.is_none() && issues.is_empty(), "blocked": blocked, "issues": issues,
        "results": {"count": node.history.len(), "active": node.active},
    });
    Ok(Effect::None { result })
}

// ---- app.input_set ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputSetParams {
    canvas: CanvasId,
    id: NodeId,
    port: String,
    /// New node and wire ids derive from it.
    seed: NodeId,
    values: Vec<Value>,
}

fn input_set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: InputSetParams = params(value)?;
    let (canvas, node, interface) = instance(c, p.canvas, p.id)?;
    let input = interface.inputs.iter().find(|input| input.port == p.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "the technique has no such input"))?;
    let (kind_id, param) = source_kind(input.port_type)?;
    if p.values.len() > MAX_VALUES {
        return Err(Error::new(ErrorKind::Overflow, "too many values for one input"));
    }
    if !input.port_type.many && p.values.len() > 1 {
        return Err(invalid("this input takes one value"));
    }
    for value in &p.values {
        if let Value::Media(asset) = value {
            c.validate_asset(asset)?;
        }
    }
    let feeds = feeds(canvas, node.id, input);
    if !feeds.other.is_empty() {
        return Err(Error::new(ErrorKind::Conflict, "this input is wired on the canvas; take its wires off to fill it from the form"));
    }
    let kind = c.kinds.get(kind_id)?;
    let mut changes = Vec::new();
    let mut sources = Vec::with_capacity(p.values.len());
    for (n, value) in p.values.iter().enumerate() {
        let supplied = [(param.to_owned(), value.clone())].into_iter().collect();
        if let Some((_, existing)) = feeds.form.get(n) {
            // Checked as a new source would be; the node keeps its place, title and wire.
            kind.instantiate_instance(existing.id, existing.config.clone(), &supplied)?;
            sources.push(existing.id);
            if existing.params.get(param) != Some(value) {
                let mut updated = (*existing).clone();
                updated.params.insert(param.to_owned(), value.clone());
                changes.push(Change::SetNode { canvas: p.canvas, node: updated });
            }
            continue;
        }
        let id = NodeId::from_bytes(derive(p.seed, "app source", n));
        let edge = EdgeId::from_bytes(derive(p.seed, "app wire", n));
        if canvas.nodes().contains_key(&id) || canvas.edges().contains_key(&edge) {
            return Err(Error::new(ErrorKind::Conflict, "a form source's id is already taken; use another seed"));
        }
        let mut source = kind.instantiate_instance(id, Default::default(), &supplied)?;
        let row = f32::from(u16::try_from(n).unwrap_or(u16::MAX));
        source.pos = Position { x: node.pos.x + SOURCE_OFFSET_X, y: node.pos.y + row * SOURCE_STEP_Y };
        if !(source.pos.x.is_finite() && source.pos.y.is_finite() && source.pos.x.abs() <= MAX_COORDINATE && source.pos.y.abs() <= MAX_COORDINATE) {
            return Err(invalid("the form's input would land outside the canvas"));
        }
        source.title = input.name.clone();
        // After the form's existing wires, in value order.
        let after = feeds.form.last().map_or(0, |(edge, _)| edge.order.saturating_add(1));
        let order = after.saturating_add(u32::try_from(n.saturating_sub(feeds.form.len())).unwrap_or(u32::MAX));
        let wire = Edge { id: edge, from: Endpoint { node: id, port: "out".into() }, to: Endpoint { node: node.id, port: input.port.clone() }, order };
        changes.push(Change::AddNode { canvas: p.canvas, node: source });
        changes.push(Change::Connect { canvas: p.canvas, edge: wire });
        sources.push(id);
    }
    // Values taken away remove their sources, and with them their wires.
    for (_, extra) in feeds.form.iter().skip(p.values.len()) {
        changes.push(Change::RemoveNode { canvas: p.canvas, id: extra.id });
    }
    let result = json!({"node": node.id, "port": input.port, "sources": sources});
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- registration ----

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "app.form",
            label: "App form",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema(&["canvas", "id"], json!({"canvas": uuid(), "id": uuid()})),
            params_doc: "{canvas, id: a technique node} -> {node, title, technique, version, name, description, inputs: [{port, name, port_type, fillable, feed: form|wired|example, values, sources, from, example}], params: [{spec, value}], outputs, ready, blocked, issues, results: {count, active}}; the form App mode shows for the node: each input with what feeds it (form sources, other wires, or the version's example when nothing does), its parameters and their values, and whether it can run. Read-only.",
            enabled: |c, v| form(c, v).map(|_| ()),
            run: form,
        },
        CommandSpec {
            id: "app.input_set",
            label: "Fill an app input",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || {
                schema(
                    &["canvas", "id", "port", "seed", "values"],
                    json!({"canvas": uuid(), "id": uuid(), "port": {"type": "string"}, "seed": uuid(), "values": {"type": "array", "maxItems": MAX_VALUES}}),
                )
            },
            params_doc: "{canvas, id: a technique node, port: one of its inputs, seed: new UUID, values: [Value]} -> {node, port, sources: [NodeId]}; fills the input from the form: one source node per value, wired to it in order, made, updated or removed as one undo step. Empty values remove them so the version's example is used. Refused while other wires feed the input; one value at most unless the input takes many. New ids derive from `seed`.",
            enabled: |c, v| {
                input_set(c, v)?;
                c.editable()
            },
            run: input_set,
        },
    ]
}

#[cfg(test)]
mod tests;
