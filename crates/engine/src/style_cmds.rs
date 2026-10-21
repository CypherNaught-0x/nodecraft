//! Trained styles (#167) as commands: `style.list` lists a canvas's Train style cards, with the
//! family each trained style is for and, given a target card, whether it can take it; `style.use`
//! wires a trained style into a card's style input as one undo step, replacing the style wired
//! there before; `style.card` adds a card on a model that takes the style, wired from it. A card
//! can only switch to a model its wiring fits, so a model that needs a style is reached through
//! `style.card`. None of them runs anything: training and styled generation run (and are costed)
//! like any other card.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Port, Position};
use nodecraft_nodes::generation::{GenerationModel, KIND, MODEL, PROMPT, STYLE_KIND, is_generation};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, ParamValues, STYLE_MIME, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// How many compatible models a refusal names.
const NAMED_MODELS: usize = 5;
/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

fn canvas<'a>(c: &'a Context<'_>, id: CanvasId) -> Result<&'a Canvas, Error> {
    c.document.canvases().get(&id).ok_or_else(|| not_found("canvas"))
}

fn model(node: &Node) -> Option<&str> {
    match node.params.get(MODEL) {
        Some(Value::Text(id)) if !id.is_empty() => Some(id),
        _ => None,
    }
}

fn port(ports: &[Port], modality: Modality) -> Option<&Port> {
    ports.iter().find(|port| port.port_type.modality == modality)
}

/// The card's active trained style: its one style asset, when it has a complete result.
fn active_style(node: &Node) -> Option<&AssetRef> {
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    if result.incomplete.is_some() {
        return None;
    }
    let value = match result.outputs.as_slice() {
        [Value::List(items)] => match items.as_slice() {
            [value] => value,
            _ => return None,
        },
        [value] => value,
        _ => return None,
    };
    match value {
        Value::Media(asset) if asset.mime.essence() == STYLE_MIME => Some(asset),
        _ => None,
    }
}

/// The family a style was trained for, when it names a valid one; a hand-edited project can hold
/// anything there.
fn family(style: &AssetRef) -> Option<&str> {
    style.mime.param("family").filter(|f| nodecraft_types::style_family(f))
}

/// A Train style card on `canvas`, with the family of its active style when it has one.
fn style_card(canvas: &Canvas, id: NodeId) -> Result<(&Node, Option<&AssetRef>), Error> {
    let node = canvas.nodes().get(&id).ok_or_else(|| not_found("document entity"))?;
    if node.kind != STYLE_KIND {
        return Err(invalid("choose a Train style card as the style"));
    }
    Ok((node, active_style(node)))
}

/// What `target` would take a style of `family` on: its style input, or why it can't.
fn fits<'a>(catalog: Option<&GenerationCatalog>, target: &'a Node, family: &str) -> Result<&'a Port, Error> {
    if target.kind == STYLE_KIND || !is_generation(&target.kind) {
        return Err(invalid("a trained style goes into a generation card whose model takes styles"));
    }
    let id = model(target).unwrap_or("this card");
    let wanted = model(target).and_then(|id| catalog?.model(&target.kind, id)).and_then(|m| m.style_family.as_deref());
    let input = port(&target.inputs, Modality::Style);
    match (input, wanted) {
        (Some(input), Some(wanted)) if wanted == family => Ok(input),
        (Some(_), Some(wanted)) => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("this style was trained for {family} models; {id} takes {wanted} styles.{}", alternatives(catalog, &target.kind, family)),
        )),
        _ => Err(Error::new(ErrorKind::Unsupported, format!("{id} doesn't take trained styles.{}", alternatives(catalog, &target.kind, family)))),
    }
}

/// The catalog models of `kind` that take `family` styles, in catalog order.
fn takers<'a>(catalog: Option<&'a GenerationCatalog>, kind: &'a str, family: &'a str) -> impl Iterator<Item = &'a GenerationModel> {
    catalog
        .into_iter()
        .flat_map(move |catalog| catalog.models(kind))
        .filter(move |m| m.style_family.as_deref() == Some(family) && m.modes.iter().any(|mode| mode.inputs.iter().any(|r| r.modality == Modality::Style)))
}

/// " Models that take it: a, b." naming the first catalog models of `kind` that take `family`
/// styles, or a hint to retrain when none do.
fn alternatives(catalog: Option<&GenerationCatalog>, kind: &str, family: &str) -> String {
    let ids: Vec<&str> = takers(catalog, kind, family).map(|m| m.id.as_str()).take(NAMED_MODELS).collect();
    if ids.is_empty() {
        " No model of this card's kind takes it; train the style again for one that does".into()
    } else {
        format!(" Models that take it: {}", ids.join(", "))
    }
}

/// Whether `node` is a Train style card with a trained style, naming its family.
pub fn trained(node: &Node) -> bool {
    node.kind == STYLE_KIND && active_style(node).and_then(family).is_some()
}

/// Whether `node` has a style input, so a style could be chosen for it.
pub fn takes_styles(node: &Node) -> bool {
    node.kind != STYLE_KIND && is_generation(&node.kind) && port(&node.inputs, Modality::Style).is_some()
}

/// One Train style card as a choice for a target card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub style: NodeId,
    /// The card's title, or its trainer when it has none.
    pub label: String,
    /// Whether it is wired into the target's style input now.
    pub wired: bool,
    /// Why the target can't take it; `None` when it can.
    pub refusal: Option<String>,
}

/// The Train style cards on `canvas` as choices for `target`, in canvas order. `style.list`
/// gives the same verdicts.
pub fn offers(canvas: &Canvas, catalog: Option<&GenerationCatalog>, target: &Node) -> Vec<Offer> {
    let input = port(&target.inputs, Modality::Style).map(|p| p.name.as_str());
    canvas
        .nodes()
        .values()
        .filter(|n| n.kind == STYLE_KIND)
        .map(|n| {
            let verdict = match active_style(n).and_then(family) {
                Some(family) => fits(catalog, target, family).map(|_| ()),
                None => Err(invalid("train this style first")),
            };
            let label = if n.title.trim().is_empty() { model(n).unwrap_or("Train style").to_owned() } else { n.title.clone() };
            let wired = input.is_some_and(|input| canvas.edges().values().any(|e| e.from.node == n.id && e.to.node == target.id && e.to.port == input));
            Offer { style: n.id, label, wired, refusal: verdict.err().map(|e| e.message) }
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    canvas: CanvasId,
    /// A card to check each style against.
    #[serde(default, rename = "for")]
    target: Option<NodeId>,
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ListParams = params(value)?;
    let canvas = canvas(c, p.canvas)?;
    let target = match p.target {
        Some(id) => Some(canvas.nodes().get(&id).ok_or_else(|| not_found("document entity"))?),
        None => None,
    };
    let styles: Vec<Json> = canvas
        .nodes()
        .values()
        .filter(|n| n.kind == STYLE_KIND)
        .map(|n| {
            let style = active_style(n);
            let family = style.and_then(family);
            // Small descriptors are resident; one that isn't just has no trigger listed.
            let trigger = style.and_then(|s| c.assets.get(&s.hash)).and_then(|bytes| nodecraft_media::style::decode(bytes).ok()).map(|s| s.trigger);
            let mut entry = json!({
                "id": n.id,
                "title": n.title,
                "trainer": model(n),
                "ready": family.is_some(),
                "family": family,
                "trigger": trigger,
            });
            if let Some(target) = target {
                let verdict = match family {
                    Some(family) => fits(c.resources.get::<GenerationCatalog>(), target, family).map(|_| ()),
                    None => Err(invalid("train this style first")),
                };
                entry["compatible"] = json!(verdict.is_ok());
                entry["reason"] = json!(verdict.err().map(|e| e.message));
            }
            entry
        })
        .collect();
    Ok(Effect::None { result: json!({"styles": styles}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UseParams {
    canvas: CanvasId,
    /// The Train style card.
    style: NodeId,
    /// The card to style.
    target: NodeId,
    /// The new connection's id.
    edge: EdgeId,
}

fn use_style(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UseParams = params(value)?;
    let canvas = canvas(c, p.canvas)?;
    let (card, style) = style_card(canvas, p.style)?;
    let family = style.and_then(family).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "this Train style card has no trained style yet; run it first"))?;
    let out = port(&card.outputs, Modality::Style).ok_or_else(|| invalid("this Train style card has no style output"))?;
    let target = canvas.nodes().get(&p.target).ok_or_else(|| not_found("document entity"))?;
    let input = fits(c.resources.get::<GenerationCatalog>(), target, family)?;
    let from = Endpoint { node: p.style, port: out.name.clone() };
    let mut replaced: Vec<EdgeId> = canvas.edges().values().filter(|e| e.to.node == p.target && e.to.port == input.name).map(|e| e.id).collect();
    replaced.sort();
    if let [only] = replaced.as_slice()
        && canvas.edges().get(only).is_some_and(|e| e.from == from)
    {
        return Err(Error::new(ErrorKind::Conflict, "this card already uses that style"));
    }
    let mut changes: Vec<Change> = replaced.iter().map(|id| Change::Disconnect { canvas: p.canvas, id: *id }).collect();
    changes.push(Change::Connect { canvas: p.canvas, edge: Edge { id: p.edge, from, to: Endpoint { node: p.target, port: input.name.clone() }, order: 0 } });
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"edge": p.edge, "replaced": replaced}), gesture: None })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CardParams {
    canvas: CanvasId,
    /// The Train style card.
    style: NodeId,
    /// The new card's id and its connection's.
    id: NodeId,
    edge: EdgeId,
    pos: Position,
    /// The model; the first image model that takes the style when absent.
    #[serde(default)]
    model: Option<String>,
}

fn card(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CardParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let canvas = canvas(c, p.canvas)?;
    let (source, style) = style_card(canvas, p.style)?;
    let style = style.ok_or_else(|| Error::new(ErrorKind::InvalidInput, "this Train style card has no trained style yet; run it first"))?;
    let family = family(style).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "this style doesn't say which models it was trained for; train it again"))?;
    let out = port(&source.outputs, Modality::Style).ok_or_else(|| invalid("this Train style card has no style output"))?;
    let catalog = c.resources.get::<GenerationCatalog>();
    let model = match p.model {
        Some(id) => takers(catalog, KIND, family)
            .find(|m| m.id == id)
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("that model doesn't take {family} styles.{}", alternatives(catalog, KIND, family))))?,
        None => takers(catalog, KIND, family)
            .next()
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("no image model here takes {family} styles; train the style again for one that does")))?,
    };
    let mut supplied = ParamValues::new();
    supplied.insert(MODEL.into(), Value::Text(model.id.clone()));
    // The trigger word calls the style up, so the prompt starts with it.
    let trigger = c.assets.get(&style.hash).and_then(|bytes| nodecraft_media::style::decode(bytes).ok()).map(|s| s.trigger).unwrap_or_default();
    if !trigger.trim().is_empty() {
        supplied.insert(PROMPT.into(), Value::Text(format!("{}, ", trigger.trim())));
    }
    let mut node = c.kinds.get(KIND)?.instantiate_instance(p.id, BTreeMap::new(), &supplied)?;
    node.pos = p.pos;
    let input = port(&node.inputs, Modality::Style).map(|port| port.name.clone()).ok_or_else(|| invalid("that model's card has no style input"))?;
    let from = Endpoint { node: p.style, port: out.name.clone() };
    let changes = Changeset::new(vec![
        Change::AddNode { canvas: p.canvas, node },
        Change::Connect { canvas: p.canvas, edge: Edge { id: p.edge, from, to: Endpoint { node: p.id, port: input }, order: 0 } },
    ]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "model": model.id}), gesture: None })
}

fn always(_: &Context<'_>, _: &Json) -> Result<(), Error> {
    Ok(())
}

fn enabled_use(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    use_style(c, value).map(|_| ())
}

fn enabled_card(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    card(c, value).map(|_| ())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn list_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas"],
        "properties": {"canvas": uuid(), "for": uuid()}})
}

fn use_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "style", "target", "edge"],
        "properties": {"canvas": uuid(), "style": uuid(), "target": uuid(), "edge": uuid()}})
}

fn card_schema() -> Json {
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    let pos = json!({"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "style", "id", "edge", "pos"],
        "properties": {"canvas": uuid(), "style": uuid(), "id": uuid(), "edge": uuid(), "pos": pos, "model": {"type": "string", "maxLength": 256}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "style.list",
            label: "List trained styles",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas, for?: NodeId} -> {styles: [{id, title, trainer, ready, family, trigger, compatible?, reason?}]}; the canvas's Train style cards. `ready` when the card's active result is a trained style; `family` is the base-model family it was trained for and `trigger` the word that calls it up in a prompt (null when unknown). With `for`, each style says whether that card's current model takes it, and if not, why, naming models that would.",
            enabled: always,
            run: list,
        },
        CommandSpec {
            id: "style.use",
            label: "Use trained style",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: use_schema,
            params_doc: "{canvas, style: Train style card NodeId, target: generation card NodeId, edge: new EdgeId} -> {edge, replaced: [EdgeId]}; wires the style card's trained style into the target's style input as one undo step, replacing any style wired there. Refused unless the style card has a trained style and the target's model takes styles of its family; the refusal names models that would. It does not run: run the target as usual, which shows its cost first.",
            enabled: enabled_use,
            run: use_style,
        },
        CommandSpec {
            id: "style.card",
            label: "New image card with this style",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: card_schema,
            params_doc: "{canvas, style: Train style card NodeId, id: new NodeId, edge: new EdgeId, pos: {x, y}, model?} -> {id, model}; adds an image card on a model that takes the style's family (the first in the catalog when `model` is absent), wired from the style card, its prompt starting with the style's trigger word, as one undo step. A card can only switch to a model its wiring fits, so this is how a card reaches a model that needs a style. It does not run: write the prompt and run the card as usual, which shows its cost first.",
            enabled: enabled_card,
            run: card,
        },
    ]
}
