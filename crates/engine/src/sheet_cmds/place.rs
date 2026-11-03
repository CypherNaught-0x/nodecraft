//! `sheet.place` (#163): sends rows' active results to a canvas as ordinary source cards, the
//! cards `history.extract` makes, so they feed other cards at once and never run.
//!
//! Every placed card records where it came from under node config [`RESULT_CONFIG`]: the sheet,
//! the row, the result (generation), the output and list item, and the asset's hash. Placement is
//! one changeset (one undo step), laid out in a grid of [`COLUMNS`] in row order from `pos`, or
//! to the right of what the canvas already holds. Rows with no result, or whose result can't
//! become a source card, are reported and skipped; when nothing can be placed it is refused.
use super::*;
use crate::history_cmds::{list_items, source_kind};

/// The node config key recording which sheet row and result a placed card holds: JSON text.
pub const RESULT_CONFIG: &str = "nodecraft.sheet_result";
/// Most cards one placement adds.
pub const MAX_PLACED: usize = MAX_ADD;
/// Cards per line of the placed grid.
pub const COLUMNS: usize = 4;
/// Distance between placed cards across and down.
pub const PLACE_SPACING: Position = Position { x: 400.0, y: 440.0 };
/// The gap left between what a canvas holds and cards placed right of it.
const PLACE_GAP: f32 = 480.0;
/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceParams {
    canvas: CanvasId,
    seed: NodeId,
    rows: Vec<NodeId>,
    /// The canvas the cards go on: a plain canvas, not a sheet.
    target: CanvasId,
    /// The first card's place; right of the target's cards when absent.
    #[serde(default)]
    pos: Option<Position>,
}

/// One value to place: the row, result, item and the value itself.
struct Placed<'a> {
    row: NodeId,
    generation: nodecraft_types::GenerationId,
    port: &'a str,
    item: Option<usize>,
    value: &'a Value,
    modality: nodecraft_types::Modality,
}

/// Where the first card goes when the caller doesn't say, for a grid of `count` cards: right
/// of everything on `canvas`, level with its top; else, when that grid would leave the canvas,
/// below everything, level with its left; else the origin. Nodes carry no size, so the gaps
/// assume cards of about the default 320-point width and 400-point height.
fn anchor(canvas: &Canvas, count: usize) -> Position {
    let nodes = canvas.nodes().values();
    let fold = |pick: fn(&Node) -> f32, better: fn(f32, f32) -> f32| {
        nodes.clone().map(pick).fold(None, |best: Option<f32>, v| Some(best.map_or(v, |best| better(best, v))))
    };
    let (right, left) = (fold(|node| node.pos.x, f32::max), fold(|node| node.pos.x, f32::min));
    let (top, bottom) = (fold(|node| node.pos.y, f32::min), fold(|node| node.pos.y, f32::max));
    let mut candidates = Vec::new();
    if let (Some(right), Some(top)) = (right, top) {
        candidates.push(Position { x: right + PLACE_GAP, y: top });
    }
    if let (Some(left), Some(bottom)) = (left, bottom) {
        candidates.push(Position { x: left, y: bottom + PLACE_SPACING.y });
    }
    candidates.into_iter().find(|from| fits(*from, count)).unwrap_or_default()
}

/// Whether every card of a grid of `count` from `from` is inside the canvas.
fn fits(from: Position, count: usize) -> bool {
    inside(from) && count.checked_sub(1).is_none_or(|last| inside(grid(from, last)) && inside(grid(from, last.min(COLUMNS - 1))))
}

fn inside(pos: Position) -> bool {
    pos.x.is_finite() && pos.y.is_finite() && pos.x.abs() <= MAX_COORDINATE && pos.y.abs() <= MAX_COORDINATE
}

/// The `n`th card's place in the grid from `from`.
fn grid(from: Position, n: usize) -> Position {
    // n < MAX_PLACED, so both are small and exact as f32.
    let (column, line) = (n % COLUMNS, n / COLUMNS);
    Position { x: from.x + column as f32 * PLACE_SPACING.x, y: from.y + line as f32 * PLACE_SPACING.y }
}

/// The values of `row`'s active result to place, in order, or why there are none.
fn values<'a>(row: &'a Node) -> Result<Vec<Placed<'a>>, &'static str> {
    let generation = row.active.and_then(|id| row.history.iter().find(|g| g.id == id)).ok_or("it has no result yet")?;
    // Results are positional; one recorded for other outputs can't be read by port.
    if generation.outputs.len() != row.outputs.len() {
        return Err("its result was made for different outputs than the row has now");
    }
    let fanned = !generation.items.is_empty();
    let mut out = Vec::new();
    for (port, value) in row.outputs.iter().zip(&generation.outputs) {
        let at = |item, value| Placed { row: row.id, generation: generation.id, port: &port.name, item, value, modality: port.port_type.modality };
        match value {
            Value::List(items) => out.extend(list_items(items, fanned).enumerate().map(|(n, value)| at(Some(n), value))),
            single => out.push(at(None, single)),
        }
    }
    if out.is_empty() { Err("its result holds nothing to place") } else { Ok(out) }
}

fn provenance(sheet: CanvasId, placed: &Placed<'_>) -> Value {
    let asset = match placed.value {
        Value::Media(asset) => json!(asset.hash.to_string()),
        _ => Json::Null,
    };
    let recorded = json!({"sheet": sheet, "row": placed.row, "generation": placed.generation, "port": placed.port, "item": placed.item, "asset": asset});
    Value::Text(recorded.to_string())
}

fn place(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PlaceParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let order = rows(canvas);
    let chosen = selected(&order, &p.rows, MAX_ADD)?;
    let target = c.document.canvases().get(&p.target).ok_or_else(|| Error::new(ErrorKind::NotFound, "the target canvas does not exist"))?;
    if target.kind() != CanvasKind::Canvas {
        return Err(invalid("results go on a plain canvas, not a sheet or a published technique"));
    }
    let mut found = Vec::new();
    let mut skipped: Vec<Json> = Vec::new();
    let mut skip = |row: NodeId, reason: &str| {
        // One entry per row: its first reason.
        if !skipped.iter().any(|entry| entry["row"] == json!(row)) {
            skipped.push(json!({"row": row, "reason": reason}));
        }
    };
    // Row order, not the order the caller listed them in: the grid reads like the sheet.
    for row in order.iter().filter(|row| chosen.contains(&row.id)) {
        let values = match values(row) {
            Ok(values) => values,
            Err(reason) => {
                skip(row.id, reason);
                continue;
            }
        };
        for one in values {
            let (kind, parameter) = match source_kind(one.modality) {
                Ok(source) => source,
                Err(error) => {
                    skip(row.id, &error.message);
                    continue;
                }
            };
            if found.len() >= MAX_PLACED {
                return Err(Error::new(ErrorKind::Overflow, format!("one placement adds at most {MAX_PLACED} cards; choose fewer rows")));
            }
            let id = NodeId::from_bytes(derive(p.seed, "place", found.len()));
            let supplied: ParamValues = [(parameter.to_owned(), one.value.clone())].into_iter().collect();
            let config: BTreeMap<String, Value> = [(RESULT_CONFIG.to_owned(), provenance(p.canvas, &one))].into_iter().collect();
            match c.kinds.get(kind)?.instantiate_instance(id, config, &supplied) {
                Ok(node) => found.push((node, json!({"id": id, "row": one.row, "generation": one.generation, "port": one.port, "item": one.item}))),
                Err(error) => skip(row.id, &error.message),
            }
        }
    }
    if found.is_empty() {
        let why = skipped.first().and_then(|first| first["reason"].as_str()).unwrap_or("nothing to place");
        let message =
            if chosen.len() == 1 { format!("nothing to place: {why}") } else { format!("none of those rows has a result to place (the first: {why})") };
        return Err(Error::new(ErrorKind::Unsupported, message));
    }
    let from = match p.pos {
        Some(pos) if !inside(pos) => return Err(invalid("position is outside the canvas")),
        Some(pos) if !fits(pos, found.len()) => return Err(invalid("the cards would go outside the canvas; place them nearer the middle")),
        Some(pos) => pos,
        None => anchor(target, found.len()),
    };
    let mut changes = Vec::with_capacity(found.len());
    let mut placed = Vec::with_capacity(found.len());
    for (n, (mut node, report)) in found.into_iter().enumerate() {
        node.pos = grid(from, n);
        changes.push(Change::AddNode { canvas: p.target, node });
        placed.push(report);
    }
    edit(c, changes, json!({"target": p.target, "placed": placed, "skipped": skipped}))
}

pub(super) fn spec() -> CommandSpec {
    spec!(
        "sheet.place",
        "Send results to a canvas",
        schema(
            &["canvas", "seed", "rows", "target"],
            json!({"canvas": uuid(), "seed": uuid(), "target": uuid(),
                "rows": {"type": "array", "items": uuid(), "minItems": 1, "maxItems": MAX_ADD},
                "pos": {"type": "object", "additionalProperties": false, "required": ["x", "y"],
                    "properties": {"x": {"type": "number"}, "y": {"type": "number"}}}})
        ),
        "{canvas: sheet, seed: new UUID, rows, target: plain CanvasId, pos?: {x, y}} -> {target, placed: [{id, row, generation, port, item}], skipped: [{row, reason}]}; puts each row's active result on `target` as source cards (as history.extract makes them: text, image, video, audio, 3D or document), every item of a list result its own card, in a grid four across in sheet order from `pos` (when absent: right of the target's cards, else below them if that would leave the canvas, else the origin). Each card records {sheet, row, generation, port, item, asset} under config `nodecraft.sheet_result`. One undo step; rows without a result are skipped and reported once each, and placing nothing is refused. At most 100 cards; new ids derive from `seed`.",
        place
    )
}

#[cfg(test)]
#[path = "place_tests.rs"]
mod tests;
