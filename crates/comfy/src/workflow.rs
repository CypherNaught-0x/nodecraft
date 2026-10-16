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

/// Cap on a workflow file.
pub const MAX_WORKFLOW_BYTES: usize = 32 * 1024 * 1024;
/// Cap on nodes in one workflow.
pub const MAX_NODES: usize = 10_000;
/// Longest chain of reroutes, primitives or bypassed nodes followed when resolving a link.
const MAX_LINK_HOPS: usize = 256;

/// Node types the ComfyUI editor shows but the server never executes.
const FRONTEND_ONLY: &[&str] = &["Note", "MarkdownNote", "Reroute", "PrimitiveNode"];

/// The value of one input in an API workflow.
#[derive(Debug, Clone, PartialEq)]
pub enum InputValue {
    /// Connected to output `output` of node `node`.
    Link { node: String, output: u32 },
    /// A literal value.
    Value(Value),
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
    pub fn parse(bytes: &[u8], catalog: Option<&NodeCatalog>) -> Result<Self, ComfyError> {
        let v: Value = crate::error::parse_json("workflow", bytes, MAX_WORKFLOW_BYTES)?;
        if v.get("nodes").is_some_and(Value::is_array) {
            let catalog = catalog.ok_or_else(|| {
                ComfyError::InvalidWorkflow("this is a ComfyUI editor (UI) workflow; connect to a server to convert it, or export it in API format".into())
            })?;
            return from_ui(&v, catalog);
        }
        Self::from_api_json(v)
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
    pub fn to_json(&self) -> Value {
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

struct UiNode {
    id: String,
    class: String,
    mode: i64,
    title: Option<String>,
    inputs: Vec<UiInput>,
    output_types: Vec<String>,
    widgets: Value,
}

struct UiInput {
    name: String,
    kind: String,
    link: Option<i64>,
}

enum Source {
    Link { node: String, output: u32 },
    Value(Value),
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
    let output_types = v.get("outputs").and_then(Value::as_array).map(|a| a.iter().map(|o| str_of(o, "type")).collect()).unwrap_or_default();
    Some(UiNode {
        id,
        class,
        mode: v.get("mode").and_then(Value::as_i64).unwrap_or(0),
        title: v.get("title").and_then(Value::as_str).map(str::to_owned),
        inputs,
        output_types,
        widgets: v.get("widgets_values").cloned().unwrap_or(Value::Null),
    })
}

/// Node modes in UI workflows.
const MODE_MUTED: i64 = 2;
const MODE_BYPASS: i64 = 4;

/// Values of the editor's "control after generate" widget, saved after seed widgets.
const SEED_CONTROLS: &[&str] = &["fixed", "increment", "decrement", "randomize"];

fn from_ui(v: &Value, catalog: &NodeCatalog) -> Result<ApiWorkflow, ComfyError> {
    let raw_nodes = v.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    if raw_nodes.len() > MAX_NODES {
        return Err(ComfyError::InvalidWorkflow(format!("{} nodes is more than the {MAX_NODES}-node limit", raw_nodes.len())));
    }
    let subgraphs: HashSet<String> = v
        .get("definitions")
        .and_then(|d| d.get("subgraphs"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.get("id").and_then(Value::as_str)).map(str::to_owned).collect())
        .unwrap_or_default();
    let mut nodes: HashMap<String, UiNode> = HashMap::new();
    for raw in &raw_nodes {
        let node = parse_ui_node(raw).ok_or_else(|| ComfyError::InvalidWorkflow("a node has no id or type".into()))?;
        if subgraphs.contains(&node.class) {
            return Err(ComfyError::InvalidWorkflow("subgraphs aren't supported yet; unpack them in ComfyUI and export again".into()));
        }
        nodes.insert(node.id.clone(), node);
    }
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

    let resolve = |link: i64| -> Option<Source> {
        let mut current = link;
        let mut seen = HashSet::new();
        for _ in 0..MAX_LINK_HOPS {
            if !seen.insert(current) {
                return None;
            }
            let (origin_id, slot) = links.get(&current)?;
            let origin = nodes.get(origin_id)?;
            if origin.class == "Reroute" {
                current = origin.inputs.first()?.link?;
            } else if origin.class == "PrimitiveNode" {
                return origin.widgets.as_array()?.first().cloned().map(Source::Value);
            } else if origin.mode == MODE_MUTED {
                return None;
            } else if origin.mode == MODE_BYPASS {
                // A bypassed node passes through its first input of the same type.
                let ty = origin.output_types.get(*slot as usize)?;
                current = origin.inputs.iter().find(|i| &i.kind == ty && i.link.is_some())?.link?;
            } else {
                return Some(Source::Link { node: origin_id.clone(), output: *slot });
            }
        }
        None
    };

    let mut out = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut ids: Vec<&String> = nodes.keys().collect();
    ids.sort_by(|a, b| id_order(a, b));
    for id in ids {
        let Some(node) = nodes.get(id) else { continue };
        if node.mode == MODE_MUTED || node.mode == MODE_BYPASS {
            continue;
        }
        let Some(info) = catalog.get(&node.class) else {
            if !FRONTEND_ONLY.contains(&node.class.as_str()) && !missing.contains(&node.class) {
                missing.push(node.class.clone());
            }
            continue;
        };
        let mut inputs = BTreeMap::new();
        let mut positional = node.widgets.as_array().map(|a| a.iter().peekable());
        for spec in &info.inputs {
            let linked = node.inputs.iter().find(|i| i.name == spec.name).and_then(|i| i.link).and_then(resolve);
            let mut widget_value = None;
            if spec.is_widget() {
                match (&mut positional, &node.widgets) {
                    (Some(values), _) => {
                        widget_value = values.next().cloned();
                        let control = values.peek().and_then(|v| v.as_str()).is_some_and(|s| SEED_CONTROLS.contains(&s));
                        let is_int = matches!(spec.kind, InputKind::Int { .. });
                        if (spec.control_after_generate || is_int) && control {
                            values.next();
                        } else if matches!(spec.kind, InputKind::Combo { image_upload: true, .. }) && values.peek().and_then(|v| v.as_str()) == Some("image") {
                            values.next(); // the upload button's value
                        }
                    }
                    (None, Value::Object(by_name)) => widget_value = by_name.get(&spec.name).cloned(),
                    _ => {}
                }
            }
            let value = match linked {
                Some(Source::Link { node, output }) => Some(InputValue::Link { node, output }),
                Some(Source::Value(v)) => Some(InputValue::Value(v)),
                None => widget_value.map(InputValue::Value),
            };
            if let Some(value) = value {
                inputs.insert(spec.name.clone(), value);
            }
        }
        // Dynamic inputs some custom nodes add at runtime aren't in /object_info; keep their links.
        for input in &node.inputs {
            if !inputs.contains_key(&input.name)
                && info.input(&input.name).is_none()
                && let Some(Source::Link { node, output }) = input.link.and_then(resolve)
            {
                inputs.insert(input.name.clone(), InputValue::Link { node, output });
            }
        }
        out.insert(node.id.clone(), ApiNode { class_type: node.class.clone(), inputs, title: node.title.clone() });
    }
    if !missing.is_empty() {
        return Err(ComfyError::MissingNodes(missing));
    }
    if out.is_empty() {
        return Err(ComfyError::InvalidWorkflow("the workflow has no nodes the server can run".into()));
    }
    Ok(ApiWorkflow { nodes: out })
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
        let again = ApiWorkflow::parse(&serde_json::to_vec(&wf.to_json()).unwrap(), None).unwrap();
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
        assert!(ApiWorkflow::parse(subgraph.as_bytes(), Some(&c)).unwrap_err().to_string().contains("subgraphs"));
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

    #[test]
    fn widget_values_by_name_and_string_ids() {
        let c = catalog();
        let ui = r#"{"nodes":[{"id":"a","type":"EmptyLatentImage","widgets_values":{"width":640,"height":480,"batch_size":2}}],"links":[]}"#;
        let wf = ApiWorkflow::parse(ui.as_bytes(), Some(&c)).unwrap();
        assert_eq!(wf.nodes["a"].inputs["width"], InputValue::Value(json!(640)));
        assert_eq!(wf.nodes["a"].inputs["batch_size"], InputValue::Value(json!(2)));
    }
}
