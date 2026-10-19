//! Typed card editing shared by the UI and automation surfaces.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleParams {
    pub canvas: CanvasId,
    pub id: NodeId,
    pub title: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamParams {
    pub canvas: CanvasId,
    pub id: NodeId,
    pub name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub value: Option<Value>,
}

fn title(_: &Context<'_>, json: &Json) -> Result<Effect, Error> {
    let p: TitleParams = params(json)?;
    if p.title.len() > 4096 {
        return Err(crate::commands::invalid("title exceeds 4096-byte limit"));
    }
    let title = p.title.trim().lines().next().unwrap_or("").to_owned();
    Ok(edit(Change::SetNodeTitle { canvas: p.canvas, id: p.id, title }))
}
fn parameter(c: &Context<'_>, json: &Json) -> Result<Effect, Error> {
    if json.get("value").and_then(|v| v.get("type")).and_then(Json::as_str) == Some("text")
        && json.get("value").and_then(|v| v.get("value")).and_then(Json::as_str).is_some_and(|text| text.len() > 1024 * 1024)
    {
        return Err(crate::commands::invalid("parameter text exceeds UTF-8 byte limit"));
    }
    let p: ParamParams = params(json)?;
    let node = c
        .document
        .canvases()
        .get(&p.canvas)
        .and_then(|canvas| canvas.nodes().get(&p.id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    let descriptor = c.kinds.get(&node.kind)?.resolve_instance(node)?;
    if node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
        return Err(crate::commands::invalid("node ports do not match its instance descriptor"));
    }
    if !descriptor.params.params.iter().any(|spec| spec.name == p.name) {
        return Err(Error::new(ErrorKind::InvalidInput, "unknown node parameter"));
    }
    if let Some(value) = &p.value
        && let Some(spec) = descriptor.params.params.iter().find(|spec| spec.name == p.name)
    {
        spec.control.validate_value(value)?;
    }
    let mut proposed = node.params.clone();
    match &p.value {
        Some(value) => {
            proposed.insert(p.name.clone(), value.clone());
        }
        None => {
            proposed.remove(&p.name);
        }
    }
    descriptor.params.resolve(&proposed, false)?;
    Ok(edit(Change::SetParam { canvas: p.canvas, id: p.id, name: p.name, value: p.value }))
}
fn edit(change: Change) -> Effect {
    Effect::Edit { changes: Changeset::new(vec![change]), result: Json::Null, gesture: None }
}
fn enabled(c: &Context<'_>, v: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    if let Effect::Edit { changes, .. } = run(c, v)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}
fn enabled_title(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, title)
}
fn enabled_parameter(c: &Context<'_>, v: &Json) -> Result<(), Error> {
    enabled(c, v, parameter)
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "node.title",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::title_schema,
            label: "Rename node",
            shortcut: None,
            params_doc: "{canvas: CanvasId, id: NodeId, title: string}; empty title uses the prompt or kind label.",
            enabled: enabled_title,
            run: title,
        },
        CommandSpec {
            id: "node.param_set",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::parameter_schema,
            label: "Set node parameter",
            shortcut: None,
            params_doc: "{canvas: CanvasId, id: NodeId, name: string, value: Value | null}; null removes the override and restores the schema default.",
            enabled: enabled_parameter,
            run: parameter,
        },
    ]
}
