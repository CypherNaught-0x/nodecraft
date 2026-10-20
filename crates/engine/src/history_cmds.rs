//! A node's generation history (#81): choose the active result (the one downstream uses), step
//! through results, delete one or all of them, and extract one to the canvas as a source node.
//! History is kept oldest first; every edit is one undo step.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Node, Position};
use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, Modality, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeParams {
    canvas: CanvasId,
    id: NodeId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectParams {
    canvas: CanvasId,
    id: NodeId,
    #[serde(deserialize_with = "Option::deserialize")]
    generation: Option<GenerationId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteParams {
    canvas: CanvasId,
    id: NodeId,
    generation: GenerationId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractParams {
    canvas: CanvasId,
    id: NodeId,
    generation: GenerationId,
    /// The output port to extract; the node's first output when omitted.
    #[serde(default)]
    port: Option<String>,
    /// Which item of a list output to extract.
    #[serde(default)]
    item: Option<usize>,
    /// The new node's id, supplied by the caller so replay stays deterministic.
    node: NodeId,
    pos: Position,
}

fn node<'a>(c: &'a Context<'_>, canvas: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    c.document
        .canvases()
        .get(&canvas)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?
        .nodes()
        .get(&id)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))
}

fn index_of(node: &Node, generation: GenerationId) -> Result<usize, Error> {
    node.history.iter().position(|g| g.id == generation).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such result"))
}

fn edit(changes: Vec<Change>, result: Json) -> Effect {
    Effect::Edit { changes: Changeset::new(changes), result, gesture: None }
}

fn select(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SelectParams = params(value)?;
    let node = node(c, p.canvas, p.id)?;
    if let Some(generation) = p.generation {
        index_of(node, generation)?;
    }
    if node.active == p.generation {
        let message = if p.generation.is_some() { "that result is already active" } else { "no result is active" };
        return Err(Error::new(ErrorKind::Conflict, message));
    }
    Ok(edit(vec![Change::SetActiveGeneration { canvas: p.canvas, id: p.id, generation: p.generation }], json!({"active": p.generation})))
}

/// The neighbour `step` places away from the active result (newest when none is active).
fn step(c: &Context<'_>, value: &Json, newer: bool) -> Result<Effect, Error> {
    let p: NodeParams = params(value)?;
    let node = node(c, p.canvas, p.id)?;
    let last = node.history.len().checked_sub(1).ok_or_else(|| Error::new(ErrorKind::Conflict, "the node has no results"))?;
    let target = match node.active.map(|active| index_of(node, active)).transpose()? {
        None => Some(last),
        Some(index) if newer => index.checked_add(1).filter(|next| *next <= last),
        Some(index) => index.checked_sub(1),
    };
    let generation = target
        .and_then(|index| node.history.get(index))
        .map(|g| g.id)
        .ok_or_else(|| Error::new(ErrorKind::Conflict, if newer { "this is the newest result" } else { "this is the oldest result" }))?;
    Ok(edit(vec![Change::SetActiveGeneration { canvas: p.canvas, id: p.id, generation: Some(generation) }], json!({"active": generation})))
}

fn previous(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    step(c, value, false)
}

fn next(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    step(c, value, true)
}

/// Deleting the active result makes its newer neighbour active, else its older one.
fn delete(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: DeleteParams = params(value)?;
    let node = node(c, p.canvas, p.id)?;
    let index = index_of(node, p.generation)?;
    let mut changes = Vec::new();
    let mut active = node.active;
    if node.active == Some(p.generation) {
        let neighbour = index.checked_add(1).and_then(|i| node.history.get(i)).or_else(|| index.checked_sub(1).and_then(|i| node.history.get(i)));
        active = neighbour.map(|g| g.id);
        changes.push(Change::SetActiveGeneration { canvas: p.canvas, id: p.id, generation: active });
    }
    changes.push(Change::RemoveGeneration { canvas: p.canvas, id: p.id, generation: p.generation });
    Ok(edit(changes, json!({"active": active})))
}

/// "Clear node": every result goes, in one undo step.
fn clear(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: NodeParams = params(value)?;
    let node = node(c, p.canvas, p.id)?;
    if node.history.is_empty() {
        return Err(Error::new(ErrorKind::Conflict, "the node has no results"));
    }
    let mut changes = Vec::with_capacity(node.history.len().saturating_add(1));
    if node.active.is_some() {
        changes.push(Change::SetActiveGeneration { canvas: p.canvas, id: p.id, generation: None });
    }
    changes.extend(node.history.iter().map(|g| Change::RemoveGeneration { canvas: p.canvas, id: p.id, generation: g.id }));
    Ok(edit(changes, json!({"removed": node.history.len()})))
}

/// The source kind that holds a value of `modality`, and its parameter.
fn source_kind(modality: Modality) -> Result<(&'static str, &'static str), Error> {
    match modality {
        Modality::Text => Ok(("source.text", "text")),
        Modality::Image => Ok(("source.image", "asset")),
        Modality::Video => Ok(("source.video", "asset")),
        Modality::Audio => Ok(("source.audio", "asset")),
        Modality::Mesh | Modality::Document => Err(Error::new(ErrorKind::Unsupported, "results of this kind can't be extracted to the canvas yet")),
    }
}

/// A new source node holding one result as its parameter. Downstream nodes read a source's
/// parameter (not a result), so it feeds them at once, never runs, and is never stale.
fn extract(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExtractParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let source = node(c, p.canvas, p.id)?;
    let generation = source.history.get(index_of(source, p.generation)?).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such result"))?;
    let port_index = match &p.port {
        Some(name) => {
            source.outputs.iter().position(|port| &port.name == name).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such output"))?
        }
        None => 0,
    };
    let port = source.outputs.get(port_index).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no outputs"))?;
    // Results are positional: one recorded for a different port layout can't be read by port.
    if generation.outputs.len() != source.outputs.len() {
        return Err(Error::new(ErrorKind::Conflict, "this result was made for different outputs than the node has now"));
    }
    let output = generation.outputs.get(port_index).ok_or_else(|| Error::new(ErrorKind::NotFound, "the result has no value for that output"))?;
    let value = match (output, p.item) {
        (Value::List(items), Some(item)) => {
            list_items(items, !generation.items.is_empty()).nth(item).ok_or_else(|| Error::new(ErrorKind::NotFound, "the list has no such item"))?
        }
        (Value::List(_), None) => return Err(invalid("that output is a list: choose an item to extract")),
        (_, Some(_)) => return Err(invalid("that output is a single value, not a list")),
        (value, None) => value,
    };
    let (kind, parameter) = source_kind(port.port_type.modality)?;
    let supplied: ParamValues = [(parameter.to_owned(), value.clone())].into_iter().collect();
    let mut extracted = c.kinds.get(kind)?.instantiate_instance(p.node, BTreeMap::new(), &supplied)?;
    extracted.pos = p.pos;
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node: extracted }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.node}), gesture: None })
}

/// The values a list output offers one by one: its items, with a list per batch item opened when
/// a batch `fanned` the result (a fanned media result, #519), so its media count individually.
pub fn list_items(items: &[Value], fanned: bool) -> impl Iterator<Item = &Value> {
    items.iter().flat_map(move |item| match item {
        Value::List(inner) if fanned => inner.iter(),
        single => std::slice::from_ref(single).iter(),
    })
}

fn enabled(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    c.editable()?;
    if let Effect::Edit { changes, .. } = run(c, value)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}
fn enabled_select(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, select)
}
fn enabled_previous(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, previous)
}
fn enabled_next(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, next)
}
fn enabled_delete(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, delete)
}
fn enabled_clear(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, clear)
}
fn enabled_extract(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, extract)
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn object(properties: Json, required: &[&str]) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": required, "properties": properties})
}
fn node_schema() -> Json {
    object(json!({"canvas": uuid(), "id": uuid()}), &["canvas", "id"])
}
fn select_schema() -> Json {
    object(json!({"canvas": uuid(), "id": uuid(), "generation": {"type": ["string", "null"], "format": "uuid"}}), &["canvas", "id", "generation"])
}
fn delete_schema() -> Json {
    object(json!({"canvas": uuid(), "id": uuid(), "generation": uuid()}), &["canvas", "id", "generation"])
}
fn extract_schema() -> Json {
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    object(
        json!({"canvas": uuid(), "id": uuid(), "generation": uuid(), "port": {"type": "string", "maxLength": 256},
            "item": {"type": "integer", "minimum": 0}, "node": uuid(),
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}}}),
        &["canvas", "id", "generation", "node", "pos"],
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "history.select",
            label: "Use this result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: select_schema,
            params_doc: "{canvas, id: NodeId, generation: GenerationId | null} -> {active}; makes a result the node's active one (what downstream nodes use); null clears it.",
            enabled: enabled_select,
            run: select,
        },
        CommandSpec {
            id: "history.previous",
            label: "Previous result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: node_schema,
            params_doc: "{canvas, id: NodeId} -> {active}; makes the next older result active (the newest when none is).",
            enabled: enabled_previous,
            run: previous,
        },
        CommandSpec {
            id: "history.next",
            label: "Next result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: node_schema,
            params_doc: "{canvas, id: NodeId} -> {active}; makes the next newer result active (the newest when none is).",
            enabled: enabled_next,
            run: next,
        },
        CommandSpec {
            id: "history.delete",
            label: "Delete result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: delete_schema,
            params_doc: "{canvas, id: NodeId, generation: GenerationId} -> {active}; removes one result. Deleting the active one makes its newer neighbour active, else its older one. Undoable.",
            enabled: enabled_delete,
            run: delete,
        },
        CommandSpec {
            id: "history.clear",
            label: "Clear node",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: node_schema,
            params_doc: "{canvas, id: NodeId} -> {removed}; removes every result of the node in one undo step.",
            enabled: enabled_clear,
            run: clear,
        },
        CommandSpec {
            id: "history.extract",
            label: "Extract result to canvas",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: extract_schema,
            params_doc: "{canvas, id: NodeId, generation, port?: output name (default the first), item?: list index (counting through the per-item lists of a batch-fanned result), node: new NodeId, pos: {x, y}} -> {id}; adds a source node (text, image, video or audio) whose parameter is that value, so it feeds downstream nodes at once. The caller supplies the new id.",
            enabled: enabled_extract,
            run: extract,
        },
    ]
}

#[cfg(test)]
mod tests;
