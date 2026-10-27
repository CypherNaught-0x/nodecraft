//! ComfyUI workflows: the API format the server executes, and conversion from the UI format the
//! ComfyUI editor saves.
//!
//! API format: `{"<node id>": {"class_type": "KSampler", "inputs": {"steps": 20,
//! "model": ["4", 0]}, "_meta": {"title": "…"}}}`, where a two-element `[node id, output index]`
//! array is a connection. UI format: `{"nodes": [...], "links": [...]}` with widget values stored
//! by position, which needs the server's [`NodeCatalog`] to map back to input names.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value};

use crate::ComfyError;
use crate::object_info::{InputKind, NodeCatalog};
mod subgraphs;

/// Cap on a workflow file.
pub const MAX_WORKFLOW_BYTES: usize = 32 * 1024 * 1024;
/// Cap on nodes in one workflow.
pub const MAX_NODES: usize = 10_000;
/// Longest chain of reroutes, primitives or bypassed nodes followed when resolving a link.
const MAX_LINK_HOPS: usize = 256;

/// Node types the ComfyUI editor shows but the server never executes.
const FRONTEND_ONLY: &[&str] = &["Note", "MarkdownNote", "Reroute", "PrimitiveNode", "SetNode", "GetNode"];

/// The value of one input in an API workflow.
#[derive(Clone, PartialEq)]
pub enum InputValue {
    /// Connected to output `output` of node `node`.
    Link { node: String, output: u32 },
    /// A literal value.
    Value(Value),
}

impl std::fmt::Debug for InputValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Link { .. } => f.write_str("Link"),
            Self::Value(value) => f.debug_tuple("Value").field(&crate::json_bounds::kind(value)).finish(),
        }
    }
}

impl InputValue {
    fn from_json(v: Value) -> Self {
        if let Value::Array(items) = &v
            && let [node, output] = items.as_slice()
            && let Some(output) = output.as_u64().and_then(|o| u32::try_from(o).ok())
        {
            let node = match node {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            if let Some(node) = node {
                return Self::Link { node, output };
            }
        }
        Self::Value(v)
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Link { node, output } => Value::Array(vec![Value::String(node.clone()), Value::from(*output)]),
            Self::Value(v) => v.clone(),
        }
    }
}

/// One node of an API workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiNode {
    pub class_type: String,
    pub inputs: BTreeMap<String, InputValue>,
    /// The title the user gave the node in the editor, if any.
    pub title: Option<String>,
}

/// A workflow in the API format, ready to queue.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApiWorkflow {
    pub nodes: BTreeMap<String, ApiNode>,
}

/// Node ids are usually numbers; order them numerically, then the rest by text.
pub(crate) fn id_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

impl ApiWorkflow {
    /// Reads a workflow file in either format. The UI format needs the server's `catalog`.
    /// See [`ApiWorkflow::convert`] to also get seed control modes and conversion warnings.
    pub fn parse(bytes: &[u8], catalog: Option<&NodeCatalog>) -> Result<Self, ComfyError> {
        Self::convert(bytes, catalog).map(|c| c.workflow)
    }

    fn from_api_json(v: Value) -> Result<Self, ComfyError> {
        let Value::Object(map) = v else {
            return Err(ComfyError::InvalidWorkflow("expected a JSON object of nodes".into()));
        };
        if map.len() > MAX_NODES {
            return Err(ComfyError::InvalidWorkflow(format!("{} nodes is more than the {MAX_NODES}-node limit", map.len())));
        }
        let mut nodes = BTreeMap::new();
        for (id, node) in map {
            let Value::Object(mut node) = node else {
                return Err(ComfyError::InvalidWorkflow(format!("node {id} is not an object")));
            };
            let Some(Value::String(class_type)) = node.remove("class_type") else {
                return Err(ComfyError::InvalidWorkflow(format!("node {id} has no class_type")));
            };
            let inputs = match node.remove("inputs") {
                Some(Value::Object(inputs)) => inputs.into_iter().map(|(k, v)| (k, InputValue::from_json(v))).collect(),
                None | Some(Value::Null) => BTreeMap::new(),
                Some(_) => return Err(ComfyError::InvalidWorkflow(format!("node {id}: inputs is not an object"))),
            };
            let title = node.get("_meta").and_then(|m| m.get("title")).and_then(Value::as_str).map(str::to_owned);
            nodes.insert(id, ApiNode { class_type, inputs, title });
        }
        if nodes.is_empty() {
            return Err(ComfyError::InvalidWorkflow("the workflow has no nodes".into()));
        }
        Ok(Self { nodes })
    }

    /// The API-format JSON the server's `/prompt` takes, also what "Open in ComfyUI" exports.
    /// Validates the shared JSON depth/item/byte limits before cloning any literal value.
    pub fn to_json(&self) -> Result<Value, ComfyError> {
        self.validate_budget(&mut crate::json_bounds::Budget::default(), 0)?;
        Ok(self.to_json_validated())
    }

    pub(crate) fn validate_budget(&self, budget: &mut crate::json_bounds::Budget, depth: usize) -> Result<(), ComfyError> {
        self.validate_budget_with(budget, depth, None)
    }

    fn validate_budget_with(&self, budget: &mut crate::json_bounds::Budget, depth: usize, replacement: Option<(&str, &str, &Value)>) -> Result<(), ComfyError> {
        if self.nodes.len() > MAX_NODES {
            return Err(ComfyError::InvalidWorkflow("workflow exceeds the 10000-node limit".into()));
        }
        budget.container(self.nodes.len(), depth)?;
        for (id, node) in &self.nodes {
            budget.key(id, depth + 1)?;
            budget.container(if node.title.is_some() { 3 } else { 2 }, depth + 1)?;
            budget.key("class_type", depth + 2)?;
            budget.text(&node.class_type, depth + 2)?;
            budget.key("inputs", depth + 2)?;
            let replacement = replacement.filter(|(target, _, _)| *target == id);
            let added = replacement.filter(|(_, name, _)| !node.inputs.contains_key(*name));
            budget.container(node.inputs.len().saturating_add(usize::from(added.is_some())), depth + 2)?;
            if let Some((_, name, value)) = added {
                budget.key(name, depth + 3)?;
                budget.value(value, depth + 3)?;
            }
            for (name, input) in &node.inputs {
                budget.key(name, depth + 3)?;
                if let Some((_, _, value)) = replacement.filter(|(_, target, _)| *target == name) {
                    budget.value(value, depth + 3)?;
                    continue;
                }
                match input {
                    InputValue::Value(value) => budget.value(value, depth + 3)?,
                    InputValue::Link { node, output } => {
                        budget.container(2, depth + 3)?;
                        budget.text(node, depth + 4)?;
                        budget.value(&Value::from(*output), depth + 4)?;
                    }
                }
            }
            if let Some(title) = &node.title {
                budget.key("_meta", depth + 2)?;
                budget.container(1, depth + 2)?;
                budget.key("title", depth + 3)?;
                budget.text(title, depth + 3)?;
            }
        }
        Ok(())
    }

    pub(crate) fn to_json_validated(&self) -> Value {
        let mut out = Map::new();
        for (id, node) in &self.nodes {
            let inputs: Map<String, Value> = node.inputs.iter().map(|(k, v)| (k.clone(), v.to_json())).collect();
            let mut obj = Map::new();
            obj.insert("class_type".into(), Value::String(node.class_type.clone()));
            obj.insert("inputs".into(), Value::Object(inputs));
            if let Some(title) = &node.title {
                obj.insert("_meta".into(), serde_json::json!({ "title": title }));
            }
            out.insert(id.clone(), Value::Object(obj));
        }
        Value::Object(out)
    }

    /// Node ids in numeric order.
    pub fn ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.nodes.keys().map(String::as_str).collect();
        ids.sort_by(|a, b| id_order(a, b));
        ids
    }

    /// Sets a literal input value, checked against the server's description of the input.
    pub fn set_input(&mut self, catalog: &NodeCatalog, node: &str, input: &str, value: Value) -> Result<(), ComfyError> {
        if let Err(error) = crate::json_bounds::Budget::default().value(&value, 0) {
            crate::json_bounds::dispose(value);
            return Err(error);
        }
        // Selectors may be copied into an error even when the node does not exist.
        let mut selectors = crate::json_bounds::Budget::default();
        selectors.text(node, 0)?;
        selectors.text(input, 0)?;
        // Validate both the current tree (safe replacement/drop) and its prospective aggregate.
        self.validate_budget(&mut crate::json_bounds::Budget::default(), 0)?;
        self.validate_budget_with(&mut crate::json_bounds::Budget::default(), 0, Some((node, input, &value)))?;
        let err = |message: String| ComfyError::InvalidInput { node: node.into(), input: input.into(), message };
        let n = self.nodes.get_mut(node).ok_or_else(|| err("no such node in the workflow".into()))?;
        let info = catalog.get(&n.class_type).ok_or_else(|| ComfyError::MissingNodes(vec![n.class_type.clone()]))?;
        let spec = info.input(input).ok_or_else(|| err(format!("{} has no input named {input}", n.class_type)))?;
        spec.check(&value).map_err(err)?;
        n.inputs.insert(input.into(), InputValue::Value(value));
        Ok(())
    }

    /// Checks the workflow against what the server has: missing node classes (custom nodes not
    /// installed), values the server would reject (including model files that aren't installed),
    /// missing required inputs and broken connections. Run before queueing to show a clear error
    /// card instead of a server error.
    pub fn check(&self, catalog: &NodeCatalog) -> CheckReport {
        let mut report = CheckReport::default();
        let mut budget = crate::json_bounds::Budget::default();
        let preflight = (|| -> Result<(), ComfyError> {
            self.validate_budget(&mut budget, 0)?;
            for (id, node) in &self.nodes {
                let Some(info) = catalog.get(&node.class_type) else { continue };
                budget.container(info.inputs.len(), 0)?;
                for spec in &info.inputs {
                    // Every input can produce a report row with copied context.
                    budget.text(id, 1)?;
                    budget.text(&spec.name, 1)?;
                    budget.bytes(256)?;
                    if let Some(InputValue::Link { node: from, output }) = node.inputs.get(&spec.name) {
                        if let Some(kind) = self.nodes.get(from).and_then(|src| catalog.get(&src.class_type)).and_then(|src| src.outputs.get(*output as usize))
                        {
                            budget.text(&kind.kind, 1)?;
                        }
                        if let InputKind::Link(kind) = &spec.kind {
                            budget.text(kind, 1)?;
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = preflight {
            report.problems.push(InputProblem { node: "-".into(), input: "-".into(), message: error.to_string() });
            return report;
        }
        let mut missing = HashSet::new();
        for id in self.ids() {
            let Some(node) = self.nodes.get(id) else { continue };
            let Some(info) = catalog.get(&node.class_type) else {
                if missing.insert(node.class_type.clone()) {
                    report.missing_nodes.push(node.class_type.clone());
                }
                continue;
            };
            let mut problem = |input: &str, message: String| report.problems.push(InputProblem { node: id.into(), input: input.into(), message });
            for spec in &info.inputs {
                match node.inputs.get(&spec.name) {
                    None if spec.required => problem(&spec.name, "required input is not set".into()),
                    None => {}
                    Some(InputValue::Value(v)) => {
                        if let Err(m) = spec.check(v) {
                            problem(&spec.name, m);
                        }
                    }
                    Some(InputValue::Link { node: from, output }) => match self.nodes.get(from) {
                        None => problem(&spec.name, format!("connected to node {from}, which is not in the workflow")),
                        Some(src) => {
                            let out = catalog.get(&src.class_type).map(|i| i.outputs.get(*output as usize).map(|o| o.kind.clone()));
                            match (out, &spec.kind) {
                                (Some(None), _) => problem(&spec.name, format!("node {from} has no output {output}")),
                                (Some(Some(kind)), InputKind::Link(want)) if !types_match(&kind, want) => {
                                    problem(&spec.name, format!("expects {want} but is connected to {kind}"));
                                }
                                _ => {}
                            }
                        }
                    },
                }
            }
        }
        report
    }
}

/// ComfyUI's `*` type and comma-separated type lists match loosely.
fn types_match(have: &str, want: &str) -> bool {
    have == want || have == "*" || want == "*" || want.split(',').any(|w| w.trim() == have)
}

/// What [`ApiWorkflow::check`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// Node classes the server doesn't have.
    pub missing_nodes: Vec<String>,
    pub problems: Vec<InputProblem>,
}

impl CheckReport {
    pub fn is_ok(&self) -> bool {
        self.missing_nodes.is_empty() && self.problems.is_empty()
    }

    /// The report as an error, or `Ok` when nothing is wrong.
    pub fn into_result(self) -> Result<(), ComfyError> {
        if !self.missing_nodes.is_empty() {
            return Err(ComfyError::MissingNodes(self.missing_nodes));
        }
        match self.problems.into_iter().next() {
            Some(p) => Err(ComfyError::InvalidInput { node: p.node, input: p.input, message: p.message }),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProblem {
    pub node: String,
    pub input: String,
    pub message: String,
}

// ---- UI format -------------------------------------------------------------------------------

/// The result of reading a workflow file: the API workflow plus what the UI format carried that
/// the API format doesn't.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversion {
    pub workflow: ApiWorkflow,
    /// A bundled official-template structural match; current values remain the file's values.
    pub template: Option<&'static str>,
    /// Public inputs promoted by saved UI subgraphs, mapped onto the expanded API graph.
    pub promoted: Vec<PromotedInput>,
    /// "Control after generate" modes (`fixed`, `randomize`, …) by (node, input), so the Workflow
    /// node can keep randomising seeds per run.
    pub controls: BTreeMap<(String, String), String>,
    /// Things worth a look after import: widget values that didn't line up with the server's
    /// inputs, unknown unconnected nodes that were ignored.
    pub warnings: Vec<String>,
}

/// One logical subgraph input and the API widgets that implement it. Shared compatible scalar
/// inputs use one free server primitive, so changing their card control updates every consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedInput {
    pub instance: String,
    pub name: String,
    pub label: String,
    pub kind: String,
    pub targets: Vec<(String, String)>,
}

struct UiNode {
    id: String,
    class: String,
    mode: i64,
    title: Option<String>,
    inputs: Vec<UiInput>,
    output_types: Vec<String>,
    has_output_links: bool,
    widgets: Value,
}

struct UiInput {
    name: String,
    kind: String,
    link: Option<i64>,
}

enum Source<'a> {
    Link {
        node: &'a str,
        output: u32,
    },
    /// A value from a PrimitiveNode, with its own "control after generate" mode.
    Value(&'a Value, Option<String>),
}

/// ComfyUI writes ids as numbers; newer files may use strings.
fn id_text(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn parse_ui_node(v: &Value) -> Option<UiNode> {
    let id = id_text(v.get("id")?)?;
    let class = v.get("type")?.as_str()?.to_owned();
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let inputs = v
        .get("inputs")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|i| UiInput { name: str_of(i, "name"), kind: str_of(i, "type"), link: i.get("link").and_then(Value::as_i64) }).collect())
        .unwrap_or_default();
    let outputs = v.get("outputs").and_then(Value::as_array);
    let output_types = outputs.map(|a| a.iter().map(|o| str_of(o, "type")).collect()).unwrap_or_default();
    let has_output_links = outputs.is_some_and(|a| a.iter().any(|o| o.get("links").and_then(Value::as_array).is_some_and(|l| !l.is_empty())));
    Some(UiNode {
        id,
        class,
        mode: v.get("mode").and_then(Value::as_i64).unwrap_or(0),
        title: v.get("title").and_then(Value::as_str).map(str::to_owned),
        inputs,
        output_types,
        has_output_links,
        widgets: v.get("widgets_values").cloned().unwrap_or(Value::Null),
    })
}

/// Node modes in UI workflows.
const MODE_MUTED: i64 = 2;
const MODE_BYPASS: i64 = 4;

/// Values of the editor's "control after generate" widget, saved after seed widgets.
const SEED_CONTROLS: &[&str] = &["fixed", "increment", "decrement", "randomize"];
/// Values the editor saves for an upload button, after a loader's required widgets.
const UPLOAD_MARKERS: &[&str] = &["image", "video", "audio", "upload"];

impl ApiWorkflow {
    /// Reads a workflow file in either format, keeping what the UI format adds (seed control
    /// modes) and warnings about the conversion. The UI format needs the server's `catalog`.
    pub fn convert(bytes: &[u8], catalog: Option<&NodeCatalog>) -> Result<Conversion, ComfyError> {
        let v: Value = crate::error::parse_json("workflow", bytes, MAX_WORKFLOW_BYTES)?;
        crate::json_bounds::Budget::default().value(&v, 0)?;
        if v.get("nodes").is_some_and(Value::is_array) {
            let catalog = catalog.ok_or_else(|| {
                ComfyError::InvalidWorkflow("this is a ComfyUI editor (UI) workflow; connect to a server to convert it, or export it in API format".into())
            })?;
            let mut conversion = from_ui(&v, catalog)?;
            conversion.template = crate::templates::matching(&v)?;
            return Ok(conversion);
        }
        Ok(Conversion { workflow: Self::from_api_json(v)?, ..Conversion::default() })
    }
}

fn from_ui(v: &Value, catalog: &NodeCatalog) -> Result<Conversion, ComfyError> {
    subgraphs::expand(v, catalog)
}

fn flat_ui(v: &Value, catalog: &NodeCatalog, definitions: &BTreeMap<String, &Value>, boundary: Option<&str>) -> Result<subgraphs::Flat, ComfyError> {
    let raw_nodes = v.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    if raw_nodes.len() > MAX_NODES {
        return Err(ComfyError::InvalidWorkflow(format!("{} nodes is more than the {MAX_NODES}-node limit", raw_nodes.len())));
    }
    let group_nodes = v.get("extra").and_then(|e| e.get("groupNodes")).and_then(Value::as_object);
    let mut nodes: HashMap<String, UiNode> = HashMap::new();
    for raw in &raw_nodes {
        let node = parse_ui_node(raw).ok_or_else(|| ComfyError::InvalidWorkflow("a node has no id or type".into()))?;
        let group_name = node.class.strip_prefix("workflow>").or_else(|| node.class.strip_prefix("workflow/"));
        if group_name.is_some_and(|g| group_nodes.is_none_or(|gn| gn.contains_key(g))) {
            return Err(ComfyError::InvalidWorkflow("group nodes aren't supported yet; convert them back to nodes in ComfyUI and export again".into()));
        }
        nodes.insert(node.id.clone(), node);
    }
    if let Some(boundary) = boundary {
        if nodes.contains_key(boundary) {
            return Err(ComfyError::InvalidWorkflow("a subgraph input boundary reuses a node id".into()));
        }
        nodes.insert(
            boundary.into(),
            UiNode {
                id: boundary.into(),
                class: "NodeCraft input boundary".into(),
                mode: 0,
                title: None,
                inputs: Vec::new(),
                output_types: Vec::new(),
                has_output_links: false,
                widgets: Value::Null,
            },
        );
    }
    let placeholder = |id: &str| boundary == Some(id) || nodes.get(id).is_some_and(|n| definitions.contains_key(&n.class));
    // link id -> (origin node, origin slot); both the array and the object link forms.
    let mut links: HashMap<i64, (String, u32)> = HashMap::new();
    for l in v.get("links").and_then(Value::as_array).into_iter().flatten() {
        let (id, origin, slot) = match l {
            Value::Array(a) => (a.first(), a.get(1), a.get(2)),
            Value::Object(_) => (l.get("id"), l.get("origin_id"), l.get("origin_slot")),
            _ => continue,
        };
        if let (Some(id), Some(origin), Some(slot)) = (id.and_then(Value::as_i64), origin.and_then(id_text), slot.and_then(Value::as_u64)) {
            links.insert(id, (origin, u32::try_from(slot).unwrap_or(u32::MAX)));
        }
    }
    // Frontend "set/get" variable nodes (KJNodes): a GetNode reads the SetNode with its name.
    let first_widget = |n: &UiNode| n.widgets.as_array().and_then(|a| a.first()).and_then(Value::as_str).map(str::to_owned);
    let setters: HashMap<String, i64> =
        nodes.values().filter(|n| n.class == "SetNode").filter_map(|n| Some((first_widget(n)?, n.inputs.first()?.link?))).collect();

    let resolve = |link: i64| -> Option<Source<'_>> {
        let mut current = link;
        let mut seen = HashSet::new();
        for _ in 0..MAX_LINK_HOPS {
            if !seen.insert(current) {
                return None;
            }
            let (origin_id, slot) = links.get(&current)?;
            let origin = nodes.get(origin_id)?;
            if origin.class == "Reroute" || origin.class == "SetNode" {
                current = origin.inputs.first()?.link?;
            } else if origin.class == "GetNode" {
                current = *setters.get(&first_widget(origin)?)?;
            } else if origin.class == "PrimitiveNode" {
                let widgets = origin.widgets.as_array()?;
                let mode = widgets.get(1).and_then(Value::as_str).filter(|s| SEED_CONTROLS.contains(s)).map(str::to_owned);
                return widgets.first().map(|v| Source::Value(v, mode));
            } else if origin.mode == MODE_MUTED {
                return None;
            } else if origin.mode == MODE_BYPASS {
                // A bypassed node passes through the input in the same slot when its type
                // matches (positive→positive, negative→negative), else the first input of that type.
                let ty = origin.output_types.get(*slot as usize)?;
                let same_slot = origin.inputs.get(*slot as usize).filter(|i| &i.kind == ty && i.link.is_some());
                current = same_slot.or_else(|| origin.inputs.iter().find(|i| &i.kind == ty && i.link.is_some()))?.link?;
            } else {
                return Some(Source::Link { node: origin_id.as_str(), output: *slot });
            }
        }
        None
    };

    // Source bytes are bounded, but shared primitive values and catalog metadata can be
    // copied into many consumers. Charge the retained output before each clone.
    let mut output_budget = crate::json_bounds::Budget::default();
    let mut conversion = Conversion::default();
    let mut fallbacks = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut ids: Vec<&String> = nodes.keys().collect();
    ids.sort_by(|a, b| id_order(a, b));
    for id in ids {
        let Some(node) = nodes.get(id) else { continue };
        if node.mode == MODE_MUTED || node.mode == MODE_BYPASS || matches!(node.class.as_str(), "SetNode" | "GetNode") {
            continue;
        }
        if placeholder(id) {
            continue;
        }
        let Some(info) = catalog.get(&node.class) else {
            if FRONTEND_ONLY.contains(&node.class.as_str()) {
                continue;
            }
            // An unknown node wired into the graph is a missing custom node; an unconnected one is
            // most likely a frontend helper (group bypassers, bookmarks) and is left out.
            let connected = node.has_output_links || node.inputs.iter().any(|i| i.link.is_some());
            if connected {
                if !missing.contains(&node.class) {
                    missing.push(node.class.clone());
                }
            } else {
                conversion.warnings.push(format!("ignored node {id} ({}): not on the server and not connected", node.class));
            }
            continue;
        };
        output_budget.container(info.inputs.len().saturating_add(node.inputs.len()), 0)?;
        output_budget.text(&node.id, 1)?;
        output_budget.text(&node.class, 1)?;
        if let Some(title) = &node.title {
            output_budget.text(title, 1)?;
        }
        let mut inputs = BTreeMap::new();
        let saved = node.widgets.as_array();
        let mut positional = saved.map(|a| a.iter().peekable());
        let mut consumed = 0usize;
        let has_upload = info.inputs.iter().any(|i| matches!(i.kind, InputKind::Combo { upload: Some(_), .. }));
        let mut upload_marker_checked = false;
        let skip_upload_marker = |values: &mut std::iter::Peekable<std::slice::Iter<'_, Value>>, consumed: &mut usize| {
            if has_upload && values.peek().and_then(|v| v.as_str()).is_some_and(|s| UPLOAD_MARKERS.contains(&s)) {
                values.next();
                *consumed += 1;
            }
        };
        for spec in &info.inputs {
            if !spec.required
                && !upload_marker_checked
                && let Some(values) = &mut positional
            {
                upload_marker_checked = true;
                skip_upload_marker(values, &mut consumed);
            }
            let linked = node.inputs.iter().find(|i| i.name == spec.name).and_then(|i| i.link).and_then(resolve);
            if matches!(&spec.kind, InputKind::Link(kind) if kind=="COMFY_DYNAMICCOMBO_V3")
                && linked.is_none()
                && node.widgets.as_object().is_some_and(|widgets| widgets.contains_key(&spec.name))
            {
                return Err(ComfyError::InvalidWorkflow("dynamic widgets are not supported in UI workflow imports".into()));
            }
            let mut widget_value = None;
            let mut own_mode = None;
            if spec.is_widget() {
                match (&mut positional, &node.widgets) {
                    (Some(values), _) => {
                        widget_value = values.next();
                        if widget_value.is_some() {
                            consumed += 1;
                        }
                        let control = values.peek().and_then(|v| v.as_str()).filter(|s| SEED_CONTROLS.contains(s)).map(str::to_owned);
                        let is_int = matches!(spec.kind, InputKind::Int { .. });
                        if let Some(mode) = control
                            && (spec.control_after_generate || is_int)
                        {
                            values.next();
                            consumed += 1;
                            own_mode = Some(mode);
                        }
                    }
                    (None, Value::Object(by_name)) => widget_value = by_name.get(&spec.name),
                    _ => {}
                }
            }
            // The editor's seed mode belongs to whatever supplies the value: a primitive's own mode,
            // none for a live connection, else the node's saved one.
            if matches!(&linked, Some(Source::Link { node, .. }) if placeholder(node))
                && let Some(value) = widget_value
            {
                output_budget.value(value, 3)?;
                output_budget.text(id, 0)?;
                output_budget.text(&spec.name, 0)?;
                fallbacks
                    .insert((id.clone(), spec.name.clone()), subgraphs::Signal::Literal { value: value.clone(), mode: own_mode.clone(), inherit_mode: true });
            }
            let (value, mode) = match linked {
                Some(Source::Link { node, output }) => {
                    output_budget.text(node, 4)?;
                    (Some(InputValue::Link { node: node.into(), output }), None)
                }
                Some(Source::Value(v, mode)) => {
                    output_budget.value(v, 3)?;
                    (Some(InputValue::Value(v.clone())), mode)
                }
                None => {
                    let value = match widget_value {
                        Some(value) => {
                            output_budget.value(value, 3)?;
                            Some(InputValue::Value(value.clone()))
                        }
                        None => None,
                    };
                    (value, own_mode)
                }
            };
            if let Some(mode) = mode {
                output_budget.text(id, 1)?;
                output_budget.text(&spec.name, 1)?;
                conversion.controls.insert((id.clone(), spec.name.clone()), mode);
            }
            if let Some(value) = value {
                output_budget.key(&spec.name, 3)?;
                inputs.insert(spec.name.clone(), value);
            }
        }
        if !upload_marker_checked && let Some(values) = &mut positional {
            skip_upload_marker(values, &mut consumed);
        }
        if let Some(saved) = saved
            && saved.len() != consumed
        {
            if info.inputs.iter().any(|spec| matches!(&spec.kind,InputKind::Link(kind) if kind=="COMFY_DYNAMICCOMBO_V3")) {
                return Err(ComfyError::InvalidWorkflow("dynamic widgets are not supported in UI workflow imports".into()));
            }
            conversion.warnings.push(format!(
                "node {id} ({}): {} saved widget values but {consumed} matched the server's inputs; check its settings",
                node.class,
                saved.len()
            ));
        }
        // Dynamic inputs some custom nodes add at runtime aren't in /object_info; keep their links.
        for input in &node.inputs {
            if !inputs.contains_key(&input.name)
                && info.input(&input.name).is_none()
                && let Some(Source::Link { node, output }) = input.link.and_then(resolve)
            {
                output_budget.key(&input.name, 3)?;
                output_budget.text(node, 4)?;
                inputs.insert(input.name.clone(), InputValue::Link { node: node.into(), output });
            }
        }
        conversion.workflow.nodes.insert(node.id.clone(), ApiNode { class_type: node.class.clone(), inputs, title: node.title.clone() });
    }
    if !missing.is_empty() {
        return Err(ComfyError::MissingNodes(missing));
    }
    if conversion.workflow.nodes.is_empty() && !nodes.values().any(|node| definitions.contains_key(&node.class)) {
        return Err(ComfyError::InvalidWorkflow("the workflow has no nodes the server can run".into()));
    }
    conversion.workflow.validate_budget(&mut crate::json_bounds::Budget::default(), 0)?;
    let mut bindings = BTreeMap::new();
    for (id, node) in &nodes {
        let Some(definition) = definitions.get(&node.class) else { continue };
        let mut values = Vec::new();
        for input in definition.get("inputs").and_then(Value::as_array).into_iter().flatten() {
            let name = input.get("name").and_then(Value::as_str).unwrap_or_default();
            let source = node.inputs.iter().find(|i| i.name == name).and_then(|i| i.link).and_then(resolve);
            values.push(source.map(|source| subgraphs::copy_source(source, &mut output_budget, boundary)).transpose()?);
        }
        bindings.insert(id.clone(), values);
    }
    let mut outputs = Vec::new();
    for output in v.get("outputs").and_then(Value::as_array).into_iter().flatten() {
        let source = output.get("linkIds").and_then(Value::as_array).and_then(|ids| ids.first()).and_then(Value::as_i64).and_then(resolve);
        outputs.push(source.map(|source| subgraphs::copy_source(source, &mut output_budget, boundary)).transpose()?);
    }
    Ok(subgraphs::Flat { conversion, bindings, outputs, fallbacks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use serde_json::json;

    fn catalog() -> NodeCatalog {
        NodeCatalog::parse(fixtures::OBJECT_INFO.as_bytes()).unwrap().0
    }

    #[test]
    fn api_format_round_trips() {
        let wf = ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        assert_eq!(wf.ids(), ["3", "4", "5", "6", "7", "8", "9", "10"]);
        let ks = &wf.nodes["3"];
        assert_eq!(ks.class_type, "KSampler");
        assert_eq!(ks.inputs["model"], InputValue::Link { node: "10".into(), output: 0 });
        assert_eq!(ks.inputs["steps"], InputValue::Value(json!(20)));
        assert_eq!(wf.nodes["6"].title.as_deref(), Some("Positive"));
        let again = ApiWorkflow::parse(&serde_json::to_vec(&wf.to_json().unwrap()).unwrap(), None).unwrap();
        assert_eq!(again, wf);
        assert!(wf.check(&catalog()).is_ok(), "{:?}", wf.check(&catalog()));
    }

    #[test]
    fn ui_format_converts_like_the_api_export() {
        let c = catalog();
        let ui = ApiWorkflow::parse(fixtures::UI_WORKFLOW.as_bytes(), Some(&c)).unwrap();
        let api = ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        for id in api.ids() {
            assert_eq!(ui.nodes.get(id).map(|n| &n.inputs), Some(&api.nodes[id].inputs), "node {id}");
            assert_eq!(ui.nodes[id].class_type, api.nodes[id].class_type);
        }
        assert_eq!(ui.ids(), api.ids(), "notes, reroutes, primitives, muted and bypassed nodes are dropped");
        let conv = ApiWorkflow::convert(fixtures::UI_WORKFLOW.as_bytes(), Some(&c)).unwrap();
        assert_eq!(conv.controls.get(&("3".to_string(), "seed".to_string())).map(String::as_str), Some("fixed"), "the primitive's mode, not the stale one");
        assert!(ui.check(&c).is_ok(), "{:?}", ui.check(&c));
    }

    #[test]
    fn ui_format_needs_a_server() {
        let err = ApiWorkflow::parse(fixtures::UI_WORKFLOW.as_bytes(), None).unwrap_err();
        assert!(matches!(err, ComfyError::InvalidWorkflow(_)));
    }

    #[test]
    fn missing_custom_nodes_are_named() {
        let ui = fixtures::UI_WORKFLOW.replace("\"type\": \"VAEDecode\"", "\"type\": \"FancyCustomDecode\"");
        let err = ApiWorkflow::parse(ui.as_bytes(), Some(&catalog())).unwrap_err();
        assert_eq!(err, ComfyError::MissingNodes(vec!["FancyCustomDecode".into()]));
        assert!(err.to_string().contains("install the custom nodes"));

        let mut api = ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        api.nodes.get_mut("8").unwrap().class_type = "FancyCustomDecode".into();
        let report = api.check(&catalog());
        assert_eq!(report.missing_nodes, ["FancyCustomDecode"]);
        assert!(matches!(report.into_result(), Err(ComfyError::MissingNodes(_))));
    }

    #[test]
    fn check_finds_missing_models_and_bad_links() {
        let c = catalog();
        let mut wf = ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        wf.nodes.get_mut("4").unwrap().inputs.insert("ckpt_name".into(), InputValue::Value(json!("not_installed.safetensors")));
        wf.nodes.get_mut("3").unwrap().inputs.insert("latent_image".into(), InputValue::Link { node: "6".into(), output: 0 });
        wf.nodes.get_mut("3").unwrap().inputs.insert("negative".into(), InputValue::Link { node: "99".into(), output: 0 });
        wf.nodes.get_mut("8").unwrap().inputs.insert("samples".into(), InputValue::Link { node: "3".into(), output: 5 });
        wf.nodes.get_mut("3").unwrap().inputs.remove("steps");
        let report = wf.check(&c);
        let messages: Vec<String> = report.problems.iter().map(|p| format!("{} {} {}", p.node, p.input, p.message)).collect();
        assert_eq!(report.problems.len(), 5, "{messages:#?}");
        assert!(messages.iter().any(|m| m.starts_with("4 ckpt_name") && m.contains("not available")));
        assert!(messages.iter().any(|m| m.starts_with("3 latent_image") && m.contains("expects LATENT but is connected to CONDITIONING")));
        assert!(messages.iter().any(|m| m.starts_with("3 negative") && m.contains("not in the workflow")));
        assert!(messages.iter().any(|m| m.starts_with("8 samples") && m.contains("no output 5")));
        assert!(messages.iter().any(|m| m.starts_with("3 steps") && m.contains("required")));
    }

    #[test]
    fn set_input_validates() {
        let c = catalog();
        let mut wf = ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        wf.set_input(&c, "6", "text", json!("a red fox")).unwrap();
        assert_eq!(wf.nodes["6"].inputs["text"], InputValue::Value(json!("a red fox")));
        assert!(wf.set_input(&c, "3", "steps", json!(-1)).is_err());
        assert!(wf.set_input(&c, "3", "nope", json!(1)).is_err());
        assert!(wf.set_input(&c, "42", "steps", json!(1)).is_err());
        assert!(wf.set_input(&c, "3", "model", json!("x")).is_err(), "links can't be set to values");
    }

    #[test]
    fn hostile_workflows_are_errors() {
        let c = catalog();
        for bad in [
            "",
            "[]",
            "{}",
            r#"{"1":5}"#,
            r#"{"1":{"inputs":{}}}"#,
            r#"{"1":{"class_type":"X","inputs":7}}"#,
            r#"{"nodes":[{"id":1}],"links":[]}"#,
            r#"{"nodes":[],"links":[]}"#,
            r#"{"nodes":[{"id":1,"type":"Note"}],"links":[]}"#,
        ] {
            assert!(ApiWorkflow::parse(bad.as_bytes(), Some(&c)).is_err(), "{bad}");
        }
        // A reroute loop terminates.
        let looped = r#"{"nodes":[
            {"id":1,"type":"Reroute","inputs":[{"name":"","type":"*","link":2}],"outputs":[{"type":"*"}]},
            {"id":2,"type":"Reroute","inputs":[{"name":"","type":"*","link":1}],"outputs":[{"type":"*"}]},
            {"id":3,"type":"VAEDecode","inputs":[{"name":"samples","type":"LATENT","link":1},{"name":"vae","type":"VAE","link":null}],"widgets_values":[]}
        ],"links":[[1,2,0,3,0,"*"],[2,1,0,2,0,"*"]]}"#;
        let wf = ApiWorkflow::parse(looped.as_bytes(), Some(&c)).unwrap();
        assert!(wf.nodes["3"].inputs.is_empty());
        let subgraph = r#"{"nodes":[{"id":1,"type":"5d1f-uuid"}],"links":[],"definitions":{"subgraphs":[{"id":"5d1f-uuid"}]}}"#;
        assert!(ApiWorkflow::parse(subgraph.as_bytes(), Some(&c)).unwrap_err().to_string().contains("subgraph"));
    }

    /// Replaces random leaves of the fixtures with hostile values; conversion, checking and
    /// discovery must never panic.
    #[test]
    fn mutated_workflows_never_panic() {
        let c = catalog();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let hostile = [json!(null), json!(-1), json!(u64::MAX), json!(1e308), json!("x"), json!([]), json!({}), json!([1, -5]), json!(["3", 99999])];
        fn leaves(v: &Value) -> usize {
            match v {
                Value::Array(a) => a.iter().map(leaves).sum(),
                Value::Object(o) => o.values().map(leaves).sum(),
                _ => 1,
            }
        }
        for source in [fixtures::UI_WORKFLOW, fixtures::API_WORKFLOW] {
            let original: Value = serde_json::from_str(source).unwrap();
            for _ in 0..400 {
                let mut v = original.clone();
                for _ in 0..1 + next() % 4 {
                    let target = (next() % leaves(&v) as u64) as usize;
                    let mut i = 0;
                    replace_nth(&mut v, target, &mut i, &hostile[(next() % hostile.len() as u64) as usize]);
                }
                if let Ok(wf) = ApiWorkflow::parse(&serde_json::to_vec(&v).unwrap(), Some(&c)) {
                    let _ = wf.check(&c);
                    let _ = crate::discover(&wf, &c);
                }
            }
        }
    }

    fn replace_nth(v: &mut Value, target: usize, i: &mut usize, with: &Value) -> bool {
        match v {
            Value::Array(a) => a.iter_mut().any(|x| replace_nth(x, target, i, with)),
            Value::Object(o) => o.values_mut().any(|x| replace_nth(x, target, i, with)),
            leaf => {
                if *i == target {
                    *leaf = with.clone();
                    return true;
                }
                *i += 1;
                false
            }
        }
    }

    /// A UI workflow around the fixture catalog: `nodes` and `links` JSON arrays.
    fn ui(nodes: &str, links: &str) -> String {
        format!(r#"{{"nodes":[{nodes}],"links":[{links}]}}"#)
    }

    const CKPT: &str = r#"{"id":1,"type":"CheckpointLoaderSimple","outputs":[{"type":"MODEL","links":[1]},{"type":"CLIP","links":[2,3]},{"type":"VAE","links":[]}],"widgets_values":["sd_xl_base_1.0.safetensors"]}"#;
    const POS: &str = r#"{"id":2,"type":"CLIPTextEncode","inputs":[{"name":"clip","type":"CLIP","link":2}],"outputs":[{"type":"CONDITIONING","links":[4]}],"widgets_values":["pos"]}"#;
    const NEG: &str = r#"{"id":3,"type":"CLIPTextEncode","inputs":[{"name":"clip","type":"CLIP","link":3}],"outputs":[{"type":"CONDITIONING","links":[5]}],"widgets_values":["neg"]}"#;

    #[test]
    fn bypassed_node_passes_each_slot_through() {
        // A bypassed two-in/two-out conditioning node (like an advanced ControlNet apply) must
        // keep positive on positive and negative on negative.
        let bypassed = r#"{"id":4,"type":"SomeControlNetApply","mode":4,
            "inputs":[{"name":"positive","type":"CONDITIONING","link":4},{"name":"negative","type":"CONDITIONING","link":5}],
            "outputs":[{"type":"CONDITIONING","links":[6]},{"type":"CONDITIONING","links":[7]}]}"#;
        let sampler = r#"{"id":5,"type":"KSampler","inputs":[{"name":"model","type":"MODEL","link":1},{"name":"positive","type":"CONDITIONING","link":6},{"name":"negative","type":"CONDITIONING","link":7}],
            "outputs":[{"type":"LATENT","links":[]}],"widgets_values":[1,"fixed",20,7.0,"euler","normal",1.0]}"#;
        let links = "[1,1,0,5,0,\"MODEL\"],[2,1,1,2,0,\"CLIP\"],[3,1,1,3,0,\"CLIP\"],[4,2,0,4,0,\"CONDITIONING\"],[5,3,0,4,1,\"CONDITIONING\"],[6,4,0,5,1,\"CONDITIONING\"],[7,4,1,5,2,\"CONDITIONING\"]";
        let c = catalog();
        let conv = ApiWorkflow::convert(ui(&[CKPT, POS, NEG, bypassed, sampler].join(","), links).as_bytes(), Some(&c)).unwrap();
        let ks = &conv.workflow.nodes["5"].inputs;
        assert_eq!(ks["positive"], InputValue::Link { node: "2".into(), output: 0 });
        assert_eq!(ks["negative"], InputValue::Link { node: "3".into(), output: 0 });
        assert_eq!(conv.controls.get(&("5".to_string(), "seed".to_string())).map(String::as_str), Some("fixed"));
        assert!(conv.warnings.is_empty(), "{:?}", conv.warnings);
    }

    #[test]
    fn object_links_set_get_variables_and_linked_widgets() {
        let set = r#"{"id":10,"type":"SetNode","inputs":[{"name":"CONDITIONING","type":"CONDITIONING","link":11}],"outputs":[],"widgets_values":["cond"]}"#;
        let get = r#"{"id":11,"type":"GetNode","outputs":[{"type":"CONDITIONING","links":[12]}],"widgets_values":["cond"]}"#;
        let latent = r#"{"id":12,"type":"EmptyLatentImage","outputs":[{"type":"LATENT","links":[]}],"widgets_values":[512,512,1]}"#;
        // `steps` converted to an input and fed from a real node's INT output (batch_size here, for the test).
        let steps_src = r#"{"id":13,"type":"SomeIntNode","inputs":[],"outputs":[{"type":"INT","links":[14]}]}"#;
        let sampler = r#"{"id":5,"type":"KSampler","inputs":[{"name":"positive","type":"CONDITIONING","link":12},{"name":"steps","type":"INT","link":14,"widget":{"name":"steps"}}],
            "outputs":[{"type":"LATENT","links":[]}],"widgets_values":[1,"randomize",20,7.0,"euler","normal",1.0]}"#;
        let links = r#"{"id":11,"origin_id":2,"origin_slot":0,"target_id":10,"target_slot":0,"type":"CONDITIONING"},
                       {"id":12,"origin_id":11,"origin_slot":0,"target_id":5,"target_slot":1,"type":"CONDITIONING"},
                       {"id":14,"origin_id":13,"origin_slot":0,"target_id":5,"target_slot":2,"type":"INT"},
                       {"id":2,"origin_id":1,"origin_slot":1,"target_id":2,"target_slot":0,"type":"CLIP"}"#;
        let c = catalog();
        let err = ApiWorkflow::convert(ui(&[CKPT, POS, set, get, latent, steps_src, sampler].join(","), links).as_bytes(), Some(&c)).unwrap_err();
        assert_eq!(err, ComfyError::MissingNodes(vec!["SomeIntNode".into()]), "a connected unknown node is a missing custom node");
        let mut catalog_with_int = c.clone();
        let (extra, _) = NodeCatalog::parse(br#"{"SomeIntNode":{"input":{},"output":["INT"]}}"#).unwrap();
        catalog_with_int.extend(extra);
        let conv = ApiWorkflow::convert(ui(&[CKPT, POS, set, get, latent, steps_src, sampler].join(","), links).as_bytes(), Some(&catalog_with_int)).unwrap();
        let ks = &conv.workflow.nodes["5"].inputs;
        assert_eq!(ks["positive"], InputValue::Link { node: "2".into(), output: 0 }, "GetNode resolves through SetNode");
        assert_eq!(ks["steps"], InputValue::Link { node: "13".into(), output: 0 });
        assert_eq!(ks["cfg"], InputValue::Value(json!(7.0)), "values after the converted widget stay aligned");
        assert!(!conv.workflow.nodes.contains_key("10") && !conv.workflow.nodes.contains_key("11"));
    }

    #[test]
    fn upload_marker_after_required_widgets_and_mismatch_warnings() {
        let c = {
            let (c, _) = NodeCatalog::parse(
                br#"{"LoadThing":{"input":{"required":{"image":[["a.png"],{"image_upload":true}]},"optional":{"strength":["FLOAT",{"default":1.0}]}},"output":["IMAGE"]}}"#,
            )
            .unwrap();
            c
        };
        let node = r#"{"id":1,"type":"LoadThing","outputs":[{"type":"IMAGE","links":[]}],"widgets_values":["cat.png","image",0.5]}"#;
        let conv = ApiWorkflow::convert(ui(node, "").as_bytes(), Some(&c)).unwrap();
        assert_eq!(conv.workflow.nodes["1"].inputs["strength"], InputValue::Value(json!(0.5)), "the upload marker is skipped");
        assert!(conv.warnings.is_empty(), "{:?}", conv.warnings);

        let extra = r#"{"id":1,"type":"LoadThing","outputs":[{"type":"IMAGE","links":[]}],"widgets_values":["cat.png","image",0.5,"surprise"]}"#;
        let conv = ApiWorkflow::convert(ui(extra, "").as_bytes(), Some(&c)).unwrap();
        assert_eq!(conv.warnings.len(), 1);
        assert!(conv.warnings[0].contains("4 saved widget values but 3"), "{:?}", conv.warnings);
    }

    #[test]
    fn group_nodes_and_unconnected_helpers() {
        let c = catalog();
        let group = r#"{"nodes":[{"id":1,"type":"workflow>MyGroup"}],"links":[],"extra":{"groupNodes":{"MyGroup":{}}}}"#;
        assert!(ApiWorkflow::parse(group.as_bytes(), Some(&c)).unwrap_err().to_string().contains("group nodes"));
        let helper = r#"{"id":9,"type":"Fast Groups Bypasser (rgthree)","inputs":[],"outputs":[]}"#;
        let conv = ApiWorkflow::convert(ui(&[CKPT, helper].join(","), "").as_bytes(), Some(&c)).unwrap();
        assert_eq!(conv.warnings.len(), 1);
        assert!(conv.warnings[0].contains("not on the server and not connected"));
    }

    #[test]
    fn widget_values_by_name_and_string_ids() {
        let c = catalog();
        let ui = r#"{"nodes":[{"id":"a","type":"EmptyLatentImage","widgets_values":{"width":640,"height":480,"batch_size":2}}],"links":[]}"#;
        let wf = ApiWorkflow::parse(ui.as_bytes(), Some(&c)).unwrap();
        assert_eq!(wf.nodes["a"].inputs["width"], InputValue::Value(json!(640)));
        assert_eq!(wf.nodes["a"].inputs["batch_size"], InputValue::Value(json!(2)));
    }
}
