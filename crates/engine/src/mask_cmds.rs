//! Inpainting and outpainting (#102). `mask.create` puts an Expand canvas and a Mask node behind
//! an image (the mask editor paints on the expanded image), `node.mask_set` stores the painted
//! strokes as one undo step, and `mask.edit` wires the image and its mask into a new generation
//! card of a model that takes a mask. Nothing here runs: the new card runs (and is costed) like
//! any other.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use nodecraft_actions::mask::{MAX_STROKES, Stroke, Strokes};
use nodecraft_graph::{Change, Changeset, Edge, Endpoint, Node, Position};
use nodecraft_nodes::generation::{KIND, MODEL, PROMPT};
use nodecraft_nodes::mask::{CONFIG, ID};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// The Expand canvas kind.
pub const EXPAND: &str = "image.expand";
/// How far right of the previous card each new card goes.
const STEP: f32 = 360.0;

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

fn check_pos(pos: Position) -> Result<(), Error> {
    if pos.x.is_finite() && pos.y.is_finite() && pos.x.abs() <= MAX_COORDINATE && pos.y.abs() <= MAX_COORDINATE {
        Ok(())
    } else {
        Err(invalid("position is outside the canvas"))
    }
}

/// `pos` moved `steps` cards right, kept on the canvas.
fn beside(pos: Position, steps: f32) -> Position {
    Position { x: (pos.x + STEP * steps).clamp(-MAX_COORDINATE, MAX_COORDINATE), y: pos.y }
}

fn node<'a>(c: &'a Context<'_>, canvas: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))?.nodes().get(&id).ok_or_else(|| not_found("document entity"))
}

/// The strokes a mask node carries, checked.
pub fn strokes(node: &Node) -> Result<Strokes, Error> {
    let json = nodecraft_nodes::mask::stored(node)?;
    Strokes::parse(json).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the node's mask can't be read: {e}")))
}

/// The models a generation card can inpaint with, in catalog order: those with a mode taking a
/// mask.
pub fn inpaint_models(catalog: &GenerationCatalog) -> Vec<String> {
    catalog.models(KIND).filter(|m| m.modes.iter().any(|mode| mode.inputs.iter().any(|r| r.role == "mask"))).map(|m| m.id.clone()).collect()
}

fn always(_: &Context<'_>, _: &Json) -> Result<(), Error> {
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    canvas: CanvasId,
    id: NodeId,
    strokes: Vec<Stroke>,
}

fn set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SetParams = params(value)?;
    let node = node(c, p.canvas, p.id)?;
    if node.kind != ID {
        return Err(invalid("only a mask node takes strokes"));
    }
    let json = Strokes { strokes: p.strokes }.encode().map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the mask isn't valid: {e}")))?;
    let mut changed = node.clone();
    changed.config.insert(CONFIG.into(), Value::Text(json));
    if changed == *node {
        return Ok(Effect::None { result: json!({"changed": false}) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas: p.canvas, node: changed }]), result: json!({"changed": true}), gesture: None })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    canvas: CanvasId,
    /// The image output to paint over.
    from: Endpoint,
    /// The new Expand canvas and Mask nodes' ids; the caller supplies them so replay stays
    /// deterministic.
    expand: NodeId,
    mask: NodeId,
    /// The new connections' ids: image → Expand, Expand → Mask.
    edges: [EdgeId; 2],
    pos: Position,
}

fn create(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CreateParams = params(value)?;
    check_pos(p.pos)?;
    let source = node(c, p.canvas, p.from.node)?;
    let output = source.outputs.iter().find(|port| port.name == p.from.port).ok_or_else(|| not_found("that output"))?;
    if output.port_type.modality != nodecraft_types::Modality::Image || output.port_type.many {
        return Err(invalid("a mask is painted over one image"));
    }
    let mut expand = c.kinds.get(EXPAND)?.instantiate_instance(p.expand, BTreeMap::new(), &ParamValues::new())?;
    expand.pos = p.pos;
    // The expanded canvas's new margins are masked from the start (outpainting).
    let mut supplied = ParamValues::new();
    supplied.insert("transparent".into(), Value::Bool(true));
    let mut mask = c.kinds.get(ID)?.instantiate_instance(p.mask, BTreeMap::new(), &supplied)?;
    mask.pos = beside(p.pos, 1.0);
    let [first, second] = p.edges;
    let changes = Changeset::new(vec![
        Change::AddNode { canvas: p.canvas, node: expand },
        Change::AddNode { canvas: p.canvas, node: mask },
        Change::Connect { canvas: p.canvas, edge: Edge { id: first, from: p.from, to: Endpoint { node: p.expand, port: "image".into() }, order: 0 } },
        Change::Connect {
            canvas: p.canvas,
            edge: Edge { id: second, from: Endpoint { node: p.expand, port: "out".into() }, to: Endpoint { node: p.mask, port: "image".into() }, order: 0 },
        },
    ]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"expand": p.expand, "mask": p.mask}), gesture: None })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditParams {
    canvas: CanvasId,
    /// The mask node to edit with.
    mask: NodeId,
    /// The new generation card's id, and its connections' (image, mask).
    id: NodeId,
    edges: [EdgeId; 2],
    pos: Position,
    prompt: String,
    /// The model; the first one that takes a mask when absent.
    #[serde(default)]
    model: Option<String>,
}

fn edit(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: EditParams = params(value)?;
    check_pos(p.pos)?;
    let mask = node(c, p.canvas, p.mask)?;
    if mask.kind != ID {
        return Err(invalid("edit from a mask node"));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| not_found("canvas"))?;
    let image = canvas
        .edges()
        .values()
        .find(|e| e.to.node == p.mask && e.to.port == "image")
        .map(|e| e.from.clone())
        .ok_or_else(|| invalid("connect an image to the mask first"))?;
    // Something must be marked: a painted stroke, or the margins an expanded canvas adds.
    let transparent = matches!(mask.params.get("transparent"), Some(Value::Bool(true)));
    let margins = canvas.nodes().get(&image.node).is_some_and(|n| {
        n.kind == EXPAND && ["left", "top", "right", "bottom"].iter().any(|side| matches!(n.params.get(*side), Some(Value::Unsigned(v)) if v.0 > 0))
    });
    if !strokes(mask)?.paints() && !(transparent && margins) {
        return Err(invalid("paint the area to change first, or expand the canvas"));
    }
    let models = c.resources.get::<GenerationCatalog>().map(inpaint_models).unwrap_or_default();
    let model = match p.model {
        Some(model) if models.contains(&model) => model,
        Some(_) => return Err(Error::new(ErrorKind::Unsupported, "that model can't edit with a mask")),
        None => models.into_iter().next().ok_or_else(|| Error::new(ErrorKind::Unsupported, "no model that edits with a mask is available"))?,
    };
    let mut supplied = ParamValues::new();
    supplied.insert(MODEL.into(), Value::Text(model));
    supplied.insert(PROMPT.into(), Value::Text(p.prompt));
    let mut generate = c.kinds.get(KIND)?.instantiate_instance(p.id, BTreeMap::new(), &supplied)?;
    generate.pos = p.pos;
    let [first, second] = p.edges;
    let changes = Changeset::new(vec![
        Change::AddNode { canvas: p.canvas, node: generate },
        Change::Connect { canvas: p.canvas, edge: Edge { id: first, from: image, to: Endpoint { node: p.id, port: "image".into() }, order: 0 } },
        Change::Connect {
            canvas: p.canvas,
            edge: Edge { id: second, from: Endpoint { node: p.mask, port: "mask".into() }, to: Endpoint { node: p.id, port: "mask".into() }, order: 0 },
        },
    ]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id}), gesture: None })
}

fn enabled_create(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    create(c, value).map(|_| ())
}

fn enabled_edit(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    edit(c, value).map(|_| ())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn pos() -> Json {
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}})
}

fn set_schema() -> Json {
    let point = json!({"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "minimum": -1, "maximum": 2}});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "strokes"],
        "properties": {"canvas": uuid(), "id": uuid(),
            "strokes": {"type": "array", "maxItems": MAX_STROKES, "items": {"type": "object", "additionalProperties": false, "required": ["size", "hardness", "points"], "properties": {
                "size": {"type": "number", "minimum": nodecraft_actions::mask::MIN_SIZE, "maximum": 1, "description": "Brush diameter as a fraction of the image's longer side."},
                "hardness": {"type": "number", "minimum": 0, "maximum": 1, "description": "Share of the radius at full strength; the rest fades out."},
                "erase": {"type": "boolean"},
                "points": {"type": "array", "minItems": 1, "maxItems": nodecraft_actions::mask::MAX_POINTS, "items": point, "description": "[x, y] from 0 to 1 across the image's width and height."}}}}}})
}

fn create_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "from", "expand", "mask", "edges", "pos"],
        "properties": {"canvas": uuid(), "expand": uuid(), "mask": uuid(), "pos": pos(),
            "edges": {"type": "array", "minItems": 2, "maxItems": 2, "items": uuid()},
            "from": {"type": "object", "required": ["node", "port"], "additionalProperties": false, "properties": {"node": uuid(), "port": {"type": "string", "maxLength": 256}}}}})
}

fn edit_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "mask", "id", "edges", "pos", "prompt"],
        "properties": {"canvas": uuid(), "mask": uuid(), "id": uuid(), "pos": pos(),
            "edges": {"type": "array", "minItems": 2, "maxItems": 2, "items": uuid()},
            "prompt": {"type": "string", "maxLength": 1024 * 1024},
            "model": {"type": "string", "maxLength": 256}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "node.mask_set",
            label: "Paint mask",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{canvas, id, strokes: [{size, hardness, erase?, points: [[x, y], …]}]} -> {changed}; a mask node's brush strokes, in order, normalized to its image (x and y from 0 to 1; size is the brush diameter as a fraction of the longer side). Painting marks what a model may change; erase strokes take it back. Replaces all strokes as one undo step.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "mask.create",
            label: "Inpaint / expand",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: create_schema,
            params_doc: "{canvas, from: {node, port}, expand: new NodeId, mask: new NodeId, edges: [new EdgeId, new EdgeId], pos: {x, y}} -> {expand, mask}; adds an Expand canvas node taking the image output `from` and a Mask node over the expanded image (its transparent margins masked), as one undo step. Paint with node.mask_set, add margins with node.param_set on the Expand node (left/top/right/bottom pixels), then mask.edit. The caller supplies the new ids.",
            enabled: enabled_create,
            run: create,
        },
        CommandSpec {
            id: "mask.edit",
            label: "Create masked edit",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: edit_schema,
            params_doc: "{canvas, mask: mask NodeId, id: new NodeId, edges: [new EdgeId, new EdgeId], pos: {x, y}, prompt, model?} -> {id}; adds a generation card wired to the mask's image and the mask, with the prompt, as one undo step. The model must take a mask (the first that does when omitted). It does not run: run the new card as usual, which shows its cost first.",
            enabled: enabled_edit,
            run: edit,
        },
    ]
}

#[cfg(test)]
mod tests;
