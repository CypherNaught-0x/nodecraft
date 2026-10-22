//! Deck commands (#122). A Deck card keeps its slides in its config (see `nodecraft-nodes::deck`
//! and `nodecraft-actions::deck`); `deck.list` shows them with what each source layer's node
//! shows now, and every other command changes them as one undo step. Every edit takes an optional
//! `expected_revision` and is refused with a conflict when the document has changed since, so an
//! agent never overwrites a change it hasn't seen. A deck from a newer NodeCraft is listed but
//! never changed. Nothing here runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_actions::deck::{self, Deck, Item, Shown, Slide};
use nodecraft_actions::layers::{self, Content, Frame, Layer};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Position};
use nodecraft_nodes::deck::{CONFIG, ID};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, FieldId, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;

/// Longest text a source layer shows, in bytes.
const MAX_SOURCE_TEXT: usize = layers::MAX_TEXT_BYTES / 2;
/// The canvas extent positions may use (as `node.create`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// Most images of one node a source layer looks through.
const MAX_NTH: u32 = 4096;

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

fn unusable(e: nodecraft_actions::image::ImageError) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the deck isn't valid: {e}"))
}

fn canvas<'a>(c: &'a Context<'_>, canvas: CanvasId) -> Result<&'a Canvas, Error> {
    c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))
}

fn card<'a>(c: &'a Context<'_>, at: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    let node = canvas(c, at)?.nodes().get(&id).ok_or_else(|| not_found("document entity"))?;
    if node.kind == ID { Ok(node) } else { Err(invalid("only a Deck card has slides")) }
}

/// The deck a Deck card stores.
pub fn stored(node: &Node) -> Result<Deck, Error> {
    Deck::parse(nodecraft_nodes::deck::stored(node)?).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the deck's slides can't be read: {e}")))
}

/// The frame a Deck card's slides are drawn into, from its parameters.
pub fn frame(c: &Context<'_>, node: &Node) -> Result<Frame, Error> {
    let values = c.kinds.get(ID)?.validate_instance(node, true)?.into_visible_values();
    let edge = |name: &str| match values.get(name) {
        Some(Value::Unsigned(v)) => u32::try_from(v.0).ok().filter(|v| (layers::MIN_FRAME..=layers::MAX_FRAME).contains(v)),
        _ => None,
    };
    let background = match values.get("background") {
        Some(Value::Text(text)) => layers::parse_color(text).ok(),
        _ => None,
    };
    match (edge("width"), edge("height"), background) {
        (Some(width), Some(height), Some(background)) => Ok(Frame { width, height, background }),
        _ => Err(invalid("the deck's size or background isn't valid")),
    }
}

/// Checks the deck may change, and that the document is the one the caller saw.
fn writable(c: &Context<'_>, deck: &Deck, expected: Option<u64>) -> Result<(), Error> {
    c.editable()?;
    if expected.is_some_and(|expected| expected != c.revision) {
        return Err(Error::new(ErrorKind::Conflict, "the document changed; list the deck again before changing it"));
    }
    if deck.writable() {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::Unsupported, "this deck was made by a newer NodeCraft; it is kept as it is and can't be changed here"))
    }
}

fn store(at: CanvasId, node: &Node, deck: &Deck, result: Json) -> Result<Effect, Error> {
    let json = deck.encode().map_err(unusable)?;
    let mut changed = node.clone();
    changed.config.insert(CONFIG.into(), Value::Text(json));
    if changed == *node {
        return Ok(Effect::None { result: with_changed(result, false) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas: at, node: changed }]), result: with_changed(result, true), gesture: None })
}

fn with_changed(mut result: Json, changed: bool) -> Json {
    if let Json::Object(map) = &mut result {
        map.insert("changed".into(), Json::Bool(changed));
    }
    result
}

/// The images a node shows, in order: a Source image's asset, else every image of its active
/// result, inside lists too. Stops once `nth` is reached.
fn image(node: &Node, nth: u32) -> Option<AssetRef> {
    if node.kind == "source.image" {
        return (nth == 0).then(|| crate::split::image_of(node).cloned()).flatten();
    }
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    let mut images = result.outputs.iter().flat_map(flatten).filter_map(crate::split::image);
    images.nth(usize::try_from(nth).ok()?).cloned()
}

fn flatten(value: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match value {
        Value::List(items) => Box::new(items.iter().flat_map(|item| match item {
            Value::List(inner) => Box::new(inner.iter()) as Box<dyn Iterator<Item = &Value>>,
            single => Box::new(std::iter::once(single)),
        })),
        single => Box::new(std::iter::once(single)),
    }
}

/// The text a node shows: a Text source's or a Note's text, else its active result's first text.
fn text(node: &Node) -> Option<&str> {
    if matches!(node.kind.as_str(), "source.text" | "structure.note") {
        return match node.params.get("text") {
            Some(Value::Text(text)) => Some(text),
            _ => None,
        };
    }
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    result.outputs.iter().flat_map(flatten).find_map(|v| if let Value::Text(text) = v { Some(text.as_str()) } else { None })
}

fn clip(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// What the `nth` image of node `origin` on `canvas` is now: its image, its text (for the
/// first), or missing.
pub fn shown(canvas: &Canvas, origin: &str, nth: u32) -> Shown<AssetRef> {
    let Some(node) = origin.parse::<NodeId>().ok().and_then(|id| canvas.nodes().get(&id)) else { return Shown::Missing };
    if nth > MAX_NTH {
        return Shown::Missing;
    }
    if let Some(asset) = image(node, nth) {
        return Shown::Image(asset);
    }
    match text(node).filter(|t| nth == 0 && !t.trim().is_empty()) {
        Some(text) => Shown::Text(clip(text, MAX_SOURCE_TEXT).into()),
        None => Shown::Missing,
    }
}

fn source_json(layer: &str, origin: &str, nth: u32, shown: Shown<AssetRef>) -> Json {
    match shown {
        Shown::Image(image) => json!({"layer": layer, "origin": origin, "nth": nth, "shows": "image", "image": image}),
        Shown::Text(text) => json!({"layer": layer, "origin": origin, "nth": nth, "shows": "text", "text": text}),
        Shown::Missing => json!({"layer": layer, "origin": origin, "nth": nth, "shows": "missing"}),
    }
}

fn slide_json(on: &Canvas, slide: &Slide) -> Json {
    let mut sources = Vec::new();
    let mut unsupported = Vec::new();
    for (index, item) in slide.layers.iter().enumerate() {
        match item {
            Item::Layer(Layer { id, content: Content::Source { origin, nth, .. }, .. }) => sources.push(source_json(id, origin, *nth, shown(on, origin, *nth))),
            Item::Layer(_) => {}
            Item::Unsupported(_) => unsupported.push(index),
        }
    }
    let mut out = serde_json::to_value(slide).unwrap_or(Json::Null);
    if let Json::Object(map) = &mut out {
        map.insert("sources".into(), Json::Array(sources));
        map.insert("unsupported".into(), json!(unsupported));
    }
    out
}

fn always(_: &Context<'_>, _: &Json) -> Result<(), Error> {
    Ok(())
}

fn revision_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    canvas: CanvasId,
    id: NodeId,
    slide: Option<String>,
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ListParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let on = canvas(c, p.canvas)?;
    let deck = stored(node)?;
    let frame = frame(c, node)?;
    let slides: Vec<Json> = match &p.slide {
        Some(id) => vec![slide_json(on, deck.slides.get(deck.index_of(id).ok_or_else(|| not_found("slide"))?).ok_or_else(|| not_found("slide"))?)],
        None => deck.slides.iter().map(|s| slide_json(on, s)).collect(),
    };
    let [r, g, b, a] = frame.background;
    Ok(Effect::None {
        result: json!({
            "revision": c.revision,
            "version": deck.version,
            "writable": deck.writable(),
            "width": frame.width,
            "height": frame.height,
            "background": format!("#{r:02x}{g:02x}{b:02x}{a:02x}"),
            "count": deck.slides.len(),
            "slides": slides,
        }),
    })
}

/// A slide as a caller gives it: its layers must all be readable, except unsupported layers the
/// deck already holds (kept as they were), so listing and setting a slide never loses them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SlideInput {
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    layers: Vec<Json>,
}

fn items(deck: &Deck, layers: Vec<Json>) -> Result<Vec<Item>, Error> {
    if layers.len() > layers::MAX_LAYERS {
        return Err(invalid("a slide holds at most 64 layers"));
    }
    let held = || deck.slides.iter().flat_map(|s| &s.layers).filter_map(|i| if let Item::Unsupported(raw) = i { Some(raw) } else { None });
    layers
        .into_iter()
        .map(|raw| {
            if held().any(|kept| *kept == raw) {
                return Ok(Item::Unsupported(raw));
            }
            let layer: Layer = serde_json::from_value(raw).map_err(|_| invalid("a layer isn't valid: a field is unknown or has the wrong type or value"))?;
            deck::usable(&layer).map_err(unusable)?;
            Ok(Item::Layer(layer))
        })
        .collect()
}

fn slide_of(deck: &Deck, input: SlideInput, fallback: String) -> Result<Slide, Error> {
    let mut slide = Slide::new(input.id.unwrap_or(fallback), input.name);
    slide.layers = items(deck, input.layers)?;
    slide.check().map_err(unusable)?;
    Ok(slide)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    canvas: CanvasId,
    id: NodeId,
    slides: Vec<SlideInput>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SetParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let mut deck = stored(node)?;
    writable(c, &deck, p.expected_revision)?;
    if p.slides.len() > deck::MAX_SLIDES {
        return Err(invalid("a deck holds at most 256 slides"));
    }
    let mut slides = Vec::with_capacity(p.slides.len());
    for (n, input) in p.slides.into_iter().enumerate() {
        let kept = input.id.as_ref().and_then(|id| deck.index_of(id)).and_then(|at| deck.slides.get(at)).map(|s| s.extra.clone());
        let mut slide = slide_of(&deck, input, format!("slide-{}", n.saturating_add(1)))?;
        slide.extra = kept.unwrap_or_default();
        slides.push(slide);
    }
    deck.slides = slides;
    store(p.canvas, node, &deck, json!({}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    canvas: CanvasId,
    id: NodeId,
    slide: SlideInput,
    /// Where it goes, 0 first; past the end (or none) is the end.
    index: Option<u32>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn slide_add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let mut deck = stored(node)?;
    writable(c, &deck, p.expected_revision)?;
    if deck.slides.len() >= deck::MAX_SLIDES {
        return Err(invalid("a deck holds at most 256 slides"));
    }
    let slide = slide_of(&deck, p.slide, deck.fresh_id())?;
    if deck.index_of(&slide.id).is_some() {
        return Err(Error::new(ErrorKind::Conflict, "a slide with that id already exists"));
    }
    let id = slide.id.clone();
    let at = p.index.and_then(|i| usize::try_from(i).ok()).unwrap_or(usize::MAX).min(deck.slides.len());
    deck.slides.insert(at, slide);
    store(p.canvas, node, &deck, json!({"slide": id}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    canvas: CanvasId,
    id: NodeId,
    slide: String,
    name: Option<String>,
    layers: Option<Vec<Json>>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn slide_update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpdateParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let mut deck = stored(node)?;
    writable(c, &deck, p.expected_revision)?;
    let at = deck.index_of(&p.slide).ok_or_else(|| not_found("slide"))?;
    let layers = p.layers.map(|layers| items(&deck, layers)).transpose()?;
    let slide = deck.slides.get_mut(at).ok_or_else(|| not_found("slide"))?;
    if let Some(name) = p.name {
        slide.name = name;
    }
    if let Some(layers) = layers {
        slide.layers = layers;
    }
    store(p.canvas, node, &deck, json!({}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SlideParams {
    canvas: CanvasId,
    id: NodeId,
    slide: String,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn slide_remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SlideParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let mut deck = stored(node)?;
    writable(c, &deck, p.expected_revision)?;
    let at = deck.index_of(&p.slide).ok_or_else(|| not_found("slide"))?;
    deck.slides.remove(at);
    store(p.canvas, node, &deck, json!({}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveParams {
    canvas: CanvasId,
    id: NodeId,
    slide: String,
    /// The new place, 0 first; past the end is the end.
    index: u32,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn slide_move(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: MoveParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let mut deck = stored(node)?;
    writable(c, &deck, p.expected_revision)?;
    let at = deck.index_of(&p.slide).ok_or_else(|| not_found("slide"))?;
    let slide = deck.slides.remove(at);
    let to = usize::try_from(p.index).unwrap_or(usize::MAX).min(deck.slides.len());
    deck.slides.insert(to, slide);
    store(p.canvas, node, &deck, json!({}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerUpdateParams {
    canvas: CanvasId,
    id: NodeId,
    slide: String,
    layer: String,
    set: Map<String, Json>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}

fn layer_update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: LayerUpdateParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let mut deck = stored(node)?;
    writable(c, &deck, p.expected_revision)?;
    let at = deck.index_of(&p.slide).ok_or_else(|| not_found("slide"))?;
    let slide = deck.slides.get_mut(at).ok_or_else(|| not_found("slide"))?;
    let index = slide.layer_index(&p.layer).ok_or_else(|| not_found("layer"))?;
    let Some(Item::Layer(current)) = slide.layers.get(index) else {
        return Err(Error::new(ErrorKind::Unsupported, "this layer was made by a newer NodeCraft; it is kept as it is and can't be changed here"));
    };
    if p.set.contains_key("id") {
        return Err(invalid("a layer's id doesn't change"));
    }
    let mut merged = serde_json::to_value(current).map_err(|_| invalid("the layer can't be read"))?;
    let Json::Object(fields) = &mut merged else { return Err(invalid("the layer can't be read")) };
    for (key, value) in p.set {
        match (key.as_str(), fields.get_mut("content"), value) {
            ("content", Some(Json::Object(content)), Json::Object(changes)) => {
                if changes.get("kind").is_some_and(|k| Some(k) != content.get("kind")) {
                    return Err(invalid("a layer's kind doesn't change: set the slide's layers instead"));
                }
                content.extend(changes);
            }
            (_, _, value) => {
                fields.insert(key, value);
            }
        }
    }
    let changed: Layer = serde_json::from_value(merged).map_err(|_| invalid("the change isn't valid: a field is unknown or has the wrong type or value"))?;
    deck::usable(&changed).map_err(unusable)?;
    if let Some(slot) = slide.layers.get_mut(index) {
        *slot = Item::Layer(changed);
    }
    store(p.canvas, node, &deck, json!({}))
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPosition {
    x: f32,
    y: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    canvas: CanvasId,
    /// The new card's id, chosen by the caller.
    id: NodeId,
    pos: StrictPosition,
    #[serde(default)]
    title: String,
    #[serde(default)]
    nodes: Vec<NodeId>,
    #[serde(default)]
    fields: Vec<FieldId>,
}

/// What becomes a slide: a node or a field, with where it sits (for the slide order).
enum Subject<'a> {
    Node(&'a Node),
    Field(FieldId, &'a nodecraft_graph::Field),
}

fn create(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CreateParams = params(value)?;
    c.editable()?;
    let pos = Position { x: p.pos.x, y: p.pos.y };
    if !(pos.x.is_finite() && pos.y.is_finite() && pos.x.abs() <= MAX_COORDINATE && pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    if p.nodes.len().saturating_add(p.fields.len()) > deck::MAX_SLIDES {
        return Err(invalid("a deck holds at most 256 slides"));
    }
    let on = canvas(c, p.canvas)?;
    let fields: Vec<FieldId> = {
        let mut seen = Vec::new();
        for id in &p.fields {
            on.fields().get(id).ok_or_else(|| not_found("field"))?;
            if !seen.contains(id) {
                seen.push(*id);
            }
        }
        seen
    };
    let mut subjects = Vec::new();
    for id in &fields {
        if let Some(field) = on.fields().get(id) {
            subjects.push(((field.rect.pos.y, field.rect.pos.x), Subject::Field(*id, field)));
        }
    }
    let mut taken = std::collections::BTreeSet::new();
    for id in &p.nodes {
        let node = on.nodes().get(id).ok_or_else(|| not_found("node"))?;
        // A node shows on its field's slide, and a deck isn't a slide.
        if node.kind == ID || node.field.is_some_and(|f| fields.contains(&f)) || !taken.insert(*id) {
            continue;
        }
        subjects.push(((node.pos.y, node.pos.x), Subject::Node(node)));
    }
    // Slides follow the canvas: top to bottom, then left to right.
    subjects.sort_by(|(a, _), (b, _)| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));

    let mut card = c.kinds.get(ID)?.instantiate_instance(p.id, BTreeMap::new(), &ParamValues::new())?;
    card.pos = pos;
    card.title = clip(p.title.trim(), layers::MAX_NAME_BYTES).into();
    let frame = frame(c, &card)?;
    let mut deck = Deck::default();
    for (_, subject) in subjects {
        let id = deck.fresh_id();
        let slide = match subject {
            Subject::Node(node) => deck::node_slide(id, &name_of(node, c), &node.id.to_string(), frame),
            Subject::Field(field_id, field) => {
                let mut members: Vec<(String, String, (f64, f64))> = on
                    .nodes()
                    .values()
                    .filter(|n| n.field == Some(field_id) && n.kind != ID)
                    .map(|n| (n.id.to_string(), name_of(n, c), (f64::from(n.pos.x), f64::from(n.pos.y))))
                    .collect();
                members.sort_by(|a, b| a.2.1.total_cmp(&b.2.1).then(a.2.0.total_cmp(&b.2.0)));
                deck::field_slide(id, &field.title, &members, frame)
            }
        };
        deck.slides.push(slide);
    }
    let json = deck.encode().map_err(unusable)?;
    card.config.insert(CONFIG.into(), Value::Text(json));
    let count = deck.slides.len();
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node: card }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "slides": count}), gesture: None })
}

/// What a slide made from `node` is called: its title, else its kind's label.
fn name_of(node: &Node, c: &Context<'_>) -> String {
    let title = node.title.trim();
    if !title.is_empty() {
        return clip(title, layers::MAX_NAME_BYTES).into();
    }
    c.kinds.get(&node.kind).map(|k| k.descriptor().label.clone()).unwrap_or_else(|_| "Node".into())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn short_id() -> Json {
    json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$"})
}

fn revision_schema() -> Json {
    json!({"type": "integer", "minimum": 0, "maximum": u64::MAX, "description": "The revision deck.list returned; refused with a conflict when the document has changed since."})
}

fn object(required: &[&str], properties: Json, edit: bool) -> Json {
    let mut properties = match properties {
        Json::Object(map) => map,
        _ => Map::new(),
    };
    properties.insert("canvas".into(), uuid());
    properties.insert("id".into(), uuid());
    if edit {
        properties.insert("expected_revision".into(), revision_schema());
    }
    let mut required: Vec<&str> = required.to_vec();
    required.splice(0..0, ["canvas", "id"]);
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn layers_schema() -> Json {
    json!({"type": "array", "maxItems": layers::MAX_LAYERS, "items": {"type": "object", "description": "A layer as layers.add takes it (content kind text, shape or source; a source's origin is a node id on this canvas), or an unsupported layer exactly as deck.list returned it."}})
}

fn slide_schema() -> Json {
    json!({"type": "object", "additionalProperties": false, "properties": {"id": short_id(), "name": {"type": "string", "maxLength": layers::MAX_NAME_BYTES}, "layers": layers_schema()}})
}

fn list_schema() -> Json {
    object(&[], json!({"slide": short_id()}), false)
}

fn set_schema() -> Json {
    object(&["slides"], json!({"slides": {"type": "array", "maxItems": deck::MAX_SLIDES, "items": slide_schema()}}), true)
}

fn add_schema() -> Json {
    object(&["slide"], json!({"slide": slide_schema(), "index": {"type": "integer", "minimum": 0, "maximum": deck::MAX_SLIDES}}), true)
}

fn update_schema() -> Json {
    object(&["slide"], json!({"slide": short_id(), "name": {"type": "string", "maxLength": layers::MAX_NAME_BYTES}, "layers": layers_schema()}), true)
}

fn slide_only_schema() -> Json {
    object(&["slide"], json!({"slide": short_id()}), true)
}

fn move_schema() -> Json {
    object(&["slide", "index"], json!({"slide": short_id(), "index": {"type": "integer", "minimum": 0, "maximum": deck::MAX_SLIDES}}), true)
}

fn layer_update_schema() -> Json {
    object(
        &["slide", "layer", "set"],
        json!({"slide": short_id(), "layer": short_id(), "set": {"type": "object", "description": "Layer fields to change, as layers.update takes them; content fields merge and its kind stays."}}),
        true,
    )
}

fn create_schema() -> Json {
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    object(
        &["pos"],
        json!({
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "title": {"type": "string", "maxLength": layers::MAX_NAME_BYTES},
            "nodes": {"type": "array", "maxItems": deck::MAX_SLIDES, "items": uuid()},
            "fields": {"type": "array", "maxItems": deck::MAX_SLIDES, "items": uuid()},
        }),
        false,
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "deck.create",
            label: "Make deck",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: create_schema,
            params_doc: "{canvas, id: new NodeId, pos: {x, y}, title?, nodes?: [NodeId], fields?: [FieldId]} -> {id, slides}; adds a Deck card with one slide per node and one per field (its nodes laid out as on the canvas, under the field's title), ordered top to bottom, then left to right. A node inside a listed field is on that field's slide. Each slide's source layers show what their node shows. One undo step.",
            enabled: always,
            run: create,
        },
        CommandSpec {
            id: "deck.list",
            label: "List slides",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas, id: Deck NodeId, slide?: slide id} -> {revision, version, writable, width, height, background, count, slides}; every slide (or the one named), first to last, as {id, name, layers, sources, unsupported}. layers are bottom first, in layers.add's format; sources says what each source layer's node shows now ({layer, origin, nth, shows: \"image\" with image, \"text\" with text, or \"missing\"}); unsupported lists the indices of layers this version keeps but can't read or draw. Pass revision as expected_revision to the edits. A deck from a newer NodeCraft lists as writable false, with no slides.",
            enabled: always,
            run: list,
        },
        CommandSpec {
            id: "deck.set",
            label: "Set slides",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{canvas, id, slides: [{id?, name?, layers?}], expected_revision?} -> {changed}; replaces a deck's slides, first to last, as one undo step. A slide without an id gets slide-n. Layers are as layers.add takes them; an unsupported layer passes only exactly as deck.list returned it.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "deck.slide_add",
            label: "Add slide",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{canvas, id, slide: {id?, name?, layers?}, index?, expected_revision?} -> {changed, slide}; adds a slide at index (0 first; past the end or none is the end), as one undo step. Its id must be new; without one it gets slide-n.",
            enabled: always,
            run: slide_add,
        },
        CommandSpec {
            id: "deck.slide_update",
            label: "Change slide",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{canvas, id, slide: slide id, name?, layers?, expected_revision?} -> {changed}; renames a slide and/or replaces its layers (bottom first, as deck.set takes them), as one undo step.",
            enabled: always,
            run: slide_update,
        },
        CommandSpec {
            id: "deck.slide_remove",
            label: "Delete slide",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: slide_only_schema,
            params_doc: "{canvas, id, slide: slide id, expected_revision?} -> {changed}; deletes a slide, as one undo step.",
            enabled: always,
            run: slide_remove,
        },
        CommandSpec {
            id: "deck.slide_move",
            label: "Move slide",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: move_schema,
            params_doc: "{canvas, id, slide: slide id, index, expected_revision?} -> {changed}; moves a slide to index (0 first; past the end is the end), as one undo step.",
            enabled: always,
            run: slide_move,
        },
        CommandSpec {
            id: "deck.layer_update",
            label: "Change slide layer",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: layer_update_schema,
            params_doc: "{canvas, id, slide: slide id, layer: layer id, set: {fields}, expected_revision?} -> {changed}; changes some of a slide layer's fields (as layers.update: name, visible, locked, opacity, x, y, width, height, rotation, flip_x, flip_y, or content fields other than its kind), as one undo step. An unsupported layer can't be changed.",
            enabled: always,
            run: layer_update,
        },
    ]
}

#[cfg(test)]
mod tests;
