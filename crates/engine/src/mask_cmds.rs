//! Inpainting and outpainting (#102). `mask.create` puts an Expand canvas and a Mask node behind
//! an image (the mask editor paints on the expanded image), `node.mask_set` stores the painted
//! strokes as one undo step, and `mask.edit` wires the image and its mask into a new generation
//! card of a model that takes a mask. `mask.margins_set` changes the expanded canvas and moves the
//! strokes of the masks painted over it, so they keep covering the same part of the image.
//! Nothing here runs: the new card runs (and is costed) like any other.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use nodecraft_actions::image::{ImageLimits, Raster};
use nodecraft_actions::mask::{self, MAX_MARGIN, MAX_STROKES, MIN_SIZE, Margins, MaskOptions, Stroke, Strokes};
use nodecraft_graph::{Change, Changeset, Edge, Endpoint, Node, Position};
use nodecraft_nodes::generation::{KIND, MODEL, PROMPT};
use nodecraft_nodes::mask::{CONFIG, ID};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamValues, Unsigned, Value};
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
pub fn beside(pos: Position, steps: f32) -> Position {
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

/// The margins an Expand canvas node adds, each at most [`MAX_MARGIN`].
pub fn margins(node: &Node) -> Margins {
    let side = |name: &str| match node.params.get(name) {
        Some(Value::Unsigned(v)) => u32::try_from(v.0).unwrap_or(MAX_MARGIN).min(MAX_MARGIN),
        _ => 0,
    };
    Margins { left: side("left"), top: side("top"), right: side("right"), bottom: side("bottom") }
}

/// `strokes` painted over a `size` image expanded by `from`, moved to cover the same part of the
/// image once it is expanded by `to` instead. The brush keeps its size in image pixels.
pub fn remap(strokes: &Strokes, size: (u32, u32), from: Margins, to: Margins) -> Strokes {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let frame = |m: Margins| (w + f64::from(m.left) + f64::from(m.right), h + f64::from(m.top) + f64::from(m.bottom));
    let (old, new) = (frame(from), frame(to));
    let axis = |v: f64, old: f64, before: u32, after: u32, new: f64| ((v * old - f64::from(before) + f64::from(after)) / new).clamp(-1.0, 2.0);
    let scale = old.0.max(old.1) / new.0.max(new.1);
    let strokes = strokes
        .strokes
        .iter()
        .map(|s| Stroke {
            size: (s.size * scale).clamp(MIN_SIZE, 1.0),
            hardness: s.hardness,
            erase: s.erase,
            points: s.points.iter().map(|[x, y]| [axis(*x, old.0, from.left, to.left, new.0), axis(*y, old.1, from.top, to.top, new.1)]).collect(),
        })
        .collect();
    Strokes { strokes }
}

/// Whether `mask` marks anything for a model to change, given whether its image has margins.
fn marks(mask: &Node, margins: bool) -> Result<bool, Error> {
    let flag = |name: &str| matches!(mask.params.get(name), Some(Value::Bool(true)));
    if flag("invert") || (flag("transparent") && margins) {
        return Ok(true);
    }
    let strokes = strokes(mask)?;
    if !strokes.paints() {
        return Ok(false);
    }
    // Erasing can take back everything painted: render small and look.
    const SIDE: u32 = 64;
    let limits = ImageLimits::default();
    let unreadable = |e| Error::new(ErrorKind::InvalidInput, format!("the node's mask can't be read: {e}"));
    let blank = Raster::new(SIDE, SIDE, [0, 0, 0, 255].repeat((SIDE * SIDE) as usize), limits).map_err(unreadable)?;
    let rendered = mask::render(&strokes, MaskOptions::default(), &blank, limits).map_err(unreadable)?;
    Ok(rendered.pixels().as_chunks::<4>().0.iter().any(|[.., alpha]| *alpha < 255))
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
    // Something must be marked: a painted stroke, the margins an expanded canvas adds, or
    // everything (inverted).
    let margins = canvas.nodes().get(&image.node).is_some_and(|n| n.kind == EXPAND && !margins(n).is_empty());
    if !marks(mask, margins)? {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarginsParams {
    canvas: CanvasId,
    /// The Expand canvas node.
    id: NodeId,
    margins: Margins,
}

fn margins_set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: MarginsParams = params(value)?;
    let m = p.margins;
    if [m.left, m.top, m.right, m.bottom].iter().any(|side| *side > MAX_MARGIN) {
        return Err(invalid("a margin is at most 4096 pixels"));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| not_found("canvas"))?;
    let expand = canvas.nodes().get(&p.id).ok_or_else(|| not_found("document entity"))?;
    if expand.kind != EXPAND {
        return Err(invalid("only an Expand canvas node has margins"));
    }
    let from = margins(expand);
    if from == m {
        return Ok(Effect::None { result: json!({"changed": false}) });
    }
    let mut changed = expand.clone();
    for (side, v) in [("left", m.left), ("top", m.top), ("right", m.right), ("bottom", m.bottom)] {
        changed.params.insert(side.into(), Value::Unsigned(Unsigned(u64::from(v))));
    }
    let mut changes = vec![Change::SetNode { canvas: p.canvas, node: changed }];
    // The masks painted over this canvas keep covering the same part of the image.
    let size = canvas
        .edges()
        .values()
        .find(|e| e.to.node == p.id && e.to.port == "image")
        .and_then(|e| canvas.nodes().get(&e.from.node))
        .and_then(crate::split::image_of)
        .and_then(|asset| Some((asset.meta.width?, asset.meta.height?)));
    let fed = canvas.edges().values().filter(|e| e.from.node == p.id && e.to.port == "image").filter_map(|e| canvas.nodes().get(&e.to.node));
    for mask in fed.filter(|n| n.kind == ID) {
        let painted = strokes(mask)?;
        if painted.strokes.is_empty() {
            continue;
        }
        let size = size.ok_or_else(|| invalid("run the image first: a painted mask can't be moved to new margins without the image's size"))?;
        let json = remap(&painted, size, from, m).encode().map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the mask isn't valid: {e}")))?;
        let mut moved = mask.clone();
        moved.config.insert(CONFIG.into(), Value::Text(json));
        changes.push(Change::SetNode { canvas: p.canvas, node: moved });
    }
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"changed": true}), gesture: None })
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

fn margins_schema() -> Json {
    let side = json!({"type": "integer", "minimum": 0, "maximum": MAX_MARGIN});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "margins"],
        "properties": {"canvas": uuid(), "id": uuid(),
            "margins": {"type": "object", "additionalProperties": false, "required": ["left", "top", "right", "bottom"],
                "properties": {"left": side, "top": side, "right": side, "bottom": side}}}})
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
            params_doc: "{canvas, from: {node, port}, expand: new NodeId, mask: new NodeId, edges: [new EdgeId, new EdgeId], pos: {x, y}} -> {expand, mask}; adds an Expand canvas node taking the image output `from` and a Mask node over the expanded image (its transparent margins masked), as one undo step. Paint with node.mask_set, add margins with mask.margins_set (node.param_set on the Expand node doesn't move painted strokes), then mask.edit. The caller supplies the new ids.",
            enabled: enabled_create,
            run: create,
        },
        CommandSpec {
            id: "mask.margins_set",
            label: "Expand canvas",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: margins_schema,
            params_doc: "{canvas, id: Expand canvas NodeId, margins: {left, top, right, bottom}} -> {changed}; sets the pixels an Expand canvas node adds on each side (0 to 4096) and moves the strokes of the masks painted over it so they cover the same part of the image, as one undo step. A painted mask needs its image's size: run the image first.",
            enabled: always,
            run: margins_set,
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
