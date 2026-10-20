//! Run-time option lists (#162): a parameter whose rule names an option source (the checkpoints
//! a ComfyUI server has) is checked against the list the host fetched, which it installs as an
//! [`OptionLists`] session resource. `node.options` returns a stored node's list for one
//! parameter, so the CLI, MCP and agents can pick a value the inspector would accept. Read-only.
use crate::commands::{CommandSpec, Context, Effect, Phase, Resources, params};
use nodecraft_types::{CanvasId, Error, ErrorKind, Issue, IssueCode, NoOptions, NodeId, OptionKey, OptionLists, OptionLookup};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host's fetched lists, or none.
pub(crate) fn lookup(resources: &Resources) -> &dyn OptionLookup {
    match resources.get::<OptionLists>() {
        Some(lists) => lists,
        None => &NoOptions,
    }
}

/// The first issue that refuses an edit setting `edited` (`None` when the edit clears it). A
/// value off its fetched list refuses only an edit setting that parameter, so a checkpoint
/// removed from the server never stops the rest of the node being edited, or the stale value
/// being cleared.
pub(crate) fn refusing<'a>(issues: &'a [Issue], edited: Option<&str>) -> Option<&'a Issue> {
    issues.iter().find(|issue| issue.code != IssueCode::NotAnOption || (edited.is_some() && issue.param.as_deref() == edited))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionsParams {
    canvas: CanvasId,
    id: NodeId,
    param: String,
}

fn node<'a>(c: &'a Context<'_>, p: &OptionsParams) -> Result<&'a nodecraft_graph::Node, Error> {
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    canvas.nodes().get(&p.id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))
}

fn options(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: OptionsParams = params(value)?;
    let node = node(c, &p)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let contract = c.kinds.get(&node.kind)?.instance_contract(node)?;
    let lists = c.resources.get::<OptionLists>();
    let resolution = contract.resolve_with(&node.params, false, lookup(c.resources))?;
    let state = resolution.get(&p.param).ok_or_else(|| Error::new(ErrorKind::NotFound, "the node has no such parameter"))?;
    let owner = contract.parameter_groups().find(|group| group.schema.params.iter().any(|spec| spec.name == p.param)).map(|group| group.namespace.as_str());
    let result = match (&state.options, owner) {
        (Some(listed), Some(owner)) => {
            // Unknown also covers a scoped source whose scope has no value, which must not
            // fall back to the unscoped list of the same name.
            let key = OptionKey { owner, source: &listed.source, scope: listed.scope.as_deref() };
            let choices = lists.filter(|_| listed.known).and_then(|lists| lists.get(key));
            json!({"source": listed.source, "scope": listed.scope, "known": choices.is_some(), "options": choices})
        }
        // A fixed list, or a parameter that is disabled or takes free values.
        _ => json!({"source": null, "scope": null, "known": false, "options": null}),
    };
    Ok(Effect::None { result })
}

fn enabled_options(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: OptionsParams = params(value)?;
    node(c, &p).map(|_| ())
}

fn options_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "param"],
        "properties": {"canvas": {"type": "string", "format": "uuid"}, "id": {"type": "string", "format": "uuid"}, "param": {"type": "string", "maxLength": 128}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.options",
        label: "List parameter options",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: options_schema,
        params_doc: "{canvas: CanvasId, id: NodeId, param: string} -> {source, scope, known, options}; the run-time list an enabled parameter's value must come from (docs/descriptor-contract.md, Option sources): its source id, the scope value it is narrowed by, and the values the host has fetched as [{value, label}], or known: false and options: null while the list is unknown. All four are null/false for a parameter without a source or while it is disabled. Read-only.",
        enabled: enabled_options,
        run: options,
    }]
}

#[cfg(test)]
mod tests;
