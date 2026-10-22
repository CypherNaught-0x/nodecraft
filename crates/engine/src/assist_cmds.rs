//! Prompt assistance (#582): `node.prompt_assist` puts a text Generate card in front of a Generate
//! card, asked to improve its typed prompt, and wires its answer into that card's prompt. The new
//! card is an ordinary one: it runs, is quoted and is paid for like any other, only when the user
//! runs it (or the card it feeds). Adding the card and the wire is one undo step.
//!
//! The typed prompt stays on the card it came from: a wired prompt replaces it only while the
//! wire is there, so removing the assist card or its wire brings the typed prompt back. Mentions
//! (`@[Title](id)`, #505) are copied into the assist card's prompt, so it reads the same cards.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use nodecraft_graph::{Change, Changeset, Edge, Endpoint, Position};
use nodecraft_nodes::generation::{self, AUDIO_KIND, GenerationModel, GenerationRole, MAX_PROMPT_BYTES, MODEL, PROMPT, TEXT_KIND};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// Longest instruction a caller may give, in bytes.
pub const MAX_INSTRUCTION_BYTES: usize = 4096;
/// Where the assist card goes, left of the card it feeds.
const OFFSET_X: f32 = -400.0;
/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// The assist card's title.
pub const TITLE: &str = "Improved prompt";

const IMAGE_INSTRUCTION: &str = "Rewrite the image prompt below so an image model can follow it closely. Keep every subject and idea it has, \
and make the subject, setting, composition, lighting and style concrete.";
const TEXT_INSTRUCTION: &str = "Rewrite the instruction below so a language model can follow it closely. Keep every request it makes, \
and say what to write, for whom, how long and in what tone.";
const REPLY: &str = "Reply with the rewritten prompt only.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssistParams {
    canvas: CanvasId,
    /// The Generate card whose prompt is improved.
    node: NodeId,
    /// The new text card's id.
    id: NodeId,
    /// The new wire's id.
    edge: EdgeId,
    /// The text model; by default one a text card on this canvas uses, or the first installed.
    #[serde(default)]
    model: Option<String>,
    /// What to ask for instead of the standard rewrite.
    #[serde(default)]
    instruction: Option<String>,
}

/// Whether `role` is a prompt the card can type: one inline text value.
fn typed_prompt(role: &GenerationRole) -> bool {
    role.role == PROMPT && role.modality == Modality::Text && role.inline && role.min <= 1 && role.max >= 1
}

/// Whether `model` can answer a typed prompt alone: a mode takes the typed prompt and needs
/// nothing else wired.
fn text_only(model: &GenerationModel) -> bool {
    model.modes.iter().any(|mode| mode.inputs.iter().any(typed_prompt) && mode.inputs.iter().all(|role| typed_prompt(role) || role.min == 0))
}

/// The text model the assist card uses.
fn pick_model(c: &Context<'_>, canvas: &nodecraft_graph::Canvas, asked: Option<&str>) -> Result<String, Error> {
    let catalog = GenerationCatalog::current(c.resources)?;
    if let Some(id) = asked {
        let model = catalog.model(TEXT_KIND, id).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no text model is called {id}")))?;
        if !text_only(model) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("{id} can't answer a prompt on its own; choose a text model with a text-only mode")));
        }
        return Ok(model.id.clone());
    }
    let usable = |id: &str| catalog.model(TEXT_KIND, id).is_some_and(text_only);
    let on_canvas = canvas.nodes().values().filter(|node| node.kind == TEXT_KIND).find_map(|node| match node.params.get(MODEL) {
        Some(Value::Text(id)) if usable(id) => Some(id.clone()),
        _ => None,
    });
    on_canvas
        .or_else(|| catalog.models(TEXT_KIND).find(|model| text_only(model)).map(|model| model.id.clone()))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no text model is installed; add one to the model catalog to improve prompts"))
}

fn ask<'a>(kind: &str, instruction: Option<&'a str>) -> &'a str {
    match instruction {
        Some(instruction) => instruction.trim(),
        None if kind == TEXT_KIND => TEXT_INSTRUCTION,
        None => IMAGE_INSTRUCTION,
    }
}

/// How far a typed prompt of `typed` bytes, asked about with `instruction`, is over the prompt
/// limit once composed; `None` when it fits.
fn overflow(kind: &str, typed: usize, instruction: Option<&str>) -> Option<usize> {
    let ask = ask(kind, instruction).len().saturating_add(REPLY.len()).saturating_add(SEPARATOR.len() + 1);
    typed.saturating_add(ask).checked_sub(MAX_PROMPT_BYTES).filter(|over| *over > 0)
}

/// Whether the standard rewrite of a `kind` card's typed prompt `typed` fits a prompt. The
/// toolbar checks it every frame; the command checks it again.
pub fn fits(kind: &str, typed: &str) -> bool {
    overflow(kind, typed.len(), None).is_none()
}

const SEPARATOR: &str = "\n\n---\n";

/// The assist card's prompt: the instruction, then the prompt to improve.
fn compose(kind: &str, typed: &str, instruction: Option<&str>) -> String {
    format!("{}\n{REPLY}{SEPARATOR}{typed}", ask(kind, instruction))
}

fn assist(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AssistParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let target = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if !generation::is_generation(&target.kind) {
        return Err(invalid("only a Generate card's prompt can be improved"));
    }
    // A speech model says its prompt word for word; a rewrite would change what it says (#672).
    if target.kind == AUDIO_KIND {
        return Err(invalid("a Generate audio card's prompt is what it says, so it isn't rewritten"));
    }
    let catalog = GenerationCatalog::current(c.resources)?;
    let takes_prompt = match target.params.get(MODEL) {
        Some(Value::Text(id)) => {
            catalog.model(&target.kind, id).is_some_and(|model| model.modes.iter().any(|mode| mode.inputs.iter().any(|role| role.role == PROMPT)))
        }
        _ => false,
    };
    if !takes_prompt {
        return Err(invalid("this card's model takes no prompt"));
    }
    if canvas.edges().values().any(|edge| edge.to.node == p.node && edge.to.port == PROMPT) {
        return Err(Error::new(ErrorKind::Conflict, "the prompt is already connected to another card; disconnect it to improve the typed prompt"));
    }
    let typed = match target.params.get(PROMPT) {
        Some(Value::Text(text)) if !text.trim().is_empty() => text.as_str(),
        _ => return Err(invalid("write a prompt first: the text model improves the card's typed prompt")),
    };
    if let Some(instruction) = &p.instruction
        && (instruction.trim().is_empty() || instruction.len() > MAX_INSTRUCTION_BYTES)
    {
        return Err(Error::new(ErrorKind::InvalidInput, format!("an instruction must have 1 to {MAX_INSTRUCTION_BYTES} bytes, not only spaces")));
    }
    if let Some(over) = overflow(&target.kind, typed.len(), p.instruction.as_deref()) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("the prompt is {over} bytes too long to improve; shorten it")));
    }
    if canvas.nodes().contains_key(&p.id) {
        return Err(Error::new(ErrorKind::Conflict, "that node id is taken"));
    }
    if canvas.edges().contains_key(&p.edge) {
        return Err(Error::new(ErrorKind::Conflict, "that edge id is taken"));
    }
    let model = pick_model(c, canvas, p.model.as_deref())?;
    let values = BTreeMap::from([
        (MODEL.to_owned(), Value::Text(model.clone())),
        (PROMPT.to_owned(), Value::Text(compose(&target.kind, typed, p.instruction.as_deref()))),
    ]);
    let mut node = c.kinds.get(TEXT_KIND)?.instantiate_instance(p.id, BTreeMap::new(), &values.into_iter().collect())?;
    node.title = TITLE.into();
    let x = target.pos.x + OFFSET_X;
    node.pos = if x.is_finite() && x.abs() <= MAX_COORDINATE { Position { x, y: target.pos.y } } else { target.pos };
    let out = node
        .outputs
        .iter()
        .find(|port| port.port_type.modality == Modality::Text && !port.port_type.many)
        .map(|port| port.name.clone())
        .ok_or_else(|| Error::new(ErrorKind::Internal, "the text model's card has no text output"))?;
    let edge = Edge { id: p.edge, from: Endpoint { node: p.id, port: out }, to: Endpoint { node: p.node, port: PROMPT.into() }, order: 0 };
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }, Change::Connect { canvas: p.canvas, edge }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "edge": p.edge, "model": model}), gesture: None })
}

fn enabled_assist(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    c.editable()?;
    assist(c, value).map(|_| ())
}

fn assist_schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "node", "id", "edge"],
        "properties": {"canvas": uuid, "node": uuid, "id": uuid, "edge": uuid,
            "model": {"type": "string", "maxLength": 256},
            "instruction": {"type": "string", "minLength": 1, "maxLength": MAX_INSTRUCTION_BYTES, "description": "at most this many UTF-8 bytes"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.prompt_assist",
        label: "Improve prompt",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: assist_schema,
        params_doc: "{canvas, node: a Generate card, id: new NodeId, edge: new EdgeId, model?: text model id, instruction?: text of at most 4096 bytes} -> {id, edge, model}; adds a text Generate card left of `node`, asked to rewrite its typed prompt (or to follow `instruction`), and wires its answer into `node`'s prompt, as one undo step. Nothing runs: run the new card, or `node`, with run.quote/run.start. `model` defaults to a text model already used on this canvas, else the first installed. Refused for a Generate audio card (it says its prompt as written), when the prompt is empty, already connected or too long to improve, or when no installed text model answers a typed prompt alone.",
        enabled: enabled_assist,
        run: assist,
    }]
}
