//! Model choices use the same complete preview/commit for a card or a bulk selection.
use crate::{
    commands::{CommandSpec, Context, Effect, params},
    model_swap::{self, Selection},
};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    canvas: CanvasId,
    nodes: Vec<NodeId>,
    model: String,
    expected_revision: u64,
    preview_binding: String,
}
impl Commit {
    fn selection(&self) -> Selection {
        Selection { canvas: self.canvas, nodes: self.nodes.clone(), model: self.model.clone() }
    }
    fn check(&self, c: &Context<'_>, selection: &Selection) -> Result<(), Error> {
        if self.expected_revision != c.revision || self.preview_binding.len() != 64 || self.preview_binding != model_swap::binding(c, selection)? {
            return Err(Error::new(ErrorKind::Conflict, "model preview is stale; refresh and review the complete proposal"));
        }
        Ok(())
    }
}
fn preview(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let selection: Selection = params(value)?;
    let prepared = model_swap::prepare(c, &selection)?;
    let result = serde_json::to_value(prepared.preview).map_err(|_| crate::commands::invalid("model preview could not be encoded"))?;
    Ok(Effect::None { result })
}
fn commit(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    c.editable()?;
    let request: Commit = params(value)?;
    let selection = request.selection();
    request.check(c, &selection)?;
    let prepared = model_swap::prepare(c, &selection)?;
    let result = serde_json::to_value(prepared.preview).map_err(|_| crate::commands::invalid("model preview could not be encoded"))?;
    if prepared.changes.changes.is_empty() {
        return Ok(Effect::None { result });
    }
    Ok(Effect::Edit { changes: prepared.changes, result, gesture: None })
}
fn enabled_preview(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let selection: Selection = params(value)?;
    model_swap::binding(c, &selection).map(|_| ())
}
fn enabled_commit(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    c.editable()?;
    let request: Commit = params(value)?;
    request.check(c, &request.selection())
}
fn schema(commit: bool) -> Value {
    let mut result = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,
        "required":["canvas","nodes","model"],"properties":{
        "canvas":{"type":"string","format":"uuid"},
        "nodes":{"type":"array","minItems":1,"maxItems":nodecraft_graph::MAX_CHANGES,"uniqueItems":true,"items":{"type":"string","format":"uuid"}},
        "model":{"type":"string","minLength":1,"maxLength":128}}});
    if commit {
        if let Some(required) = result.get_mut("required").and_then(Value::as_array_mut) {
            required.extend([json!("expected_revision"), json!("preview_binding")]);
        }
        if let Some(properties) = result.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert("expected_revision".into(), json!({"type":"integer","minimum":0}));
            properties.insert("preview_binding".into(), json!({"type":"string","minLength":64,"maxLength":64}));
        }
    }
    result
}
fn preview_schema() -> Value {
    schema(false)
}
fn commit_schema() -> Value {
    schema(true)
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "node.model_preview",
            label: "Preview model change",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: preview_schema,
            params_doc: "{canvas: CanvasId, nodes: NodeId[], model: catalog ID}; complete read-only source-contract migration preview. Parameter values are not included in notices. No run or payment consent.",
            enabled: enabled_preview,
            run: preview,
        },
        CommandSpec {
            id: "node.set_model",
            label: "Change model",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: commit_schema,
            params_doc: "{canvas: CanvasId, nodes: NodeId[], model: catalog ID, expected_revision: u64, preview_binding: string}; atomically apply exactly the reviewed model preview, one Undo step. Obtain a new preview after any source/catalog change. No run or payment consent.",
            enabled: enabled_commit,
            run: commit,
        },
    ]
}
