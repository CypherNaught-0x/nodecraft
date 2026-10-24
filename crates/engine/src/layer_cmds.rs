//! The Layer Editor's commands (#101). A Layer Editor node keeps its layers in its config (see
//! `nodecraft-nodes::layers`); every image wired into it has a layer, stored or automatic, and
//! text and shape layers are added on top. `layers.list` shows them all, bottom first; every
//! other command changes them as one undo step. A stored image layer whose wire is gone is still
//! listed (and not drawn) until it is deleted or its image is wired in again. Editing stores the automatic image layers too,
//! so each keeps its place in the stack from then on. Nothing here runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_actions::layers::{self, Content, Frame, Layer, Layers};
use nodecraft_graph::{Change, Changeset, Node};
use nodecraft_nodes::layers::{CONFIG, ID, INPUT};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

fn unusable(e: nodecraft_actions::image::ImageError) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the layers aren't valid: {e}"))
}

fn editor<'a>(c: &'a Context<'_>, canvas: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    let node = c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))?.nodes().get(&id).ok_or_else(|| not_found("document entity"))?;
    if node.kind == ID { Ok(node) } else { Err(invalid("only a Layer Editor node has layers")) }
}

/// The layers a Layer Editor node stores, checked.
pub fn stored(node: &Node) -> Result<Layers, Error> {
    Layers::parse(nodecraft_nodes::layers::stored(node)?).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the node's layers can't be read: {e}")))
}

/// The frame a Layer Editor node draws into, from its parameters (defaults where unset).
pub fn frame(c: &Context<'_>, node: &Node) -> Result<Frame, Error> {
    let kind = c.kinds.get(ID)?;
    let values = kind.validate_instance(node, true)?.into_visible_values();
    crate::action_exec::layers::frame(&values).map_err(|e| Error::new(ErrorKind::InvalidInput, e))
}

/// The node each image wired into a Layer Editor node comes from, in input order, one entry per
/// image as a run plans them (routers resolved; see `inputs`).
pub fn sources(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Vec<String>, Error> {
    Ok(inputs(c, canvas, id)?.into_iter().map(|(origin, _)| origin).collect())
}

/// Most images the editor lists from its wires; a run refuses more than `MAX_LAYERS` anyway.
const MAX_INPUTS: usize = 4 * layers::MAX_LAYERS;

/// The images wired in now, in input order, each with the node it comes from: a source image's
/// asset, or each image of the upstream node's active result on the wired port, so a wire that
/// carries a list gives one entry per item, as a run does. A node with no result yet counts as
/// one image, None. For previews and layer identity; a run resolves its inputs itself.
pub fn inputs(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Vec<(String, Option<AssetRef>)>, Error> {
    let canvas = c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))?;
    let incoming = nodecraft_exec::routing::incoming(canvas, c.kinds)?;
    let feeds = incoming.feeds.get(&id).into_iter().flatten().filter(|(port, ..)| *port == INPUT);
    let mut out = Vec::new();
    for (_, _, from, from_port) in feeds {
        let images = canvas.nodes().get(from).map(|node| shown(node, from_port)).unwrap_or_else(|| vec![None]);
        out.extend(images.into_iter().map(|image| (from.to_string(), image.cloned())));
        if out.len() > MAX_INPUTS {
            out.truncate(MAX_INPUTS);
            break;
        }
    }
    Ok(out)
}

/// The images `node` sends on `port` now, one per value a run receives: each item of a list (a
/// batch's per-item lists flattened, as the planner collects them), and one None without a result.
fn shown<'a>(node: &'a Node, port: &str) -> Vec<Option<&'a AssetRef>> {
    if node.kind == "source.image" {
        return vec![crate::split::image_of(node)];
    }
    let result = node.outputs.iter().position(|p| p.name == port).and_then(|index| Some((node.history.iter().find(|g| Some(g.id) == node.active)?, index)));
    let Some(value) = result.and_then(|(result, index)| result.outputs.get(index).map(|v| (v, !result.items.is_empty()))) else {
        return vec![None];
    };
    match value {
        (Value::List(items), fanned) => items
            .iter()
            .flat_map(|item| match item {
                Value::List(inner) if fanned => inner.iter().collect::<Vec<_>>(),
                single => vec![single],
            })
            .take(MAX_INPUTS.saturating_add(1))
            .map(crate::split::image)
            .collect(),
        (single, _) => vec![crate::split::image(single)],
    }
}

/// Every layer the node shows, stored and automatic, bottom first.
pub fn listed(c: &Context<'_>, canvas: CanvasId, node: &Node) -> Result<Layers, Error> {
    Ok(stored(node)?.with_sources(&sources(c, canvas, node.id)?, frame(c, node)?))
}

fn store(canvas: CanvasId, node: &Node, layers: Layers) -> Result<Effect, Error> {
    let json = layers.encode().map_err(unusable)?;
    let mut changed = node.clone();
    changed.config.insert(CONFIG.into(), Value::Text(json));
    if changed == *node {
        return Ok(Effect::None { result: json!({"changed": false}) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas, node: changed }]), result: json!({"changed": true}), gesture: None })
}

fn index_of(layers: &Layers, id: &str) -> Result<usize, Error> {
    layers.layers.iter().position(|l| l.id == id).ok_or_else(|| not_found("layer"))
}

fn always(_: &Context<'_>, _: &Json) -> Result<(), Error> {
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeParams {
    canvas: CanvasId,
    id: NodeId,
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: NodeParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let frame = frame(c, node)?;
    let layers = listed(c, p.canvas, node)?;
    let layers = serde_json::to_value(&layers.layers).map_err(|_| invalid("the layers can't be listed"))?;
    let inputs: Vec<Json> = inputs(c, p.canvas, p.id)?.into_iter().map(|(origin, image)| json!({"origin": origin, "image": image})).collect();
    let fonts = fonts::listed(node);
    Ok(Effect::None { result: json!({"width": frame.width, "height": frame.height, "layers": layers, "inputs": inputs, "fonts": fonts}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    canvas: CanvasId,
    id: NodeId,
    layers: Vec<Layer>,
}

fn set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SetParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    if p.layers.iter().any(|l| matches!(l.content, Content::Image { .. })) {
        return Err(invalid("an image layer names its image's node (kind \"source\"), not an input"));
    }
    store(p.canvas, node, Layers { layers: p.layers })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    canvas: CanvasId,
    id: NodeId,
    layer: Layer,
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    if !matches!(p.layer.content, Content::Text { .. } | Content::Shape { .. }) {
        return Err(invalid("only text and shape layers are added; an image gets its layer when it is wired in"));
    }
    let mut layers = listed(c, p.canvas, node)?;
    if layers.get(&p.layer.id).is_some() {
        return Err(Error::new(ErrorKind::Conflict, "a layer with that id already exists"));
    }
    let id = p.layer.id.clone();
    layers.layers.push(p.layer);
    let effect = store(p.canvas, node, layers)?;
    Ok(with_result(effect, json!({"changed": true, "layer": id})))
}

fn with_result(effect: Effect, result: Json) -> Effect {
    match effect {
        Effect::Edit { changes, gesture, .. } => Effect::Edit { changes, result, gesture },
        other => other,
    }
}

/// The fields a locked layer keeps.
pub(crate) const GEOMETRY: [&str; 7] = ["x", "y", "width", "height", "rotation", "flip_x", "flip_y"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    canvas: CanvasId,
    id: NodeId,
    layer: String,
    set: Map<String, Json>,
}

fn update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpdateParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut layers = listed(c, p.canvas, node)?;
    let at = index_of(&layers, &p.layer)?;
    let current = layers.layers.get(at).ok_or_else(|| not_found("layer"))?;
    if p.set.contains_key("id") {
        return Err(invalid("a layer's id doesn't change"));
    }
    // Unlocking in the same update frees the geometry.
    let unlocking = p.set.get("locked") == Some(&Json::Bool(false));
    if current.locked && !unlocking && GEOMETRY.iter().any(|f| p.set.contains_key(*f)) {
        return Err(invalid("the layer is locked: unlock it to move or resize it"));
    }
    let mut merged = serde_json::to_value(current).map_err(|_| invalid("the layer can't be read"))?;
    let Json::Object(fields) = &mut merged else { return Err(invalid("the layer can't be read")) };
    for (key, value) in p.set {
        match (key.as_str(), fields.get_mut("content"), value) {
            ("content", Some(Json::Object(content)), Json::Object(changes)) => {
                if changes.get("kind").is_some_and(|k| Some(k) != content.get("kind")) {
                    return Err(invalid("a layer's kind doesn't change: add a new layer instead"));
                }
                content.extend(changes);
            }
            (_, _, value) => {
                fields.insert(key, value);
            }
        }
    }
    let changed: Layer = serde_json::from_value(merged).map_err(|_| invalid("the change isn't valid: a field is unknown or has the wrong type or value"))?;
    if let Some(slot) = layers.layers.get_mut(at) {
        *slot = changed;
    }
    store(p.canvas, node, layers)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerParams {
    canvas: CanvasId,
    id: NodeId,
    layer: String,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: LayerParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let (sources, frame) = (sources(c, p.canvas, node.id)?, frame(c, node)?);
    let mut layers = stored(node)?.with_sources(&sources, frame);
    let at = index_of(&layers, &p.layer)?;
    if layers.layers.get(at).is_some_and(|l| Layers::is_wired(l, &sources)) {
        return Err(invalid("an image's layer stays while the image is wired in: hide it, or disconnect the image"));
    }
    layers.layers.remove(at);
    store(p.canvas, node, layers)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReorderParams {
    canvas: CanvasId,
    id: NodeId,
    layer: String,
    /// The new place in the stack, 0 at the bottom; past the top means the top.
    index: u32,
}

fn reorder(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ReorderParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut layers = listed(c, p.canvas, node)?;
    let at = index_of(&layers, &p.layer)?;
    let layer = layers.layers.remove(at);
    let to = usize::try_from(p.index).unwrap_or(usize::MAX).min(layers.layers.len());
    layers.layers.insert(to, layer);
    store(p.canvas, node, layers)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlignParams {
    canvas: CanvasId,
    id: NodeId,
    layers: Vec<String>,
    to: layers::Edge,
}

fn align(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AlignParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    if p.layers.is_empty() || p.layers.len() > layers::MAX_LAYERS {
        return Err(invalid("align one to 64 layers"));
    }
    let frame = frame(c, node)?;
    let mut layers = listed(c, p.canvas, node)?;
    let mut picked = Vec::with_capacity(p.layers.len());
    for id in &p.layers {
        let at = index_of(&layers, id)?;
        if !picked.contains(&at) {
            picked.push(at);
        }
    }
    let skipped = layers::align(&mut layers.layers, &picked, p.to, (f64::from(frame.width), f64::from(frame.height)));
    let effect = store(p.canvas, node, layers)?;
    let changed = matches!(effect, Effect::Edit { .. });
    Ok(match effect {
        Effect::Edit { changes, gesture, .. } => Effect::Edit { changes, result: json!({"changed": changed, "locked": skipped}), gesture },
        _ => Effect::None { result: json!({"changed": false, "locked": skipped}) },
    })
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn layer_id() -> Json {
    json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$"})
}

fn color() -> Json {
    json!({"type": "string", "pattern": "^#([0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$"})
}

fn content_schema() -> Json {
    let size = json!({"type": "number", "minimum": 1, "maximum": 1000});
    json!({"oneOf": [
        {"type": "object", "additionalProperties": false, "required": ["kind", "text", "size", "color"], "properties": {
            "kind": {"const": "text"},
            "text": {"type": "string", "maxLength": layers::MAX_TEXT_BYTES},
            "font": {"type": "string", "maxLength": 128, "description": "The font's name; unknown fonts draw in the built-in one (\"lato\")."},
            "size": size,
            "line_spacing": {"type": "number", "minimum": 0.5, "maximum": 5},
            "tracking": {"type": "number", "minimum": -1, "maximum": 2, "description": "Extra space after each character, in em."},
            "align": {"enum": ["left", "center", "right"]},
            "color": color()}},
        {"type": "object", "additionalProperties": false, "required": ["kind", "shape"], "properties": {
            "kind": {"const": "shape"},
            "shape": {"enum": ["rectangle", "ellipse", "triangle"]},
            "fill": color(), "stroke": color(),
            "stroke_width": {"type": "number", "minimum": 0, "maximum": 1000}}},
        {"type": "object", "additionalProperties": false, "required": ["kind", "origin", "nth"], "properties": {
            "kind": {"const": "source"},
            "origin": {"type": "string", "maxLength": 64, "description": "The id of the node the image comes from."},
            "nth": {"type": "integer", "minimum": 0, "description": "Which of that node's images, from 0."},
            "fit": {"type": "boolean", "description": "Keep the image's proportions, centred in the box."}}}
    ]})
}

fn layer_properties() -> Map<String, Json> {
    let coordinate = json!({"type": "number", "minimum": -100_000, "maximum": 100_000});
    let extent = json!({"type": "number", "exclusiveMinimum": 0, "maximum": 100_000});
    let properties = json!({
        "name": {"type": "string", "maxLength": 128},
        "visible": {"type": "boolean"}, "locked": {"type": "boolean"},
        "opacity": {"type": "number", "minimum": 0, "maximum": 1},
        "x": coordinate, "y": coordinate, "width": extent, "height": extent,
        "rotation": {"type": "number", "minimum": -3600, "maximum": 3600, "description": "Degrees, clockwise."},
        "flip_x": {"type": "boolean"}, "flip_y": {"type": "boolean"},
    });
    match properties {
        Json::Object(map) => map,
        _ => Map::new(),
    }
}

fn layer_schema() -> Json {
    let mut properties = layer_properties();
    properties.insert("id".into(), layer_id());
    properties.insert("content".into(), content_schema());
    json!({"type": "object", "additionalProperties": false, "required": ["id", "name", "x", "y", "width", "height", "content"], "properties": properties})
}

fn object(required: &[&str], mut properties: Map<String, Json>) -> Json {
    properties.insert("canvas".into(), uuid());
    properties.insert("id".into(), uuid());
    let mut required: Vec<&str> = required.to_vec();
    required.splice(0..0, ["canvas", "id"]);
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn props(value: Json) -> Map<String, Json> {
    match value {
        Json::Object(map) => map,
        _ => Map::new(),
    }
}

fn list_schema() -> Json {
    object(&[], Map::new())
}

fn set_schema() -> Json {
    object(&["layers"], props(json!({"layers": {"type": "array", "maxItems": layers::MAX_LAYERS, "items": layer_schema()}})))
}

fn add_schema() -> Json {
    object(&["layer"], props(json!({"layer": layer_schema()})))
}

fn update_schema() -> Json {
    let mut fields = layer_properties();
    fields.insert("content".into(), json!({"type": "object", "description": "Content fields to change; the kind stays."}));
    object(&["layer", "set"], props(json!({"layer": layer_id(), "set": {"type": "object", "additionalProperties": false, "properties": fields}})))
}

fn layer_schema_only() -> Json {
    object(&["layer"], props(json!({"layer": layer_id()})))
}

fn reorder_schema() -> Json {
    object(&["layer", "index"], props(json!({"layer": layer_id(), "index": {"type": "integer", "minimum": 0, "maximum": layers::MAX_LAYERS}})))
}

fn align_schema() -> Json {
    object(
        &["layers", "to"],
        props(
            json!({"layers": {"type": "array", "minItems": 1, "maxItems": layers::MAX_LAYERS, "items": layer_id()}, "to": {"enum": ["left", "center", "right", "top", "middle", "bottom"]}}),
        ),
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "layers.list",
            label: "List layers",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas, id: Layer Editor NodeId} -> {width, height, layers, inputs, fonts}; the node's frame size and every layer, bottom first: the stored ones and an automatic one for each wired image no stored layer names (content kind \"source\", fitted to the frame). x and y are the layer's centre in frame pixels; rotation is in degrees, clockwise. inputs lists each wired image in input order as {origin, image}: the node it comes from and the image it shows now (null until that node has a result); a source layer shows the nth input from its origin. fonts lists the fonts the node holds as {name, font}, in the order added (see layers.font_add).",
            enabled: always,
            run: list,
        },
        CommandSpec {
            id: "node.layers_set",
            label: "Set layers",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{canvas, id, layers: [layer, …]} -> {changed}; replaces a Layer Editor node's layers, bottom first, as one undo step. A layer is {id, name, visible?, locked?, opacity?, x, y, width, height, rotation?, flip_x?, flip_y?, content}; content is {kind: \"text\", text, font?, size, line_spacing?, tracking?, align?, color}, {kind: \"shape\", shape, fill?, stroke?, stroke_width?} or {kind: \"source\", origin, nth, fit?} for a wired image. Wired images no layer names are added on top when the node runs.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "layers.add",
            label: "Add layer",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{canvas, id, layer} -> {changed, layer}; adds a text or shape layer on top of a Layer Editor node's layers, as one undo step (see node.layers_set for the layer's fields). The caller picks the layer's id; it must be new.",
            enabled: always,
            run: add,
        },
        CommandSpec {
            id: "layers.update",
            label: "Change layer",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{canvas, id, layer: layer id, set: {fields}} -> {changed}; changes some of a layer's fields (name, visible, locked, opacity, x, y, width, height, rotation, flip_x, flip_y, or content fields other than its kind), as one undo step. A locked layer doesn't move or resize unless the same change unlocks it.",
            enabled: always,
            run: update,
        },
        CommandSpec {
            id: "layers.remove",
            label: "Delete layer",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: layer_schema_only,
            params_doc: "{canvas, id, layer: layer id} -> {changed}; deletes a layer, as one undo step. An image's layer stays while the image is wired in (hide it instead); once its wire is gone, it can be deleted.",
            enabled: always,
            run: remove,
        },
        CommandSpec {
            id: "layers.reorder",
            label: "Move layer in stack",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: reorder_schema,
            params_doc: "{canvas, id, layer: layer id, index} -> {changed}; moves a layer to `index` in the stack (0 is the bottom; past the top is the top), as one undo step.",
            enabled: always,
            run: reorder,
        },
        CommandSpec {
            id: "layers.align",
            label: "Align layers",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: align_schema,
            params_doc: "{canvas, id, layers: [layer id, …], to: left|center|right|top|middle|bottom} -> {changed, locked}; lines layers up by their bounds (rotation included): one layer against the frame, several against the box around them all, as one undo step. Locked layers stay put and are listed in `locked`.",
            enabled: always,
            run: align,
        },
    ]
    .into_iter()
    .chain(fonts::specs())
    .collect()
}

pub mod fonts;
#[cfg(test)]
mod tests;
