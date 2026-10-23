//! Importing ComfyUI workflows as cards (#9). Reading a workflow needs the server's node list
//! (`/object_info`), which only a host can fetch.
//!
//! `comfy.nodes.refresh` asks the host's [`NODES_SERVICE`] to read the list; the host runs
//! [`fetch`] off the UI thread and hands the outcome to [`finish`], which installs it as a session
//! resource. A failed fetch keeps the list installed before. `comfy.nodes.status` reports the last
//! outcome. `comfy.workflow.import` turns a workflow file (API or UI format) into one
//! `generation.comfy_workflow` card using the installed list. Handlers do no I/O.
use crate::commands::{CommandSpec, Context, Effect, Phase, invalid, params};
use crate::provider_setup::HostEnv;
use crate::session::Session;
use nodecraft_comfy::{ApiWorkflow, NodeCatalog};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_nodes::comfy_workflow;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ParamValues};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

/// The host service that reads the ComfyUI server's node list.
pub const NODES_SERVICE: &str = "comfy.nodes.fetch";

/// A ComfyUI server's node list, as [`fetch`] reads it.
pub type NodeList = Arc<NodeCatalog>;

/// The canvas extent positions may use (as `node.create`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// At most this many inputs may be named as parameters.
const MAX_EXPOSE: usize = 256;
/// At most this many warnings are returned; the count says how many there were.
const MAX_WARNINGS: usize = 32;

/// The server's node list, a session resource.
struct ComfyNodes(NodeList);

/// The outcome of one fetch, which `comfy.nodes.status` reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub id: u64,
    /// How many node classes the server lists, when it answered.
    pub nodes: Option<usize>,
    /// Why the list couldn't be read; what was installed before stays.
    pub error: Option<String>,
}

struct LastFetch(Report);

/// Reads the node list of the ComfyUI server in `env`'s settings. The host calls it off the UI
/// thread: it may read the server's token from the keychain and calls the server.
pub async fn fetch(env: &HostEnv<'_>) -> Result<NodeList, Error> {
    let provider = crate::provider_setup::comfyui(env)
        .map_err(|readiness| Error::new(ErrorKind::Unsupported, readiness.message("ComfyUI").unwrap_or_else(|| "ComfyUI isn't set up".into())))?;
    provider
        .catalog()
        .await
        .map_err(|error| Error::new(crate::option_fetch::provider_kind(error.kind), format!("ComfyUI's node list couldn't be read: {}", error.message)))
}

/// Installs what fetch `id` brought back and records the outcome for `comfy.nodes.status`. A
/// failed fetch keeps the list installed before.
pub fn finish(session: &mut Session, id: u64, fetched: Result<NodeList, Error>) -> Report {
    let report = match fetched {
        Ok(catalog) => {
            let nodes = catalog.len();
            session.swap_resource(Some(ComfyNodes(catalog)));
            Report { id, nodes: Some(nodes), error: None }
        }
        Err(error) => Report { id, nodes: None, error: Some(error.message) },
    };
    session.swap_resource(Some(LastFetch(report.clone())));
    report
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn enabled_refresh(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    if !c.services.contains(NODES_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't read the ComfyUI server's nodes; the desktop app can"));
    }
    Ok(())
}

fn refresh(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_refresh(c, value)?;
    Ok(Effect::Service { capability: NODES_SERVICE, request: json!({}) })
}

fn enabled_status(_: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    Ok(())
}

fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: NoParams = params(value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let last = c.resources.get::<LastFetch>().map(|last| &last.0);
    let nodes = c.resources.get::<ComfyNodes>().map(|nodes| nodes.0.len());
    Ok(Effect::None { result: json!({"last": last, "nodes": nodes}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportParams {
    canvas: CanvasId,
    id: NodeId,
    pos: Position,
    /// The workflow file's text, in API or UI format.
    workflow: String,
    /// The card's title; the workflow's when absent.
    #[serde(default)]
    title: Option<String>,
    /// The inputs to make parameters, as `node.input`; the prompts and seeds when absent. File
    /// inputs are always ports.
    #[serde(default)]
    params: Option<Vec<String>>,
}

fn import(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: ImportParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    if p.workflow.len() > nodecraft_comfy::MAX_WORKFLOW_BYTES {
        return Err(Error::new(ErrorKind::Overflow, "this ComfyUI workflow is larger than 32 MiB"));
    }
    if p.params.as_ref().is_some_and(|names| names.len() > MAX_EXPOSE) {
        return Err(Error::new(ErrorKind::Overflow, format!("name at most {MAX_EXPOSE} parameters")));
    }
    if p.title.as_ref().is_some_and(|title| title.chars().count() > 256) {
        return Err(invalid("the title is longer than 256 characters"));
    }
    let Some(ComfyNodes(catalog)) = c.resources.get::<ComfyNodes>() else {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "NodeCraft hasn't read the ComfyUI server's nodes yet; refresh them (comfy.nodes.refresh), then import again",
        ));
    };
    let conversion = ApiWorkflow::convert(p.workflow.as_bytes(), Some(catalog))
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("this isn't a ComfyUI workflow NodeCraft can read: {e}")))?;
    let workflow = conversion.workflow;
    let check = workflow.check(catalog);
    if !check.missing_nodes.is_empty() {
        let mut missing = check.missing_nodes.iter().take(MAX_WARNINGS).map(|n| bounded(n, 128)).collect::<Vec<_>>().join(", ");
        let more = check.missing_nodes.len().saturating_sub(MAX_WARNINGS);
        if more > 0 {
            missing.push_str(&format!(" and {more} more"));
        }
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("the ComfyUI server doesn't have these nodes: {missing}. Install them, refresh the server's nodes, then import again"),
        ));
    }
    let mut exposed =
        nodecraft_comfy::discover(&workflow, catalog).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("this ComfyUI workflow can't be read: {e}")))?;
    let media = |input: &nodecraft_comfy::ExposedInput| matches!(input.kind, nodecraft_comfy::ExposedKind::Media(_));
    match &p.params {
        None => exposed.inputs.retain(|input| media(input) || input.role.exposed_by_default()),
        Some(names) => {
            let wanted: BTreeSet<&str> = names.iter().map(String::as_str).collect();
            let offered: BTreeSet<String> = exposed.inputs.iter().filter(|input| !media(input)).map(full_name).collect();
            if let Some(unknown) = wanted.iter().find(|name| !offered.contains(**name)) {
                return Err(Error::new(ErrorKind::InvalidInput, format!("the workflow has no input {} that can be a parameter", bounded(unknown, 128))));
            }
            exposed.inputs.retain(|input| media(input) || wanted.contains(full_name(input).as_str()));
        }
    }
    let config = comfy_workflow::propose(&workflow, &exposed)?;
    let mut node = c.kinds.get(comfy_workflow::ID)?.instantiate_instance(p.id, config, &ParamValues::new())?;
    node.pos = p.pos;
    if let Some(title) = p.title.filter(|t| !t.trim().is_empty()) {
        node.title = title;
    }
    // Seeds aren't re-randomised per run yet (docs/decisions/2026-10-23-comfy-workflow-card.md).
    let modes = conversion.controls.iter().filter(|(_, mode)| mode.as_str() != "fixed").map(|((node, input), mode)| {
        bounded(&format!("{node}.{input} was set to \"{mode}\" in ComfyUI; the card keeps its value fixed until you change it"), 512)
    });
    let warnings: Vec<String> = conversion
        .warnings
        .iter()
        .map(|w| bounded(w, 512))
        .chain(modes)
        .chain(check.problems.iter().map(|problem| bounded(&format!("{}.{}: {}", problem.node, problem.input, problem.message), 512)))
        .collect();
    let result = json!({
        "id": p.id,
        "ports": node.inputs.iter().map(|port| &port.name).collect::<Vec<_>>(),
        "params": node.params.keys().filter(|name| name.as_str() != comfy_workflow::MODEL).collect::<Vec<_>>(),
        "outputs": node.outputs.iter().map(|port| &port.name).collect::<Vec<_>>(),
        "warnings": warnings.iter().take(MAX_WARNINGS).collect::<Vec<_>>(),
        "more_warnings": warnings.len().saturating_sub(MAX_WARNINGS),
    });
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

fn enabled_import(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    c.editable()?;
    import(c, value).map(|_| ())
}

fn full_name(input: &nodecraft_comfy::ExposedInput) -> String {
    format!("{}.{}", input.node, input.input)
}

/// `text` cut to at most `max` bytes at a char boundary.
fn bounded(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "properties": {}, "required": []})
}

fn import_schema() -> Value {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "pos", "workflow"],
        "properties": {"canvas": uuid, "id": uuid,
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "workflow": {"type": "string", "maxLength": nodecraft_comfy::MAX_WORKFLOW_BYTES},
            "title": {"type": "string", "maxLength": 256},
            "params": {"type": "array", "maxItems": MAX_EXPOSE, "items": {"type": "string", "maxLength": 256}}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "comfy.nodes.refresh",
            label: "Read ComfyUI server nodes",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {pending: id}; reads the node list (/object_info) of the ComfyUI server in settings, which comfy.workflow.import needs. The host answers later: read the outcome with comfy.nodes.status. A list that can't be read keeps the one read before. Refused when the host can't fetch (the CLI).",
            enabled: enabled_refresh,
            run: refresh,
        },
        CommandSpec {
            id: "comfy.nodes.status",
            label: "ComfyUI server node status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {last: {id, nodes, error} | null, nodes: number | null}; the outcome of the last comfy.nodes.refresh, and how many node classes the list in use has (null before one was read). Read-only.",
            enabled: enabled_status,
            run: status,
        },
        CommandSpec {
            id: "comfy.workflow.import",
            label: "Import ComfyUI workflow",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: import_schema,
            params_doc: "{canvas, id, pos: {x, y}, workflow: string, title?: string, params?: [\"node.input\"]} -> {id, ports, params, outputs, warnings, more_warnings}; places a ComfyUI workflow file (API or UI format) as one card, read against the server's nodes from comfy.nodes.refresh. Its file inputs become ports, the inputs named in `params` (by default its prompts and seeds) become parameters, and its save nodes become results. Refused when the nodes haven't been read, the server lacks a node the workflow uses, or the workflow saves nothing. The caller supplies a new id.",
            enabled: enabled_import,
            run: import,
        },
    ]
}

#[cfg(test)]
mod tests;
