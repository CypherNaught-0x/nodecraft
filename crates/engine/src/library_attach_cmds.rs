//! `library.attach` (#96): several of the Library's entries on a canvas at once, as source
//! cards, in one undo step. An entry is an asset the project already holds (named by its hash)
//! or one value of a past result (as `history.extract` names it). Each card is what
//! `history.extract` (for a result) or the Assets browser's Insert (for an asset) makes, laid out
//! in a grid from `at`, and titled after where it came from.
//!
//! The document is the only source of truth: an asset's type and metadata come from the
//! document's own reference to it, never from the caller, and an asset whose bytes the project
//! no longer stores is refused. The whole request is refused when any entry is, so the step is
//! either all of it or nothing.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::history_cmds::{result_value, source_kind};
use crate::library_browse_cmds::{locate_all, short_label};
use nodecraft_graph::{Change, Changeset, Node, Position};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, GenerationId, NodeId, ParamValues};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashSet};

/// The most entries one request places.
pub const MAX_ITEMS: usize = 32;
/// The grid spacing when the caller doesn't say.
pub const DEFAULT_STEP: f32 = 320.0;
/// The largest grid spacing.
pub const MAX_STEP: f32 = 10_000.0;
/// The canvas extent positions may use (as `node.move`).
pub(crate) const MAX_COORDINATE: f32 = 10_000_000.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    canvas: CanvasId,
    node: NodeId,
    generation: GenerationId,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    item: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetItem {
    hash: Blake3,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultItem {
    from: Origin,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Item {
    Asset(AssetItem),
    Result(ResultItem),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachParams {
    canvas: CanvasId,
    items: Vec<Item>,
    /// One new card id per item, supplied by the caller so replay stays deterministic.
    ids: Vec<NodeId>,
    /// The first card's top-left corner.
    at: Position,
    #[serde(default)]
    step: Option<f32>,
}

/// The smallest column count whose square holds `n` (at most [`MAX_ITEMS`]): the grid
/// `library.attach` lays cards out in, which a frontend uses to centre it.
pub fn columns(n: usize) -> usize {
    (1..=MAX_ITEMS).find(|c| c.saturating_mul(*c) >= n).unwrap_or(MAX_ITEMS)
}

pub(crate) fn within(value: f32) -> bool {
    value.is_finite() && value.abs() <= MAX_COORDINATE
}

/// The cards the request adds, with what each was made from; nothing is changed yet.
fn plan(c: &Context<'_>, value: &Json) -> Result<(CanvasId, Vec<Node>, Vec<Json>), Error> {
    let p: AttachParams = params(value)?;
    if p.items.is_empty() {
        return Err(invalid("choose at least one entry to place"));
    }
    if p.items.len() > MAX_ITEMS {
        return Err(invalid("at most 32 entries are placed at once"));
    }
    if p.ids.len() != p.items.len() {
        return Err(invalid("give one new card id per entry"));
    }
    let step = p.step.unwrap_or(DEFAULT_STEP);
    if !(step.is_finite() && step > 0.0 && step <= MAX_STEP) {
        return Err(invalid("step must be more than 0 and at most 10000"));
    }
    if !(within(p.at.x) && within(p.at.y)) {
        return Err(invalid("position is outside the canvas"));
    }
    let target = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let mut fresh = HashSet::new();
    for id in &p.ids {
        if target.nodes().contains_key(id) || !fresh.insert(*id) {
            return Err(Error::new(ErrorKind::Conflict, "a new card id is already taken; choose fresh ids"));
        }
    }
    let columns = columns(p.items.len());
    // One walk of the document for every asset entry, never one per entry.
    let located = if p.items.iter().any(|item| matches!(item, Item::Asset(_))) { locate_all(c.document) } else { Default::default() };
    let mut nodes = Vec::with_capacity(p.items.len());
    let mut placed = Vec::with_capacity(p.items.len());
    for (index, (item, id)) in p.items.iter().zip(&p.ids).enumerate() {
        let (column, row) = (index % columns, index / columns);
        let pos = Position { x: p.at.x + step * column as f32, y: p.at.y + step * row as f32 };
        if !(within(pos.x) && within(pos.y)) {
            return Err(invalid("the cards would go outside the canvas"));
        }
        let (kind, parameter, value, title, from) = match item {
            Item::Asset(AssetItem { hash }) => {
                let found =
                    located.get(hash).ok_or_else(|| Error::new(ErrorKind::NotFound, "the project holds no such image, video, sound, 3D model or PDF"))?;
                if !c.assets.contains(hash) {
                    return Err(Error::new(ErrorKind::NotFound, "the file is missing from this project"));
                }
                let (kind, parameter) = source_kind(found.kind)?;
                let title = match found.result {
                    Some((_, index, count)) => format!("{} · {index}/{count}", short_label(&found.label)),
                    None if found.titled => short_label(&found.label),
                    None => String::new(),
                };
                let from = json!({"canvas": found.canvas, "node": found.node, "generation": found.result.map(|r| r.0)});
                (kind, parameter, nodecraft_types::Value::Media(found.asset.clone()), title, from)
            }
            Item::Result(ResultItem { from }) => {
                let (value, modality) = result_value(c, from.canvas, from.node, from.generation, from.port.as_deref(), from.item)?;
                if let nodecraft_types::Value::Media(asset) = value
                    && !c.assets.contains(&asset.hash)
                {
                    return Err(Error::new(ErrorKind::NotFound, "the result's file is missing from this project"));
                }
                let (kind, parameter) = source_kind(modality)?;
                // result_value found the card and its result, so both exist.
                let source = c.document.canvases().get(&from.canvas).and_then(|canvas| canvas.nodes().get(&from.node));
                let title = source
                    .and_then(|node| {
                        let index = node.history.iter().position(|g| g.id == from.generation)?;
                        let label = if node.title.trim().is_empty() { &node.kind } else { &node.title };
                        Some(format!("{} · {}/{}", short_label(label), index.saturating_add(1), node.history.len()))
                    })
                    .unwrap_or_default();
                let from = json!({"canvas": from.canvas, "node": from.node, "generation": from.generation, "port": from.port, "item": from.item});
                (kind, parameter, value.clone(), title, from)
            }
        };
        let supplied: ParamValues = [(parameter.to_owned(), value.clone())].into_iter().collect();
        let mut node = c.kinds.get(kind)?.instantiate_instance(*id, BTreeMap::new(), &supplied)?;
        node.pos = pos;
        node.title = title;
        let hash = match &value {
            nodecraft_types::Value::Media(asset) => Some(asset.hash),
            _ => None,
        };
        placed.push(json!({"id": id, "kind": kind, "hash": hash, "from": from}));
        nodes.push(node);
    }
    Ok((p.canvas, nodes, placed))
}

fn attach(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let (canvas, nodes, placed) = plan(c, value)?;
    let changes = Changeset::new(nodes.into_iter().map(|node| Change::AddNode { canvas, node }).collect());
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"placed": placed}), gesture: None })
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    c.editable()?;
    plan(c, value).map(|_| ())
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    let asset = json!({"type": "object", "additionalProperties": false, "required": ["hash"],
        "properties": {"hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"}}});
    let result = json!({"type": "object", "additionalProperties": false, "required": ["from"],
        "properties": {"from": {"type": "object", "additionalProperties": false, "required": ["canvas", "node", "generation"],
            "properties": {"canvas": uuid, "node": uuid, "generation": uuid, "port": {"type": "string", "maxLength": 256}, "item": {"type": "integer", "minimum": 0}}}}});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "items", "ids", "at"],
        "properties": {
            "canvas": uuid,
            "items": {"type": "array", "minItems": 1, "maxItems": MAX_ITEMS, "items": {"oneOf": [asset, result]}},
            "ids": {"type": "array", "minItems": 1, "maxItems": MAX_ITEMS, "items": uuid},
            "at": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "step": {"type": "number", "exclusiveMinimum": 0, "maximum": MAX_STEP}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "library.attach",
        label: "Place on canvas",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, items: [{hash} | {from: {canvas, node, generation, port?, item?}}] (1-32), ids: [NodeId] (one new card id per item), at: {x, y}, step?: grid spacing (320)} -> {placed: [{id, kind, hash, from}]}; puts library entries on `canvas` as source cards in one undo step, in a grid from `at`. `{hash}` is an asset library.assets lists (its type and metadata come from the project, not the caller); `{from}` is one value of a past result, as history.extract and library.history's `place` name it, on any canvas. Each card is titled after the card and result it came from, and `from` says where that was. Refused whole if any entry is unknown, missing from the project, a trained style, or an id is taken.",
        enabled,
        run: attach,
    }]
}

#[cfg(test)]
mod tests;
