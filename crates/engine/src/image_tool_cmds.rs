//! The image tools menu (#103): shortcuts on an image card that create the right node, already
//! wired. `image.tools` lists every tool for one image output, with whether it can be used there
//! and why not, the models that can do it and the command that makes it; `image.tool` makes the
//! ones that are a single new node (upscale, remove background, split into layers, colour grade,
//! open in 3D) as one undo step. Inpaint and expand go through the mask editor (`mask.create`,
//! `mask.edit`), Make Element through `element.create`, and Split into grid is a host action.
//!
//! A model tool is offered only when the catalog has a model for it: one with a mode of the
//! tool's id ([`Tool::modes`]) that needs nothing but the one image (a prompt it can do without
//! is fine). Nothing here runs: the new card runs, and is priced, like any other.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use crate::mask_cmds::{beside, inpaint_models};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Port, Position};
use nodecraft_nodes::generation::{GenerationMode, KIND, MESH_KIND, MODEL};
use nodecraft_nodes::layers;
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// The colour grade action.
const GRADE: &str = "image.grade";

/// One entry of the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Inpaint,
    Expand,
    RemoveBackground,
    Upscale,
    SplitGrid,
    SplitLayers,
    Grade,
    Element,
    #[serde(rename = "to_3d")]
    To3d,
}

impl Tool {
    /// Every tool, in menu order.
    pub const ALL: [Tool; 9] =
        [Tool::Inpaint, Tool::Expand, Tool::RemoveBackground, Tool::Upscale, Tool::SplitGrid, Tool::SplitLayers, Tool::Grade, Tool::Element, Tool::To3d];

    pub fn id(self) -> &'static str {
        match self {
            Tool::Inpaint => "inpaint",
            Tool::Expand => "expand",
            Tool::RemoveBackground => "remove_background",
            Tool::Upscale => "upscale",
            Tool::SplitGrid => "split_grid",
            Tool::SplitLayers => "split_layers",
            Tool::Grade => "grade",
            Tool::Element => "element",
            Tool::To3d => "to_3d",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Inpaint => "Inpaint…",
            Tool::Expand => "Expand…",
            Tool::RemoveBackground => "Remove background",
            Tool::Upscale => "Upscale",
            Tool::SplitGrid => "Split into grid…",
            Tool::SplitLayers => "Split into layers",
            Tool::Grade => "Color grade",
            Tool::Element => "Make Element…",
            Tool::To3d => "Open in 3D",
        }
    }

    /// The command that makes it; `None` for a host action (Split into grid).
    pub fn command(self) -> Option<&'static str> {
        match self {
            Tool::Inpaint | Tool::Expand => Some("mask.create"),
            Tool::Element => Some("element.create"),
            Tool::SplitGrid => None,
            Tool::RemoveBackground | Tool::Upscale | Tool::SplitLayers | Tool::Grade | Tool::To3d => Some("image.tool"),
        }
    }

    /// The generation kind and catalog mode ids of a model tool.
    fn modes(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            Tool::RemoveBackground => Some((KIND, &["remove-background"])),
            Tool::Upscale => Some((KIND, &["upscale"])),
            Tool::SplitLayers => Some((KIND, &["image-to-layers"])),
            Tool::To3d => Some((MESH_KIND, &["image-to-mesh"])),
            _ => None,
        }
    }
}

/// The input role of `mode` the image goes to, when the image is all it needs.
fn image_role(mode: &GenerationMode) -> Option<&str> {
    let mut image = None;
    for role in &mode.inputs {
        if role.modality == Modality::Image && role.min <= 1 && role.max >= 1 && image.is_none() {
            image = Some(role.role.as_str());
        } else if role.min > 0 {
            return None;
        }
    }
    image
}

/// The models that can do `tool`, in catalog order, each with the input the image goes to.
pub fn tool_models(catalog: &GenerationCatalog, tool: Tool) -> Vec<(String, String)> {
    let Some((kind, ids)) = tool.modes() else { return Vec::new() };
    catalog
        .models(kind)
        .filter_map(|model| {
            let role = model.modes.iter().filter(|mode| ids.contains(&mode.id.as_str())).find_map(image_role)?;
            Some((model.id.clone(), role.to_owned()))
        })
        .collect()
}

/// The image output `from` names on `canvas`, checked.
fn image_output<'a>(canvas: &'a Canvas, from: &Endpoint) -> Result<(&'a Node, &'a Port), Error> {
    let node = canvas.nodes().get(&from.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "the image node does not exist"))?;
    let port = node.outputs.iter().find(|p| p.name == from.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "the image node has no such output"))?;
    if port.port_type.modality != Modality::Image {
        return Err(invalid("pick an image output"));
    }
    Ok((node, port))
}

/// Why `tool` can't be used on `port` now, if it can't.
fn unavailable(catalog: Option<&GenerationCatalog>, tool: Tool, port: &Port) -> Option<String> {
    let single = !port.port_type.many;
    match tool {
        // These take one image, so a card that makes several can't feed them.
        Tool::Inpaint | Tool::Expand | Tool::Grade | Tool::SplitGrid if !single => Some("works on one image: use it on a single image".into()),
        Tool::Inpaint if catalog.is_none_or(|catalog| inpaint_models(catalog).is_empty()) => Some("no model that edits with a mask is available".into()),
        Tool::Element if !single => Some("works on single images".into()),
        Tool::RemoveBackground | Tool::Upscale | Tool::SplitLayers | Tool::To3d if catalog.is_none_or(|catalog| tool_models(catalog, tool).is_empty()) => {
            Some(format!("no model that can {} is available", verb(tool)))
        }
        _ => None,
    }
}

/// One entry of the menu for an image output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub tool: Tool,
    /// Why it can't be used there now; `None` when it can.
    pub reason: Option<String>,
    /// The catalog models that can do it, the default first.
    pub models: Vec<String>,
}

/// The menu for the image output `from` on `canvas`, in menu order.
pub fn menu(canvas: &Canvas, catalog: Option<&GenerationCatalog>, from: &Endpoint) -> Result<Vec<Entry>, Error> {
    let (_, port) = image_output(canvas, from)?;
    Ok(Tool::ALL
        .iter()
        .map(|&tool| {
            let models = match (tool, catalog) {
                (Tool::Inpaint | Tool::Expand, Some(catalog)) => inpaint_models(catalog),
                (_, Some(catalog)) => tool_models(catalog, tool).into_iter().map(|(id, _)| id).collect(),
                (_, None) => Vec::new(),
            };
            Entry { tool, reason: unavailable(catalog, tool, port), models }
        })
        .collect())
}

fn verb(tool: Tool) -> &'static str {
    match tool {
        Tool::RemoveBackground => "remove a background",
        Tool::Upscale => "upscale an image",
        Tool::SplitLayers => "split an image into layers",
        Tool::To3d => "make a 3D model from an image",
        _ => "do that",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsParams {
    canvas: CanvasId,
    from: Endpoint,
}

fn tools(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ToolsParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let entries: Vec<Json> = menu(canvas, c.resources.get::<GenerationCatalog>(), &p.from)?
        .into_iter()
        .map(|e| {
            let tool = e.tool;
            json!({"tool": tool.id(), "label": tool.label(), "available": e.reason.is_none(), "reason": e.reason, "command": tool.command(), "models": e.models})
        })
        .collect();
    Ok(Effect::None { result: json!({"tools": entries}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolParams {
    canvas: CanvasId,
    tool: Tool,
    /// The image output it works on.
    from: Endpoint,
    /// The new node's id; the caller supplies it so replay stays deterministic.
    id: NodeId,
    /// The new connections' ids: one, and a second for Split into layers (card to editor).
    edges: Vec<EdgeId>,
    /// Where the new node goes; Split into layers puts its editor one card to the right.
    pos: Position,
    /// Split into layers: the Layer Editor's id.
    #[serde(default)]
    editor: Option<NodeId>,
    /// The model; the first one that can do it when absent.
    #[serde(default)]
    model: Option<String>,
}

fn tool(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ToolParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    if p.tool.command() != Some("image.tool") {
        return Err(invalid(match p.tool.command() {
            Some(_) => "use that tool's own command (see image.tools)",
            None => "that tool is a host action",
        }));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let (_, port) = image_output(canvas, &p.from)?;
    if let Some(reason) = unavailable(c.resources.get::<GenerationCatalog>(), p.tool, port) {
        return Err(Error::new(ErrorKind::Unsupported, reason));
    }
    let wanted_edges = if p.tool == Tool::SplitLayers { 2 } else { 1 };
    if p.edges.len() != wanted_edges {
        return Err(invalid(if wanted_edges == 2 { "Split into layers makes two connections: give two edge ids" } else { "give one edge id" }));
    }
    if (p.tool == Tool::SplitLayers) != p.editor.is_some() {
        return Err(invalid("give an editor id for Split into layers, and only for it"));
    }
    if p.editor == Some(p.id) {
        return Err(invalid("the editor and the card need different ids"));
    }
    if p.tool == Tool::Grade && p.model.is_some() {
        return Err(invalid("Color grade runs locally: it takes no model"));
    }
    let mut changes = Vec::new();
    let edge = |id: EdgeId, from: Endpoint, node: NodeId, port: &str| Edge { id, from, to: Endpoint { node, port: port.into() }, order: 0 };
    let first = p.edges.first().copied().ok_or_else(|| invalid("give one edge id"))?;
    let mut result = json!({"id": p.id});
    if p.tool == Tool::Grade {
        let mut node = c.kinds.get(GRADE)?.instantiate_instance(p.id, BTreeMap::new(), &ParamValues::new())?;
        node.pos = p.pos;
        changes.push(Change::AddNode { canvas: p.canvas, node });
        changes.push(Change::Connect { canvas: p.canvas, edge: edge(first, p.from, p.id, "image") });
    } else {
        let catalog = GenerationCatalog::current(c.resources)?;
        let models = tool_models(catalog, p.tool);
        let (model, role) = match p.model {
            Some(model) => models
                .into_iter()
                .find(|(id, _)| *id == model)
                .ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("that model can't {}", verb(p.tool))))?,
            None => models.into_iter().next().ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("no model that can {} is available", verb(p.tool))))?,
        };
        let kind = if p.tool == Tool::To3d { MESH_KIND } else { KIND };
        let mut supplied = ParamValues::new();
        supplied.insert(MODEL.into(), Value::Text(model.clone()));
        let mut card = c.kinds.get(kind)?.instantiate_instance(p.id, BTreeMap::new(), &supplied)?;
        card.pos = p.pos;
        let out = card.outputs.first().map(|o| o.name.clone()).ok_or_else(|| Error::new(ErrorKind::Internal, "the new card has no output"))?;
        changes.push(Change::AddNode { canvas: p.canvas, node: card });
        changes.push(Change::Connect { canvas: p.canvas, edge: edge(first, p.from, p.id, &role) });
        if let (Some(editor_id), Some(second)) = (p.editor, p.edges.get(1).copied()) {
            let mut editor = c.kinds.get(layers::ID)?.instantiate_instance(editor_id, BTreeMap::new(), &ParamValues::new())?;
            editor.pos = beside(p.pos, 1.0);
            changes.push(Change::AddNode { canvas: p.canvas, node: editor });
            changes.push(Change::Connect { canvas: p.canvas, edge: edge(second, Endpoint { node: p.id, port: out }, editor_id, "layers") });
            result["editor"] = json!(editor_id);
        }
        result["model"] = json!(model);
    }
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

fn enabled_tool(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    tool(c, value).map(|_| ())
}

fn enabled_tools(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    tools(c, value).map(|_| ())
}

fn endpoint_schema() -> Json {
    json!({"type": "object", "required": ["node", "port"], "additionalProperties": false,
        "properties": {"node": {"type": "string", "format": "uuid"}, "port": {"type": "string", "maxLength": 256}}})
}

fn tools_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "from"], "properties": {"canvas": {"type": "string", "format": "uuid"}, "from": endpoint_schema()}})
}

fn tool_schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "tool", "from", "id", "edges", "pos"],
        "properties": {"canvas": uuid, "id": uuid, "editor": uuid, "from": endpoint_schema(),
            "tool": {"enum": ["remove_background", "upscale", "split_layers", "grade", "to_3d"]},
            "edges": {"type": "array", "minItems": 1, "maxItems": 2, "items": uuid},
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "model": {"type": "string", "maxLength": 128}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "image.tools",
            label: "Image tools",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: tools_schema,
            params_doc: "{canvas, from: {node, port}} -> {tools: [{tool, label, available, reason, command, models}]}; the image tools menu for one image output, in menu order: inpaint, expand, remove_background, upscale, split_grid, split_layers, grade, element, to_3d. `command` makes the tool (null: a host action); `models` are the catalog models that can do it, the first being the default. Read-only.",
            enabled: enabled_tools,
            run: tools,
        },
        CommandSpec {
            id: "image.tool",
            label: "Use image tool",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: tool_schema,
            params_doc: "{canvas, tool: remove_background|upscale|split_layers|grade|to_3d, from: {node, port}, id: new NodeId, edges: [new EdgeId], pos: {x, y}, editor?: new NodeId, model?} -> {id, model?, editor?}; adds the tool's node with the image wired in, as one undo step: a generation card with a model that can do it (the first one when `model` is absent; a 3D card for to_3d), or a Color grade action. split_layers also adds a Layer Editor (`editor`) fed by the card, and takes two edge ids. Nothing runs.",
            enabled: enabled_tool,
            run: tool,
        },
    ]
}

#[cfg(test)]
mod tests;
