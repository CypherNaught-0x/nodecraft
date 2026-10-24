//! Elements (#104): reusable subjects a prompt mentions as `#name`. `element.create` makes an
//! Element from image outputs, `element.sheet` adds a generation card that turns its images
//! into one reference sheet (wired back into the Element, so the card's history and active
//! result are the Element's reference history and default), and `element.list` lists a canvas's
//! Elements and who mentions them. Nothing here runs: the sheet card runs (and is costed) like
//! any other.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use nodecraft_exec::mentions::{self, Resolved};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Position};
use nodecraft_nodes::element::{self, CATEGORIES, CATEGORY, GUIDELINES, ID, IMAGES, MAX_IMAGES, NAME, SHEET};
use nodecraft_nodes::generation::{GenerationModel, KIND, MODEL, PROMPT};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

pub(crate) fn check_pos(pos: Position) -> Result<(), Error> {
    if pos.x.is_finite() && pos.y.is_finite() && pos.x.abs() <= MAX_COORDINATE && pos.y.abs() <= MAX_COORDINATE {
        Ok(())
    } else {
        Err(invalid("position is outside the canvas"))
    }
}

pub(crate) fn canvas<'a>(c: &'a Context<'_>, id: CanvasId) -> Result<&'a Canvas, Error> {
    c.document.canvases().get(&id).ok_or_else(|| not_found("canvas"))
}

/// Whether another Element on `canvas` than `except` is called `name` (ignoring case).
pub(crate) fn taken(canvas: &Canvas, name: &str, except: Option<NodeId>) -> bool {
    canvas.nodes().values().any(|n| Some(n.id) != except && element::name(n).is_some_and(|other| other.eq_ignore_ascii_case(name)))
}

pub(crate) fn text(node: &Node, name: &str) -> String {
    match node.params.get(name) {
        Some(Value::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    canvas: CanvasId,
    /// The new Element's id; the caller supplies it so replay stays deterministic.
    id: NodeId,
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    guidelines: Option<String>,
    /// The image outputs to take as its reference images, in order.
    from: Vec<Endpoint>,
    /// One new connection id per entry of `from`.
    edges: Vec<EdgeId>,
    pos: Position,
}

fn create(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CreateParams = params(value)?;
    check_pos(p.pos)?;
    if !element::valid_name(&p.name) {
        return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
    }
    let canvas = canvas(c, p.canvas)?;
    if taken(canvas, &p.name, None) {
        return Err(Error::new(ErrorKind::Conflict, "another Element on this canvas has that name"));
    }
    if p.from.len() > MAX_IMAGES {
        return Err(invalid("an Element holds at most 8 reference images"));
    }
    if p.edges.len() != p.from.len() {
        return Err(invalid("give one new edge id per image"));
    }
    for from in &p.from {
        let source = canvas.nodes().get(&from.node).ok_or_else(|| not_found("document entity"))?;
        let port = source.outputs.iter().find(|port| port.name == from.port).ok_or_else(|| not_found("that output"))?;
        if port.port_type.modality != Modality::Image {
            return Err(invalid("an Element is made of images"));
        }
    }
    let mut supplied = ParamValues::new();
    supplied.insert(NAME.into(), Value::Text(p.name));
    if let Some(category) = p.category {
        supplied.insert(CATEGORY.into(), Value::Text(category));
    }
    if let Some(guidelines) = p.guidelines {
        supplied.insert(GUIDELINES.into(), Value::Text(guidelines));
    }
    let mut node = c.kinds.get(ID)?.instantiate_instance(p.id, BTreeMap::new(), &supplied)?;
    node.pos = p.pos;
    let mut changes = vec![Change::AddNode { canvas: p.canvas, node }];
    for (order, (from, edge)) in p.from.into_iter().zip(p.edges).enumerate() {
        let order = u32::try_from(order).map_err(|_| invalid("too many images"))?;
        changes.push(Change::Connect { canvas: p.canvas, edge: Edge { id: edge, from, to: Endpoint { node: p.id, port: IMAGES.into() }, order } });
    }
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id}), gesture: None })
}

/// What the reference sheet shows for each category. NodeCraft's own wording.
fn subject(category: &str) -> &'static str {
    match category {
        "person" => "the person in the reference images: face close-up, front, three-quarter and profile views, and a full-length view",
        "character" => "the character in the reference images: front, side and back turnaround, plus a few expressions",
        "product" => "the product in the reference images from front, back, side and top, with a close-up of its details",
        "object" => "the object in the reference images from several angles, with a close-up of its details",
        "material" => "the material in the reference images as flat swatches and a close-up of its texture",
        "setting" => "the place in the reference images from several viewpoints, wide and close",
        "style" => "the visual style of the reference images, applied to a few varied subjects",
        "moodboard" => "the colours, textures and mood of the reference images as one moodboard",
        _ => "the subject of the reference images from several angles",
    }
}

/// The prompt a sheet card starts with, from the Element's category and guidelines.
pub fn sheet_prompt(node: &Node) -> String {
    let mut prompt =
        format!("A reference sheet of {}. Plain neutral background, even lighting, every view consistent with the others.", subject(&text(node, CATEGORY)));
    let guidelines = text(node, GUIDELINES);
    // A `#` would make the sheet card mention Elements, possibly its own: drop it.
    let guidelines = guidelines.replace('#', "");
    if !guidelines.trim().is_empty() {
        prompt.push_str(" Keep: ");
        prompt.push_str(guidelines.trim());
    }
    prompt
}

/// The image role of `model` that takes `images` references, when a mode of it needs nothing
/// else wired.
fn role_for(model: &GenerationModel, images: usize) -> Option<&str> {
    model.modes.iter().find_map(|mode| {
        let others_optional = mode.inputs.iter().filter(|r| r.role != PROMPT && r.modality != Modality::Image).all(|r| r.min == 0);
        let fits =
            |r: &&nodecraft_nodes::generation::GenerationRole| r.modality == Modality::Image && usize::from(r.min) <= images && usize::from(r.max) >= images;
        let role = mode.inputs.iter().find(fits)?;
        let rest = mode.inputs.iter().filter(|r| r.modality == Modality::Image && r.role != role.role).all(|r| r.min == 0);
        (others_optional && rest).then_some(role.role.as_str())
    })
}

/// The models that can make a sheet from `images` reference images, in catalog order, with the
/// input role each takes them on.
pub fn sheet_models(catalog: &GenerationCatalog, images: usize) -> Vec<(String, String)> {
    catalog.models(KIND).filter_map(|m| Some((m.id.clone(), role_for(m, images)?.to_owned()))).collect()
}

/// The Element's reference image wires, in order.
pub(crate) fn image_wires(canvas: &Canvas, id: NodeId) -> Vec<&Edge> {
    let mut wires: Vec<&Edge> = canvas.edges().values().filter(|e| e.to.node == id && e.to.port == IMAGES).collect();
    wires.sort_by_key(|e| (e.order, e.id));
    wires
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SheetParams {
    canvas: CanvasId,
    /// The Element.
    element: NodeId,
    /// The new card's id, and its connections': one per reference image, then card → sheet.
    id: NodeId,
    edges: Vec<EdgeId>,
    pos: Position,
    /// The model; the first that takes this many images when absent.
    #[serde(default)]
    model: Option<String>,
}

fn sheet(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SheetParams = params(value)?;
    check_pos(p.pos)?;
    let canvas = canvas(c, p.canvas)?;
    let node = canvas.nodes().get(&p.element).ok_or_else(|| not_found("document entity"))?;
    if node.kind != ID {
        return Err(invalid("make a reference sheet from an Element"));
    }
    if canvas.edges().values().any(|e| e.to.node == p.element && e.to.port == SHEET) {
        return Err(Error::new(ErrorKind::Conflict, "this Element already has a reference sheet: run its card again to make another"));
    }
    let wires = image_wires(canvas, p.element);
    if wires.is_empty() {
        return Err(invalid("connect reference images to the Element first"));
    }
    if p.edges.len() != wires.len().saturating_add(1) {
        return Err(invalid("give one new edge id per reference image, then one for the sheet"));
    }
    let models = c.resources.get::<GenerationCatalog>().map(|catalog| sheet_models(catalog, wires.len())).unwrap_or_default();
    let (model, role) = match p.model {
        Some(model) => {
            models.into_iter().find(|(id, _)| *id == model).ok_or_else(|| Error::new(ErrorKind::Unsupported, "that model can't take these reference images"))?
        }
        None => models.into_iter().next().ok_or_else(|| Error::new(ErrorKind::Unsupported, "no model that takes this many reference images is available"))?,
    };
    let mut supplied = ParamValues::new();
    supplied.insert(MODEL.into(), Value::Text(model));
    supplied.insert(PROMPT.into(), Value::Text(sheet_prompt(node)));
    let mut card = c.kinds.get(KIND)?.instantiate_instance(p.id, BTreeMap::new(), &supplied)?;
    card.pos = p.pos;
    let mut changes = vec![Change::AddNode { canvas: p.canvas, node: card }];
    let mut ids = p.edges.into_iter();
    for (order, wire) in wires.iter().enumerate() {
        let edge = ids.next().ok_or_else(|| invalid("too few edge ids"))?;
        let order = u32::try_from(order).map_err(|_| invalid("too many images"))?;
        changes.push(Change::Connect {
            canvas: p.canvas,
            edge: Edge { id: edge, from: wire.from.clone(), to: Endpoint { node: p.id, port: role.clone() }, order },
        });
    }
    let edge = ids.next().ok_or_else(|| invalid("too few edge ids"))?;
    let out = Endpoint { node: p.id, port: "out".into() };
    changes.push(Change::Connect { canvas: p.canvas, edge: Edge { id: edge, from: out, to: Endpoint { node: p.element, port: SHEET.into() }, order: 0 } });
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id}), gesture: None })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    canvas: CanvasId,
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ListParams = params(value)?;
    let canvas = canvas(c, p.canvas)?;
    let mut mentioned: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    let index = mentions::Index::new(canvas, c.kinds);
    for node in canvas.nodes().values() {
        for mention in index.of(node) {
            if let Resolved::Element(id) | Resolved::Wired(id) = mention.resolved {
                mentioned.entry(id).or_default().push(node.id);
            }
        }
    }
    let elements: Vec<Json> = canvas
        .nodes()
        .values()
        .filter(|n| n.kind == ID)
        .map(|n| {
            let name = text(n, NAME);
            let sheet = canvas.edges().values().find(|e| e.to.node == n.id && e.to.port == SHEET).map(|e| e.from.node);
            json!({
                "id": n.id,
                "name": name,
                "valid_name": element::valid_name(&name),
                "shared_name": element::valid_name(&name) && taken(canvas, &name, Some(n.id)),
                "category": text(n, CATEGORY),
                "images": image_wires(canvas, n.id).len(),
                "sheet": sheet,
                "computed": n.active.is_some(),
                "mentioned_by": mentioned.get(&n.id).cloned().unwrap_or_default(),
            })
        })
        .collect();
    Ok(Effect::None { result: json!({"elements": elements}) })
}

fn always(_: &Context<'_>, _: &Json) -> Result<(), Error> {
    Ok(())
}

fn enabled_create(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    create(c, value).map(|_| ())
}

fn enabled_sheet(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    sheet(c, value).map(|_| ())
}

pub(crate) fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

pub(crate) fn pos() -> Json {
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}})
}

fn create_schema() -> Json {
    let endpoint = json!({"type": "object", "required": ["node", "port"], "additionalProperties": false, "properties": {"node": uuid(), "port": {"type": "string", "maxLength": 256}}});
    let categories: Vec<&str> = CATEGORIES.iter().map(|(value, _)| *value).collect();
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "name", "from", "edges", "pos"],
        "properties": {"canvas": uuid(), "id": uuid(), "pos": pos(),
            "name": {"type": "string", "minLength": 1, "maxLength": element::MAX_NAME, "pattern": "^[A-Za-z0-9_-]+$"},
            "category": {"type": "string", "enum": categories},
            "guidelines": {"type": "string", "maxLength": 4096},
            "from": {"type": "array", "maxItems": MAX_IMAGES, "items": endpoint},
            "edges": {"type": "array", "maxItems": MAX_IMAGES, "items": uuid()}}})
}

fn sheet_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "element", "id", "edges", "pos"],
        "properties": {"canvas": uuid(), "element": uuid(), "id": uuid(), "pos": pos(),
            "edges": {"type": "array", "minItems": 2, "maxItems": MAX_IMAGES + 1, "items": uuid()},
            "model": {"type": "string", "maxLength": 256}}})
}

fn list_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas"], "properties": {"canvas": uuid()}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "element.create",
            label: "Make Element",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: create_schema,
            params_doc: "{canvas, id: new NodeId, name, category?, guidelines?, from: [{node, port}], edges: [new EdgeId], pos: {x, y}} -> {id}; adds an Element wired from up to 8 image outputs, in order, as one undo step. The name (letters, digits, - and _) must be free on the canvas; prompts mention it as #name, which feeds the Element's references into that generation card's reference image list (not while its prompt is wired). Categories: general, person, character, product, object, material, setting, style, moodboard.",
            enabled: enabled_create,
            run: create,
        },
        CommandSpec {
            id: "element.sheet",
            label: "Make reference sheet",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: sheet_schema,
            params_doc: "{canvas, element: Element NodeId, id: new NodeId, edges: [one new EdgeId per reference image, then one more], pos: {x, y}, model?} -> {id}; adds a generation card taking the Element's reference images, prompted from its category and guidelines, and wires its result into the Element's sheet input, as one undo step. The card is wired from the images connected now; wire later images into the card too. Once the card has a result the Element gives the sheet instead of its images (until then, its images); the card's history and active result are its reference history and default. It does not run: run the card as usual, which shows its cost first.",
            enabled: enabled_sheet,
            run: sheet,
        },
        CommandSpec {
            id: "element.list",
            label: "List Elements",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas} -> {elements: [{id, name, valid_name, shared_name, category, images, sheet, computed, mentioned_by}]}; the canvas's Elements: how many reference images are wired, the sheet card if any, whether its references have been computed, and the nodes whose prompt mentions it. A shared name blocks the cards mentioning it.",
            enabled: always,
            run: list,
        },
    ]
}

#[cfg(test)]
mod tests;
