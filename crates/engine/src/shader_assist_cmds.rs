//! Shaders written by a model (#90). `shader.write` puts a text Generate card beside a custom
//! shader node, asked to write (or fix) its shader; the card is an ordinary one, quoted and paid
//! for only when it runs. `shader.apply` reads the card's answer, validates it as a shader and
//! stores it on the node, as `node.shader_set` does. Nothing a model writes reaches a GPU before
//! it passes the same validation as a shader a person typed.
//!
//! The model answers with one fenced WGSL block whose parameters are declared in comment lines,
//! `// param: name | Label | min | max | default`: code a model can write without escaping it
//! into JSON, and still valid WGSL as written.
use crate::assist_cmds::pick_model;
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_nodes::generation::{MODEL, PROMPT, TEXT_KIND};
use nodecraft_nodes::shader::{ID, StoredParam, StoredShader, stored};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The config key linking a shader request card to the shader node it writes for.
pub const FOR_CONFIG: &str = "nodecraft.shader_for";
/// Longest request a caller may make, in bytes.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// Longest compiler message passed on for a fix, in bytes.
pub const MAX_ERROR_BYTES: usize = 4096;
/// Longest answer read as a shader: the source limit, with room for the model's prose.
const MAX_REPLY_BYTES: usize = 2 * nodecraft_gpu::shader::MAX_SOURCE;
/// The request card's title.
pub const TITLE: &str = "Shader request";
/// Where the request card goes, left of the shader node.
const OFFSET_X: f32 = -400.0;
/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// How a parameter declaration line starts.
const PARAM_LINE: &str = "// param:";
/// Longest parameter name or label in a shader shown to the model, in bytes.
const MAX_NAME_BYTES: usize = 256;

const CONTRACT: &str = "You write image filters for NodeCraft's custom shader node, in WGSL.

A filter is one function, called once for every pixel of the image:

    fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>

`color` is the pixel's RGBA, each 0 to 1. `uv` is its position, 0 to 1 across and down the image. \
Return the new RGBA; it is clamped to 0 to 1.

You may also call:
- `pixel(x: i32, y: i32) -> vec4<f32>`: the source pixel at (x, y), clamped to the image's edges;
- `size() -> vec2<f32>`: the image's width and height in pixels;
- one function per parameter you declare, named after it and returning its value as f32: `strength()`.

Rules: no loops of any kind (unroll a small fixed number of taps by hand), no bindings, globals, \
overrides or entry points, no arrays over 16 KiB, and at most a few thousand operations a pixel. \
You may write helper functions. Declare up to 16 float parameters, one comment line each, in the code:

    // param: name | Label | min | max | default

Names are lowercase letters, digits and underscores, start with a letter, and aren't `effect`, \
`pixel`, `size` or begin with `nc_`. Labels are short words a person reads beside a slider.

Reply with the whole shader, parameter lines included, in one fenced code block marked wgsl, and nothing else.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteParams {
    canvas: CanvasId,
    /// The custom shader node to write for.
    node: NodeId,
    /// The new text card's id.
    id: NodeId,
    /// What the shader should do, or how to change it.
    #[serde(default)]
    request: Option<String>,
    /// Why the current shader was refused, to be fixed.
    #[serde(default)]
    error: Option<String>,
    /// The text model; by default one a text card on this canvas uses, or the first installed.
    #[serde(default)]
    model: Option<String>,
    /// The shader to show the model instead of the stored one: an editor's unsaved draft, or the
    /// answer `error` refused. Need not be valid; it is what the message is about.
    #[serde(default)]
    shader: Option<StoredShader>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyParams {
    canvas: CanvasId,
    /// The custom shader node to store the shader on.
    id: NodeId,
    /// The card whose active answer holds the shader.
    from: NodeId,
}

fn bounded(name: &str, text: Option<&str>, limit: usize) -> Result<Option<String>, Error> {
    match text {
        None => Ok(None),
        Some(text) if text.trim().is_empty() || text.len() > limit => {
            Err(Error::new(ErrorKind::InvalidInput, format!("a {name} must have 1 to {limit} bytes, not only spaces")))
        }
        Some(text) => Ok(Some(text.trim().to_owned())),
    }
}

/// The request card's prompt: the contract, the shader as it is, why it was refused, and what to
/// change.
fn compose(current: &StoredShader, request: Option<&str>, error: Option<&str>) -> String {
    // The declarations go last, so the compiler's line numbers still match the source.
    let mut prompt = format!("{CONTRACT}\n\n---\nThe shader as it is now:\n\n```wgsl\n{}\n", current.source.trim_end());
    prompt.push_str(&declarations(&current.params));
    prompt.push_str("```\n");
    if let Some(error) = error {
        prompt.push_str(&format!("\nIt was refused with this message; fix it:\n\n{error}\n"));
    }
    match request {
        Some(request) => prompt.push_str(&format!("\nWrite the shader that does this (keep what still fits from the one above):\n\n{request}\n")),
        None => prompt.push_str("\nKeep what it does; change only what the message asks for.\n"),
    }
    prompt
}

/// `params` as declaration lines, each ending in a newline. A `|` in a label would read as a
/// field separator, so it is shown as `/`.
fn declarations(params: &[StoredParam]) -> String {
    params.iter().map(|p| format!("{PARAM_LINE} {} | {} | {} | {} | {}\n", p.name, p.label.replace('|', "/"), p.min, p.max, p.default)).collect()
}

/// A caller's shader for the prompt, within the limits of a stored one. It needn't validate.
fn shown(shader: StoredShader) -> Result<StoredShader, Error> {
    let too_long = |text: &str| text.len() > MAX_NAME_BYTES || text.contains('\n');
    if shader.source.len() > nodecraft_gpu::shader::MAX_SOURCE
        || shader.params.len() > nodecraft_gpu::shader::MAX_PARAMS
        || shader.params.iter().any(|p| too_long(&p.name) || too_long(&p.label))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "a shader to show the model is at most {} KiB with at most {} parameters, each name and label one line of at most {MAX_NAME_BYTES} bytes",
                nodecraft_gpu::shader::MAX_SOURCE / 1024,
                nodecraft_gpu::shader::MAX_PARAMS
            ),
        ));
    }
    Ok(shader)
}

fn write(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: WriteParams = params(value)?;
    let request = bounded("request", p.request.as_deref(), MAX_REQUEST_BYTES)?;
    let error = bounded("compiler message", p.error.as_deref(), MAX_ERROR_BYTES)?;
    if request.is_none() && error.is_none() {
        return Err(invalid("say what the shader should do, or pass the message it was refused with"));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let target = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if target.kind != ID {
        return Err(invalid("only a custom shader node has a shader to write"));
    }
    // A damaged stored shader is written afresh rather than shown to the model.
    let current = match p.shader {
        Some(shader) => shown(shader)?,
        None => stored(target).unwrap_or_default(),
    };
    if canvas.nodes().contains_key(&p.id) {
        return Err(Error::new(ErrorKind::Conflict, "that node id is taken"));
    }
    let model = pick_model(c, canvas, p.model.as_deref())?;
    let values = BTreeMap::from([
        (MODEL.to_owned(), Value::Text(model.clone())),
        (PROMPT.to_owned(), Value::Text(compose(&current, request.as_deref(), error.as_deref()))),
    ]);
    let config = BTreeMap::from([(FOR_CONFIG.to_owned(), Value::Text(p.node.to_string()))]);
    let mut node = c.kinds.get(TEXT_KIND)?.instantiate_instance(p.id, config, &values.into_iter().collect())?;
    node.title = TITLE.into();
    let x = target.pos.x + OFFSET_X;
    node.pos = if x.is_finite() && x.abs() <= MAX_COORDINATE { Position { x, y: target.pos.y } } else { target.pos };
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "model": model}), gesture: None })
}

/// The shader node a request card writes for, if it is one.
pub fn written_for(node: &nodecraft_graph::Node) -> Option<NodeId> {
    match node.config.get(FOR_CONFIG) {
        Some(Value::Text(id)) => id.parse().ok(),
        _ => None,
    }
}

/// The shader in a model's answer: the first fenced code block (the whole answer without one),
/// with its `// param:` lines read as declarations and left out of the source. `Err` says what is wrong, for the person
/// or the model to act on.
pub fn parse_reply(reply: &str) -> Result<StoredShader, String> {
    if reply.len() > MAX_REPLY_BYTES {
        return Err(format!("the answer is longer than {MAX_REPLY_BYTES} bytes, too long for a shader"));
    }
    let code = fenced(reply).unwrap_or(reply).trim();
    if code.is_empty() {
        return Err("the answer holds no shader".into());
    }
    let mut params = Vec::new();
    let mut source = String::with_capacity(code.len().saturating_add(1));
    for line in code.lines() {
        let Some(declaration) = line.trim().strip_prefix(PARAM_LINE) else {
            source.push_str(line);
            source.push('\n');
            continue;
        };
        if params.len() >= nodecraft_gpu::shader::MAX_PARAMS {
            return Err(format!("the shader declares more than {} parameters", nodecraft_gpu::shader::MAX_PARAMS));
        }
        params.push(parameter(declaration)?);
    }
    // Parameters are stored apart from the source, so the lines declaring them aren't kept.
    let source = source.trim().to_owned();
    if source.is_empty() {
        return Err("the answer holds no shader, only parameter lines".into());
    }
    Ok(StoredShader { source: format!("{source}\n"), params })
}

/// The text of the first fenced block tagged `wgsl` in `reply`, else of its first fenced block,
/// without fences or tag. A block left open (an answer cut off) runs to the end.
fn fenced(reply: &str) -> Option<&str> {
    let mut first = None;
    let mut rest = reply;
    while let Some(open) = rest.find("```") {
        let after = rest.get(open.saturating_add(3)..)?;
        // The rest of the opening line is a language tag.
        let Some(end) = after.find('\n') else { break };
        let tag = after.get(..end)?.trim();
        let body = after.get(end.saturating_add(1)..)?;
        let (code, next) = match body.find("```") {
            Some(close) => (body.get(..close)?, body.get(close.saturating_add(3)..)?),
            None => (body, ""),
        };
        if tag.eq_ignore_ascii_case("wgsl") {
            return Some(code);
        }
        first.get_or_insert(code);
        rest = next;
    }
    first
}

/// The text of `card`'s active answer.
pub fn answer(card: &nodecraft_graph::Node) -> Result<&str, Error> {
    let answer = card
        .active
        .and_then(|active| card.history.iter().find(|generation| generation.id == active))
        .and_then(|generation| generation.outputs.first())
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "the card has no answer yet; run it first"))?;
    match answer {
        Value::Text(answer) => Ok(answer),
        _ => Err(invalid("the card's answer isn't text")),
    }
}

fn parameter(declaration: &str) -> Result<StoredParam, String> {
    let fields: Vec<&str> = declaration.split('|').map(str::trim).collect();
    let [name, label, min, max, default] = fields.as_slice() else {
        return Err(format!("a parameter line must read `{PARAM_LINE} name | Label | min | max | default`, not `{PARAM_LINE}{declaration}`"));
    };
    let number = |field: &str, text: &str| -> Result<f64, String> {
        text.parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| format!("parameter {name}'s {field} isn't a number: {text}"))
    };
    Ok(StoredParam {
        name: (*name).to_owned(),
        label: (*label).to_owned(),
        min: number("min", min)?,
        max: number("max", max)?,
        default: number("default", default)?,
    })
}

fn apply(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ApplyParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let target = canvas.nodes().get(&p.id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if target.kind != ID {
        return Err(invalid("only a custom shader node takes a shader"));
    }
    let from = canvas.nodes().get(&p.from).ok_or_else(|| Error::new(ErrorKind::NotFound, "the answering card does not exist"))?;
    let shader = parse_reply(answer(from)?).map_err(|message| Error::new(ErrorKind::InvalidInput, format!("the answer isn't a shader: {message}")))?;
    crate::shader_cmds::set_shader(c, p.canvas, p.id, shader)
}

fn enabled_write(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    c.editable()?;
    write(c, value).map(|_| ())
}

fn enabled_apply(c: &Context<'_>, _: &Json) -> Result<(), Error> {
    c.editable()
}

fn write_schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "node", "id"],
        "properties": {"canvas": uuid, "node": uuid, "id": uuid,
            "request": {"type": "string", "minLength": 1, "maxLength": MAX_REQUEST_BYTES, "description": "what the shader should do; at most this many UTF-8 bytes"},
            "error": {"type": "string", "minLength": 1, "maxLength": MAX_ERROR_BYTES, "description": "the message the current shader was refused with, to fix"},
            "model": {"type": "string", "maxLength": 256},
            "shader": {"type": "object", "additionalProperties": false, "required": ["source", "params"], "description": "the shader to show instead of the stored one (a draft, or a refused answer); need not be valid",
                "properties": {"source": {"type": "string", "maxLength": nodecraft_gpu::shader::MAX_SOURCE},
                    "params": {"type": "array", "maxItems": nodecraft_gpu::shader::MAX_PARAMS, "items": {"type": "object", "required": ["name", "label", "min", "max", "default"],
                        "properties": {"name": {"type": "string"}, "label": {"type": "string"}, "min": {"type": "number"}, "max": {"type": "number"}, "default": {"type": "number"}}}}}}}})
}

fn apply_schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "from"], "properties": {"canvas": uuid, "id": uuid, "from": uuid}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "shader.write",
            label: "Write shader with a model",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: write_schema,
            params_doc: "{canvas, node: a custom shader node, id: new NodeId, request?: text of at most 4096 bytes, error?: the message its shader was refused with, model?: text model id, shader?: {source, params} to show instead of the stored shader (an unsaved draft, or the answer `error` refused; need not be valid)} -> {id, model}; adds a text Generate card left of `node`, asked to write a shader that does `request` (or to fix the current one for `error`), as one undo step. Give `request`, `error` or both. Nothing runs: quote and run the new card with run.quote/run.start, then store its answer with shader.apply. `model` defaults to a text model already used on this canvas, else the first installed.",
            enabled: enabled_write,
            run: write,
        },
        CommandSpec {
            id: "shader.apply",
            label: "Use the written shader",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: apply_schema,
            params_doc: "{canvas, id: a custom shader node, from: a card with a text answer (usually the card shader.write added; any text card is read)} -> {changed}; reads the shader from `from`'s active answer (one fenced WGSL block, parameters declared as `// param: name | Label | min | max | default` lines), validates it as node.shader_set does and stores it, as one undo step. Refused with the compiler's message when it isn't valid; pass that to shader.write as `error` to have it fixed.",
            enabled: enabled_apply,
            run: apply,
        },
    ]
}

#[cfg(test)]
mod tests;
