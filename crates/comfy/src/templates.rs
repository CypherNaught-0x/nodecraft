//! Pinned, versioned choices of public fields. A structural match selects controls; it never
//! replaces values or changes the runnable graph. Unknown graphs retain ordinary discovery.
use crate::{ComfyError, Exposed, ExposedKind, PromotedInput, json_bounds::Budget};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    repository: String,
    commit: String,
    profiles: Vec<Profile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    source_blob: String,
    usage: u64,
    shape: Value,
    fields: Vec<Field>,
    outputs: Vec<Output>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    node: Option<String>,
    input: String,
    label: String,
    kind: Kind,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Text,
    Int,
    Float,
    Bool,
    Choice,
    Media,
}
impl Kind {
    fn accepts(&self, kind: &ExposedKind) -> bool {
        matches!(
            (self, kind),
            (Self::Text, ExposedKind::Text { .. })
                | (Self::Int, ExposedKind::Int { .. })
                | (Self::Float, ExposedKind::Float { .. })
                | (Self::Bool, ExposedKind::Bool)
                | (Self::Choice, ExposedKind::Choice { .. })
                | (Self::Media, ExposedKind::Media(_))
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    node: String,
    label: String,
}

fn invalid() -> ComfyError {
    ComfyError::InvalidWorkflow("the bundled ComfyUI template mappings are invalid".into())
}
fn manifest() -> Result<&'static Manifest, ComfyError> {
    static DATA: OnceLock<Result<Manifest, ComfyError>> = OnceLock::new();
    DATA.get_or_init(|| {
        let data: Manifest = serde_json::from_str(include_str!("../../../assets/comfy/template-mappings.v1.json")).map_err(|_| invalid())?;
        if data.version != 1 || data.repository != "Comfy-Org/workflow_templates" || data.commit.len() != 40 || data.profiles.len() > 32 {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        for profile in &data.profiles {
            if profile.id.is_empty()
                || !ids.insert(&profile.id)
                || profile.id.len() > 128
                || profile.source_blob.len() != 40
                || profile.fields.len() > 256
                || profile.outputs.is_empty()
                || profile.outputs.len() > 256
            {
                return Err(invalid());
            }
            Budget::default().value(&profile.shape, 0)?;
            let bounded = |s: &str| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
            for field in &profile.fields {
                if field.instance.is_some() == field.node.is_some()
                    || !bounded(&field.input)
                    || !bounded(&field.label)
                    || !bounded(field.instance.as_deref().or(field.node.as_deref()).unwrap_or_default())
                {
                    return Err(invalid());
                }
            }
            if profile.outputs.iter().any(|o| !bounded(&o.node) || !bounded(&o.label)) {
                return Err(invalid());
            }
            let _ = profile.usage;
        }
        Ok(data)
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn keys(value: &Value, names: &[&str], budget: &mut Budget) -> Result<Value, ComfyError> {
    budget.container(names.len(), 0)?;
    let mut out = Map::new();
    for &name in names {
        if let Some(value) = value.get(name) {
            budget.text(name, 0)?;
            budget.value(value, 0)?;
            out.insert(name.into(), value.clone());
        }
    }
    Ok(Value::Object(out))
}

/// The public graph contract, excluding prompts, model choices, titles, layout and other widget
/// values. Port order, links, execution modes, proxy bindings and widget counts remain significant.
fn shape(root: &Value) -> Result<Value, ComfyError> {
    Budget::default().value(root, 0)?;
    fn ports(root: &Value, name: &str, budget: &mut Budget) -> Result<Value, ComfyError> {
        let ports = root.get(name).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        budget.container(ports.len(), 0)?;
        ports.iter().map(|p| keys(p, &["name", "type", "link", "links", "linkIds", "widget"], budget)).collect()
    }
    fn scope(root: &Value, budget: &mut Budget) -> Result<Value, ComfyError> {
        let mut out = keys(root, &["id", "links"], budget)?;
        let Some(out) = out.as_object_mut() else { return Err(invalid()) };
        for name in ["inputNode", "outputNode"] {
            if let Some(boundary) = root.get(name) {
                out.insert(name.into(), keys(boundary, &["id"], budget)?);
            }
        }
        let nodes = root.get("nodes").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        budget.container(nodes.len(), 0)?;
        let mut sorted = Vec::with_capacity(nodes.len());
        for node in nodes {
            let mut projected = keys(node, &["id", "type"], budget)?;
            let Some(obj) = projected.as_object_mut() else { return Err(invalid()) };
            let mode = node.get("mode").unwrap_or(&Value::Null);
            budget.value(mode, 0)?;
            obj.insert("mode".into(), if mode.is_null() { Value::from(0) } else { mode.clone() });
            for name in ["inputs", "outputs"] {
                obj.insert(name.into(), ports(node, name, budget)?);
            }
            let widgets = node.get("widgets_values");
            let count = widgets.and_then(Value::as_array).map_or(0, Vec::len);
            obj.insert("widget_count".into(), Value::from(count));
            if let Some(widgets) = widgets.and_then(Value::as_object) {
                budget.container(widgets.len(), 0)?;
                let mut names = Vec::new();
                for key in widgets.keys() {
                    budget.text(key, 0)?;
                    names.push(Value::String(key.clone()));
                }
                obj.insert("widget_names".into(), Value::Array(names));
            }
            if let Some(proxies) = node.get("properties").and_then(|p| p.get("proxyWidgets")) {
                budget.value(proxies, 0)?;
                obj.insert("proxies".into(), proxies.clone());
            }
            sorted.push(projected);
        }
        sorted.sort_by_key(|n| n.get("id").map(Value::to_string));
        out.insert("nodes".into(), Value::Array(sorted));
        out.insert("inputs".into(), ports(root, "inputs", budget)?);
        out.insert("outputs".into(), ports(root, "outputs", budget)?);
        Ok(Value::Object(std::mem::take(out)))
    }
    let mut budget = Budget::default();
    let mut root_shape = scope(root, &mut budget)?;
    // The root workflow UUID is editor state, unlike definition ids used as node classes.
    if let Some(root_shape) = root_shape.as_object_mut() {
        root_shape.remove("id");
    }
    let definitions = root.get("definitions").and_then(|v| v.get("subgraphs")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    budget.container(definitions.len(), 0)?;
    let mut shapes = definitions.iter().map(|d| scope(d, &mut budget)).collect::<Result<Vec<_>, _>>()?;
    shapes.sort_by_key(|d| d.get("id").map(Value::to_string));
    Ok(serde_json::json!({"root":root_shape,"definitions":shapes}))
}

pub(crate) fn matching(root: &Value) -> Result<Option<&'static str>, ComfyError> {
    let data = manifest()?;
    // Avoid projecting API exports, and avoid work when no bundled profile has this node count.
    let Some(nodes) = root.get("nodes").and_then(Value::as_array) else { return Ok(None) };
    let candidates =
        data.profiles.iter().filter(|p| p.shape.get("root").and_then(|r| r.get("nodes")).and_then(Value::as_array).is_some_and(|n| n.len() == nodes.len()));
    let candidates = candidates.collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let actual = shape(root)?;
    Ok(candidates.into_iter().find(|p| p.shape == actual).map(|p| p.id.as_str()))
}

/// Apply a matched template's choices atomically. If server metadata differs, return false and
/// leave discovery intact. Paid-node evidence and every discovered upload remain available.
pub fn curate_template(id: Option<&str>, promoted: &[PromotedInput], exposed: &mut Exposed) -> Result<bool, ComfyError> {
    let Some(id) = id else { return Ok(false) };
    let Some(profile) = manifest()?.profiles.iter().find(|p| p.id == id) else { return Ok(false) };
    let mut wanted = Vec::new();
    for field in &profile.fields {
        let target = if let Some(instance) = &field.instance {
            let Some(promoted) = promoted.iter().find(|p| p.instance == *instance && p.name == field.input) else { return Ok(false) };
            let [target] = promoted.targets.as_slice() else { return Ok(false) };
            (target.0.as_str(), target.1.as_str())
        } else {
            (field.node.as_deref().unwrap_or_default(), field.input.as_str())
        };
        let Some(index) = exposed.inputs.iter().position(|i| i.node == target.0 && i.input == target.1 && field.kind.accepts(&i.kind)) else {
            return Ok(false);
        };
        if wanted.iter().any(|(i, _)| *i == index) {
            return Ok(false);
        }
        wanted.push((index, field.label.as_str()));
    }
    // Never lose a file upload simply because a server adds another optional loader input.
    for (index, input) in exposed.inputs.iter().enumerate() {
        if matches!(input.kind, ExposedKind::Media(_)) && !wanted.iter().any(|(i, _)| *i == index) {
            wanted.push((index, input.label.as_str()));
        }
    }
    let mut results = Vec::new();
    for output in &profile.outputs {
        let Some(index) = exposed.outputs.iter().position(|o| o.node == output.node && !o.preview) else { return Ok(false) };
        if results.iter().any(|(i, _)| *i == index) {
            return Ok(false);
        }
        results.push((index, output.label.as_str()));
    }
    // Resolve labels before taking the vectors: some extra upload labels borrow discovery.
    let wanted = wanted.into_iter().map(|(i, label)| (i, label.to_owned())).collect::<Vec<_>>();
    let mut inputs = std::mem::take(&mut exposed.inputs).into_iter().map(Some).collect::<Vec<_>>();
    for (index, label) in wanted {
        if let Some(mut input) = inputs.get_mut(index).and_then(Option::take) {
            input.label = label;
            exposed.inputs.push(input);
        }
    }
    let mut outputs = std::mem::take(&mut exposed.outputs).into_iter().map(Some).collect::<Vec<_>>();
    for (index, label) in results {
        if let Some(mut output) = outputs.get_mut(index).and_then(Option::take) {
            output.label = label.to_owned();
            exposed.outputs.push(output);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
