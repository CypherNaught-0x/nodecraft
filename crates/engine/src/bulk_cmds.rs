//! Atomic bulk parameter edits admitted through each target's semantic contract.
mod domain;

use std::collections::BTreeSet;

use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

pub use domain::{parameter_owner, shared_node_specs, shared_specs};

pub const MAX_TARGETS: usize = 256;
const MAX_PROPOSED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkParams {
    pub canvas: CanvasId,
    pub ids: Vec<NodeId>,
    pub revision: u64,
    pub name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub value: Option<Value>,
}

fn prepare(c: &Context<'_>, json: &Json) -> Result<Effect, Error> {
    // Bound the collection before deserializing/cloning hostile caller-owned JSON.
    let ids = json.get("ids").and_then(Json::as_array).ok_or_else(|| invalid("bulk targets must be an array"))?;
    if !(2..=MAX_TARGETS).contains(&ids.len()) {
        return Err(invalid("bulk edits require between 2 and 256 targets"));
    }
    if json.get("name").and_then(Json::as_str).is_none_or(|name| name.is_empty() || name.len() > 128) {
        return Err(invalid("bulk parameter name must contain 1 to 128 bytes"));
    }
    if json.get("value").and_then(|value| value.get("value")).and_then(Json::as_str).is_some_and(|text| text.len() > 1024 * 1024) {
        return Err(invalid("bulk parameter text exceeds UTF-8 byte limit"));
    }
    json_budget(json, 0, &mut 0)?;
    let p: BulkParams = params(json)?;
    c.editable()?;
    if p.revision != c.revision {
        return Err(Error::new(ErrorKind::Conflict, "bulk selection is stale; refresh before editing"));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let mut seen = BTreeSet::new();
    let mut first_contract: Option<nodecraft_types::Contract> = None;
    let mut first_kind = None;
    let mut common_control = None;
    // Check every borrowed target before instance resolution or proposed-node cloning.
    check_selection_payload(
        p.ids
            .iter()
            .map(|id| canvas.nodes().get(id).ok_or_else(|| Error::new(ErrorKind::NotFound, "bulk target does not exist")))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let mut changes = Vec::with_capacity(p.ids.len());
    let mut proposed_bytes = 0usize;
    for id in p.ids {
        if !seen.insert(id) {
            return Err(invalid("bulk targets must be unique"));
        }
        let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "bulk target does not exist"))?;
        let kind = c.kinds.get(&node.kind)?;
        let generation = if nodecraft_nodes::generation::is_generation(&node.kind) {
            let snapshot = crate::generation_catalog::GenerationCatalog::current(c.resources)?;
            snapshot.validate_instance_binding(kind, node)?;
            Some(snapshot)
        } else {
            None
        };
        let instance = kind.resolve_instance(node)?;
        if node.inputs != instance.inputs || node.outputs != instance.outputs {
            return Err(invalid("bulk target ports do not match its instance descriptor"));
        }
        if first_kind.as_ref().is_some_and(|kind| kind != &node.kind) {
            return Err(invalid("bulk targets require the same node kind"));
        }
        first_kind.get_or_insert_with(|| node.kind.clone());
        let contract = c.kinds.get(&node.kind)?.instance_contract(node)?;
        let owner = parameter_owner(&contract, &p.name).ok_or_else(|| invalid("unknown bulk parameter"))?;
        if first_contract.as_ref().is_some_and(|first| first.compatible(&p.name, owner).is_err()) {
            return Err(invalid("bulk parameter has incompatible meanings, units or controls"));
        }
        if first_contract.is_none() {
            first_contract = Some(owner.clone());
        }
        let resolved = contract.resolve(&node.params, false)?;
        let state = resolved.get(&p.name).ok_or_else(|| invalid("unknown bulk parameter"))?;
        if !state.visible || !state.enabled {
            return Err(invalid("bulk parameter must be visible and enabled on every target"));
        }
        let spec = owner.schema.params.iter().find(|spec| spec.name == p.name).ok_or_else(|| invalid("unknown bulk parameter"))?;
        common_control = Some(match common_control {
            Some(ref common) => domain::intersect(common, &spec.control).ok_or_else(|| invalid("bulk parameter has no common edit domain"))?,
            None => spec.control.clone(),
        });
        if matches!(spec.control, nodecraft_types::ParamControl::Model) {
            return Err(Error::new(ErrorKind::Unsupported, "bulk model changes require the shared model migration command"));
        }
        if let Some(value) = &p.value {
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
        // A controlling edit may reset dependent hidden/disabled overrides. Publish these
        // removals in the SAME transaction, and admit checks only after resets settle.
        let mut resets = BTreeSet::new();
        for _ in 0..=instance.params.params.len() {
            let resolution = contract.resolve_with(&proposed, false, crate::option_cmds::lookup(c.resources))?;
            let pending: Vec<_> = resolution.params.iter().filter(|state| state.reset).map(|state| state.name.clone()).collect();
            if pending.is_empty() {
                if let Some(issue) = crate::option_cmds::refusing(&resolution.issues, p.value.as_ref().map(|_| p.name.as_str())) {
                    return Err(invalid(&issue.message));
                }
                break;
            }
            for name in pending {
                proposed.remove(&name);
                resets.insert(name);
            }
        }
        // Authoritative kind hooks may impose cross-field constraints or resolve changed
        // ports from the proposed values. Validate the complete node before any effect exists.
        let mut proposed_node = node.clone();
        proposed_node.params = proposed.clone();
        let kind = c.kinds.get(&node.kind)?;
        kind.validate_instance(&proposed_node, false)?;
        if let Some(generation) = generation {
            generation.validate_instance_binding(kind, &proposed_node)?;
            generation.validate_wiring(canvas, &proposed_node, false)?;
        }
        let final_contract = kind.instance_contract(&proposed_node)?;
        let final_resolution = final_contract.resolve_with(&proposed_node.params, false, crate::option_cmds::lookup(c.resources))?;
        if let Some(issue) = crate::option_cmds::refusing(&final_resolution.issues, p.value.as_ref().map(|_| p.name.as_str())) {
            return Err(invalid(&issue.message));
        }
        if final_resolution.params.iter().any(|state| state.reset) {
            return Err(invalid("bulk edit changes its contract and requires additional resets"));
        }
        let bytes = serde_json::to_vec(&proposed).map_err(|_| invalid("bulk parameters could not be encoded"))?.len();
        proposed_bytes = proposed_bytes.checked_add(bytes).ok_or_else(|| invalid("bulk parameter payload is too large"))?;
        if proposed_bytes > MAX_PROPOSED_BYTES {
            return Err(invalid("bulk proposed parameters exceed 16 MiB; use a smaller selection"));
        }
        changes.push(Change::SetParam { canvas: p.canvas, id, name: p.name.clone(), value: p.value.clone() });
        for name in resets {
            if name != p.name {
                changes.push(Change::SetParam { canvas: p.canvas, id, name, value: None });
            }
        }
    }
    Ok(Effect::Edit { changes: Changeset::new(changes), result: Json::Null, gesture: None })
}
/// Bound borrowed selection data before consumers resolve contracts or clone draft values.
pub fn check_selection_payload<'a>(nodes: impl IntoIterator<Item = &'a nodecraft_graph::Node>) -> Result<(), Error> {
    let mut bytes = 0;
    let mut count = 0usize;
    for node in nodes {
        count = count.saturating_add(1);
        if count > MAX_TARGETS {
            return Err(invalid("bulk selection exceeds target limit"));
        }
        for (name, value) in node.params.iter().chain(&node.config) {
            add_budget(&mut bytes, name.len())?;
            value_budget(value, 0, &mut bytes)?;
        }
    }
    Ok(())
}

// Borrowed, depth/count/byte bounded preflight before deserializing or cloning Values.
fn add_budget(bytes: &mut usize, extra: usize) -> Result<(), Error> {
    *bytes = bytes.checked_add(extra).ok_or_else(|| invalid("bulk payload is too large"))?;
    if *bytes > MAX_PROPOSED_BYTES {
        return Err(invalid("bulk payload exceeds 16 MiB"));
    }
    Ok(())
}
fn json_budget(value: &Json, depth: usize, bytes: &mut usize) -> Result<(), Error> {
    if depth > 16 {
        return Err(invalid("bulk payload nesting exceeds limit"));
    }
    add_budget(bytes, 64)?;
    match value {
        Json::String(text) => add_budget(bytes, text.len()),
        Json::Array(values) => {
            for value in values {
                json_budget(value, depth + 1, bytes)?;
            }
            Ok(())
        }
        Json::Object(values) => {
            for (name, value) in values {
                add_budget(bytes, name.len())?;
                json_budget(value, depth + 1, bytes)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
fn value_budget(value: &Value, depth: usize, bytes: &mut usize) -> Result<(), Error> {
    if depth > 16 {
        return Err(invalid("bulk parameter nesting exceeds limit"));
    }
    add_budget(bytes, 64)?;
    match value {
        Value::Text(text) => add_budget(bytes, text.len()),
        Value::Media(asset) => add_budget(bytes, asset.mime.as_str().len()),
        Value::List(values) => {
            for value in values {
                value_budget(value, depth + 1, bytes)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn enabled(c: &Context<'_>, json: &Json) -> Result<(), Error> {
    prepare(c, json).map(|_| ())
}
fn schema() -> Json {
    let mut schema = crate::command_schemas::parameter_schema();
    if let Some(properties) = schema.get_mut("properties").and_then(Json::as_object_mut) {
        properties.remove("id");
        properties
            .insert("ids".into(), json!({"type":"array","minItems":2,"maxItems":MAX_TARGETS,"uniqueItems":true,"items":{"type":"string","format":"uuid"}}));
        properties.insert("revision".into(), json!({"type":"integer","minimum":0,"maximum":u64::MAX}));
    }
    if let Some(object) = schema.as_object_mut() {
        object.insert("required".into(), json!(["canvas", "ids", "revision", "name", "value"]));
    }
    schema
}
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.params_set_bulk",
        label: "Set shared node parameter",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, ids: NodeId[2..256], revision, name, value: Value | null}; one atomic edit for semantically compatible instance contracts; null removes overrides.",
        enabled,
        run: prepare,
    }]
}
