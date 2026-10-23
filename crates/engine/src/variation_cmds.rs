//! `node.variations_set` (#81): how many variations one run of a generation node makes. See
//! [`nodecraft_exec::variations`]. `canvas.set_variation_cards` (#808): whether a canvas's runs
//! put their extra variations on cards beside the source instead of in its history.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_exec::run::record_changes;
use nodecraft_exec::variations::{MAX_VARIATIONS, VARIATIONS_CONFIG, requested};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Generation, Node, Position};
use nodecraft_nodes::Category;
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, Unsigned, Value as Wire};
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

/// How far right of its source a variation's card goes, and how far down each taken spot moves it.
const CARD_STEP_X: f32 = 360.0;
/// Taller than the tallest card (a media card with its prompt), so stacked cards never overlap.
const CARD_STEP_Y: f32 = 600.0;
/// Spots tried below the first before a card goes on the last one anyway.
const MAX_CARD_TRIES: u16 = 64;
/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

/// The first spot right of `source` that no card takes, stepping down past taken ones.
fn free_spot(canvas: &Canvas, source: &Node, placed: &[Position]) -> Position {
    let x = (source.pos.x + CARD_STEP_X).clamp(-MAX_COORDINATE, MAX_COORDINATE);
    let near = |pos: &Position, y: f32| (pos.x - x).abs() < CARD_STEP_X && (pos.y - y).abs() < CARD_STEP_Y;
    let taken = |y: f32| canvas.nodes().values().any(|n| near(&n.pos, y)) || placed.iter().any(|pos| near(pos, y));
    let mut y = source.pos.y.clamp(-MAX_COORDINATE, MAX_COORDINATE);
    for _ in 0..MAX_CARD_TRIES {
        if !taken(y) {
            break;
        }
        y = (y + CARD_STEP_Y).clamp(-MAX_COORDINATE, MAX_COORDINATE);
    }
    Position { x, y }
}

/// The seed parameters `node` pins while asking for several variations (#808): a model that
/// follows its seed returns the same result for each. Empty for one variation, or a node whose
/// model isn't in `catalog` or pins none.
pub fn pinned_seeds(catalog: &crate::generation_catalog::GenerationCatalog, node: &Node) -> Vec<String> {
    if nodecraft_exec::variations::requested(node) < 2 {
        return Vec::new();
    }
    let Some(Wire::Text(model)) = node.params.get(nodecraft_nodes::generation::MODEL) else { return Vec::new() };
    let run = catalog.run_catalog();
    run.catalog().get(model).and_then(|model| model.pinned_seeds(&node.params, false).ok()).unwrap_or_default()
}

/// The card a variation `generation` goes on when its canvas lays variations out as cards (#808):
/// derived from the result, so replaying a run's records makes the same cards.
pub fn card_id(generation: nodecraft_types::GenerationId) -> NodeId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nodecraft.variation.card");
    hasher.update(generation.as_bytes());
    let mut bytes = [0u8; 16];
    if let Some(head) = hasher.finalize().as_bytes().get(..16) {
        bytes.copy_from_slice(head);
    }
    NodeId::from_bytes(bytes)
}

fn edge_id(card: NodeId, edge: EdgeId) -> EdgeId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nodecraft.variation.edge");
    hasher.update(card.as_bytes());
    hasher.update(edge.as_bytes());
    let mut bytes = [0u8; 16];
    if let Some(head) = hasher.finalize().as_bytes().get(..16) {
        bytes.copy_from_slice(head);
    }
    EdgeId::from_bytes(bytes)
}

/// The changes that put `generation`, a variation of `source`, on a new card `card` beside it
/// (#808): a copy of `source` that makes one result a run, wired to the same inputs, holding just
/// that result. One undo step with the run's record.
/// `placed` holds the cards placed earlier in the same record, which aren't on `canvas` yet; this
/// card's spot is added to it.
pub(crate) fn card_changes(
    canvas_id: CanvasId,
    canvas: &Canvas,
    source: &Node,
    card: NodeId,
    generation: Generation,
    placed: &mut Vec<Position>,
) -> Result<Vec<Change>, Error> {
    if canvas.nodes().contains_key(&card) {
        return Err(Error::new(ErrorKind::Conflict, "a variation's card id is already on the canvas"));
    }
    let mut node = source.clone();
    node.id = card;
    node.pos = free_spot(canvas, source, placed);
    placed.push(node.pos);
    node.config.remove(VARIATIONS_CONFIG);
    node.history = Vec::new();
    node.active = None;
    let wires = canvas.edges().values().filter(|edge| edge.to.node == source.id).map(|edge| {
        let mut to = edge.to.clone();
        to.node = card;
        Change::Connect { canvas: canvas_id, edge: Edge { id: edge_id(card, edge.id), from: edge.from.clone(), to, order: edge.order } }
    });
    let mut changes: Vec<Change> = vec![Change::AddNode { canvas: canvas_id, node: node.clone() }];
    changes.extend(wires);
    changes.extend(record_changes(canvas_id, &node, generation)?);
    Ok(changes)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CardsParams {
    canvas: CanvasId,
    cards: bool,
}

fn cards_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "cards"], "properties": {
        "canvas": {"type": "string", "format": "uuid"}, "cards": {"type": "boolean"}
    }})
}

fn set_cards(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: CardsParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no canvas has that id"))?;
    let result = json!({"canvas": p.canvas, "cards": p.cards});
    if canvas.variation_cards() == p.cards {
        return Ok(Effect::None { result });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetCanvasVariationCards { canvas: p.canvas, cards: p.cards }]), result, gesture: None })
}

fn cards_enabled(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    if let Effect::Edit { changes, .. } = set_cards(c, value)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "canvas.set_variation_cards",
            label: "Variations as cards",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: cards_schema,
            params_doc: "{canvas, cards} -> {canvas, cards}. Where a run on this canvas puts a node's variations: false (the default) keeps them all in the node's history; true keeps the one the run passes on (the last) on the node and puts each other variation on a new card beside it, a copy of the node holding that result, wired to the same inputs, in the same undo step as the run's results.",
            enabled: cards_enabled,
            run: set_cards,
        },
        CommandSpec {
            id: "node.variations_set",
            label: "Set variations",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{canvas, id, count?: 1..20|null}; how many variations one run of a generation node makes, each its own paid request (quoted as such) recorded as a result of its own; null means one. Doesn't make results stale. Returns {count}.",
            enabled: always,
            run: variations_set,
        },
    ]
}

#[cfg(test)]
mod tests;
