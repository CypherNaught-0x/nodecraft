//! Limits for programmatic JSON, before any recursive serde_json operation.

use crate::ComfyError;
use serde_json::Value;

/// Maximum nesting of in-memory JSON, including the outgoing request envelope.
pub const MAX_JSON_DEPTH: usize = 64;
/// Maximum JSON values and object keys in one conversion.
pub const MAX_JSON_ITEMS: usize = 2_000_000;
/// Maximum encoded JSON bytes, including keys and envelope metadata.
pub const MAX_JSON_BYTES: usize = 32 * 1024 * 1024;

pub(crate) struct Budget {
    items: usize,
    bytes: usize,
    max_items: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self { items: 0, bytes: 0, max_items: MAX_JSON_ITEMS }
    }
}

fn limit(message: &'static str) -> ComfyError {
    ComfyError::InvalidInput { node: "-".into(), input: "-".into(), message: message.into() }
}

impl Budget {
    pub(crate) fn bytes(&mut self, count: usize) -> Result<(), ComfyError> {
        self.bytes = self.bytes.saturating_add(count);
        if self.bytes > MAX_JSON_BYTES {
            return Err(limit("encoded JSON exceeds the 32 MiB limit"));
        }
        Ok(())
    }

    fn item(&mut self, depth: usize) -> Result<(), ComfyError> {
        if depth > MAX_JSON_DEPTH {
            return Err(limit("JSON nesting exceeds the 64-level limit"));
        }
        self.items = self.items.saturating_add(1);
        if self.items > self.max_items {
            return Err(limit("JSON values and keys exceed the 2000000-item limit"));
        }
        Ok(())
    }

    pub(crate) fn container(&mut self, len: usize, depth: usize) -> Result<(), ComfyError> {
        self.item(depth)?;
        // Reject wide containers before allocating traversal state or iterating their children.
        if len > self.max_items.saturating_sub(self.items) {
            return Err(limit("JSON values and keys exceed the 2000000-item limit"));
        }
        self.bytes(2usize.saturating_add(len.saturating_sub(1)))
    }

    pub(crate) fn text(&mut self, text: &str, depth: usize) -> Result<(), ComfyError> {
        self.item(depth)?;
        self.bytes(text.len().saturating_add(2))?;
        // Account for JSON escaping, without allocating an encoded copy.
        for byte in text.bytes() {
            self.bytes(match byte {
                b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 1,
                0..=31 => 5,
                _ => 0,
            })?;
        }
        Ok(())
    }

    pub(crate) fn key(&mut self, key: &str, depth: usize) -> Result<(), ComfyError> {
        self.text(key, depth)?;
        self.bytes(1)
    }

    pub(crate) fn value(&mut self, value: &Value, depth: usize) -> Result<(), ComfyError> {
        enum Frame<'a> {
            Array(std::slice::Iter<'a, Value>, usize),
            Object(serde_json::map::Iter<'a>, usize),
        }
        let mut stack = Vec::new();
        let mut next = Some((value, depth));
        loop {
            if let Some((value, depth)) = next.take() {
                match value {
                    Value::String(text) => self.text(text, depth)?,
                    Value::Array(values) => {
                        self.container(values.len(), depth)?;
                        stack.push(Frame::Array(values.iter(), depth));
                    }
                    Value::Object(values) => {
                        self.container(values.len(), depth)?;
                        stack.push(Frame::Object(values.iter(), depth));
                    }
                    Value::Null => {
                        self.item(depth)?;
                        self.bytes(4)?;
                    }
                    Value::Bool(value) => {
                        self.item(depth)?;
                        self.bytes(if *value { 4 } else { 5 })?;
                    }
                    Value::Number(value) => {
                        self.item(depth)?;
                        self.bytes(value.to_string().len())?;
                    }
                }
            }
            while let Some(frame) = stack.last_mut() {
                next = match frame {
                    Frame::Array(iter, depth) => iter.next().map(|value| (value, depth.saturating_add(1))),
                    Frame::Object(iter, depth) => match iter.next() {
                        Some((key, value)) => {
                            self.key(key, depth.saturating_add(1))?;
                            Some((value, depth.saturating_add(1)))
                        }
                        None => None,
                    },
                };
                if next.is_some() {
                    break;
                }
                stack.pop();
            }
            if next.is_none() {
                return Ok(());
            }
        }
    }
}

/// Dispose of rejected owned input without recursively dropping an arbitrarily deep Value.
pub(crate) fn dispose(value: Value) {
    enum Frame {
        Array(std::vec::IntoIter<Value>),
        Object(serde_json::map::IntoIter),
    }
    let mut stack = Vec::new();
    let mut next = Some(value);
    loop {
        match next.take() {
            Some(Value::Array(values)) => stack.push(Frame::Array(values.into_iter())),
            Some(Value::Object(values)) => stack.push(Frame::Object(values.into_iter())),
            _ => {}
        }
        while let Some(frame) = stack.last_mut() {
            next = match frame {
                Frame::Array(iter) => iter.next(),
                Frame::Object(iter) => iter.next().map(|(_, value)| value),
            };
            if next.is_some() {
                break;
            }
            stack.pop();
        }
        if next.is_none() {
            return;
        }
    }
}

pub(crate) fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "text",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiWorkflow, InputValue, NodeCatalog, PromptRequest, Server, discover};

    fn nested(depth: usize) -> Value {
        let mut value = Value::String("fabricated-json-secret".into());
        for _ in 0..depth {
            value = Value::Array(vec![value]);
        }
        value
    }

    fn workflow() -> ApiWorkflow {
        ApiWorkflow::parse(crate::fixtures::API_WORKFLOW.as_bytes(), None).unwrap()
    }
    fn catalog() -> NodeCatalog {
        NodeCatalog::parse(crate::fixtures::OBJECT_INFO.as_bytes()).unwrap().0
    }

    #[test]
    fn invalid_input_errors_never_echo_arbitrary_values() {
        let catalog = catalog();
        for (class, input, value) in [
            ("KSampler", "steps", serde_json::json!({"credential":"fabricated-json-secret"})),
            ("KSampler", "sampler_name", Value::String("fabricated-json-secret".into())),
            ("CLIPTextEncode", "text", serde_json::json!(["fabricated-json-secret"])),
        ] {
            let error = catalog.get(class).unwrap().input(input).unwrap().check(&value).unwrap_err();
            assert!(!error.contains("fabricated-json-secret"));
            assert!(error.len() < 200);
        }
    }

    #[test]
    fn deep_programmatic_values_are_rejected_before_cloning_discovery_or_debug() {
        let mut workflow = workflow();
        let input = workflow.nodes.get_mut("6").unwrap().inputs.get_mut("text").unwrap();
        *input = InputValue::Value(nested(50_000));
        let catalog = catalog();
        assert!(!format!("{input:?}").contains("fabricated-json-secret"));
        assert!(workflow.to_json().is_err());
        assert!(discover(&workflow, &catalog).is_err());
        assert!(workflow.check(&catalog).into_result().is_err());
        let input = workflow.nodes.get_mut("6").unwrap().inputs.remove("text").unwrap();
        let InputValue::Value(value) = input else { panic!("literal") };
        dispose(value);
    }

    #[test]
    fn prompt_rejects_extra_data_and_envelope_metadata_before_copying() {
        let mut prompt = PromptRequest::new(workflow(), "client");
        prompt.extra_data.insert("credential".into(), nested(50_000));
        let error = Server::new("http://localhost:8188").unwrap().prompt(&prompt).unwrap_err();
        assert!(!error.to_string().contains("fabricated-json-secret"));
        assert!(error.to_string().len() < 200);
        dispose(prompt.extra_data.remove("credential").unwrap());
        prompt.client_id = "x".repeat(MAX_JSON_BYTES);
        assert!(prompt.to_json().is_err());
        prompt.client_id.clear();
        prompt.extra_data.insert("a".into(), Value::String("x".repeat(MAX_JSON_BYTES / 2)));
        prompt.extra_data.insert("b".into(), Value::String("y".repeat(MAX_JSON_BYTES / 2)));
        assert!(prompt.to_json().is_err(), "one aggregate budget across extra_data entries");
    }

    #[test]
    fn owned_deep_rejections_are_disposed_iteratively_and_mutations_are_atomic() {
        let mut workflow = workflow();
        let before = workflow.clone();
        let catalog = catalog();
        for (node, input) in [("6", "text"), ("missing", "text"), ("6", "missing")] {
            assert!(workflow.set_input(&catalog, node, input, nested(50_000)).is_err());
            assert_eq!(workflow, before);
        }
        // Each input fits independently, but the prospective workflow must fit as a whole.
        let half = MAX_JSON_BYTES / 2;
        workflow.set_input(&catalog, "6", "text", Value::String("a".repeat(half))).unwrap();
        assert!(workflow.set_input(&catalog, "7", "text", Value::String("b".repeat(half))).is_err());
        assert_eq!(workflow.nodes["7"], before.nodes["7"]);
    }

    #[test]
    fn repeated_catalog_metadata_is_bounded_before_reports_and_discovery() {
        let name = "long-input".repeat(100_000);
        let raw = serde_json::json!({"Custom": {"input": {"required": {name: ["STRING", {}]}}, "output": []}});
        let catalog = NodeCatalog::parse(&serde_json::to_vec(&raw).unwrap()).unwrap().0;
        let mut workflow = ApiWorkflow::default();
        for id in 0..100 {
            workflow.nodes.insert(id.to_string(), crate::ApiNode { class_type: "Custom".into(), inputs: Default::default(), title: None });
        }
        let report = workflow.check(&catalog);
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0].message.len() < 200);
        assert!(discover(&workflow, &catalog).is_err());
    }

    #[test]
    fn duplicate_catalog_order_does_not_multiply_large_options_or_exposed_values() {
        let large = "x".repeat(1024 * 1024);
        let raw = serde_json::json!({
            "Choice": {"input":{"required":{"option":[[large]]}}, "input_order":{"required":vec!["option";1000]}, "output":[]},
            "Text": {"input":{"required":{"text":["STRING",{}]}}, "input_order":{"required":vec!["text";1000]}, "output":[]}
        });
        let catalog = NodeCatalog::parse(&serde_json::to_vec(&raw).unwrap()).unwrap().0;
        assert_eq!(catalog.get("Choice").unwrap().inputs.len(), 1);
        assert_eq!(catalog.get("Text").unwrap().inputs.len(), 1);
        let mut workflow = ApiWorkflow::default();
        workflow.nodes.insert(
            "1".into(),
            crate::ApiNode { class_type: "Text".into(), title: None, inputs: [("text".into(), InputValue::Value(Value::String(large)))].into_iter().collect() },
        );
        assert_eq!(discover(&workflow, &catalog).unwrap().inputs.len(), 1);
        workflow.nodes.get_mut("1").unwrap().inputs.insert("text".into(), InputValue::Value(Value::String("x".repeat(MAX_JSON_BYTES / 2 + 1))));
        assert!(workflow.to_json().is_ok());
        assert!(discover(&workflow, &catalog).is_err(), "exposure copies share the aggregate allocation budget");
    }

    #[test]
    fn missing_selector_errors_and_preexisting_hostile_workflows_are_guarded() {
        let catalog = catalog();
        let mut workflow = workflow();
        let large = "x".repeat(MAX_JSON_BYTES);
        for (node, input) in [(large.as_str(), "text"), ("missing", large.as_str())] {
            let error = workflow.set_input(&catalog, node, input, Value::Null).unwrap_err();
            assert!(error.to_string().len() < 200);
        }
        workflow.nodes.get_mut("6").unwrap().inputs.insert("text".into(), InputValue::Value(nested(50_000)));
        assert!(workflow.set_input(&catalog, "6", "text", Value::String("replacement".into())).is_err());
        let InputValue::Value(value) = workflow.nodes.get_mut("6").unwrap().inputs.remove("text").unwrap() else { panic!("literal") };
        // The previous hostile tree was neither replaced nor dropped by the failed edit.
        let mut cursor = &value;
        let mut depth = 0;
        while let Value::Array(values) = cursor {
            depth += 1;
            cursor = values.first().unwrap();
        }
        assert_eq!(depth, 50_000);
        dispose(value);
    }

    #[test]
    fn shared_ui_primitive_values_are_charged_before_each_consumer_clone() {
        let catalog = catalog();
        let mut nodes = vec![serde_json::json!({"id": 1, "type":"PrimitiveNode", "widgets_values":["x".repeat(1024 * 1024)]})];
        let mut links = Vec::new();
        for id in 2..42 {
            nodes.push(serde_json::json!({"id":id, "type":"CLIPTextEncode", "inputs":[{"name":"text","type":"STRING","link":id}]}));
            links.push(serde_json::json!([id, 1, 0, id, 0, "STRING"]));
        }
        let source = serde_json::to_vec(&serde_json::json!({"nodes":nodes,"links":links})).unwrap();
        assert!(source.len() < 2 * 1024 * 1024);
        assert!(ApiWorkflow::convert(&source, Some(&catalog)).is_err());
    }

    #[test]
    fn scalar_request_builders_and_prompt_envelope_have_their_own_budget() {
        let server = Server::new("http://localhost:8188").unwrap();
        let large = "x".repeat(MAX_JSON_BYTES);
        assert!(server.interrupt(Some(&large)).is_err());
        assert!(server.delete_from_queue(&[large]).is_err());
        let mut workflow = workflow();
        workflow.nodes.get_mut("6").unwrap().inputs.insert("text".into(), InputValue::Value(nested(MAX_JSON_DEPTH - 3)));
        assert!(workflow.to_json().is_ok());
        let mut prompt = PromptRequest::new(workflow, "client");
        assert!(prompt.to_json().is_err(), "request envelope consumes one more nesting level");
        let InputValue::Value(value) = prompt.workflow.nodes.get_mut("6").unwrap().inputs.remove("text").unwrap() else { panic!("literal") };
        dispose(value);
    }

    #[test]
    fn borrowed_validator_bounds_width_keys_escaping_and_depth_with_small_scratch() {
        let array = Value::Array(vec![Value::Null; 101]);
        assert!(Budget { max_items: 100, ..Budget::default() }.value(&array, 0).is_err());
        let object = Value::Object((0..100).map(|i| (i.to_string(), Value::Null)).collect());
        assert!(Budget { max_items: 100, ..Budget::default() }.value(&object, 0).is_err());
        let object = Value::Object([("k".repeat(MAX_JSON_BYTES), Value::Null)].into_iter().collect());
        assert!(Budget::default().value(&object, 0).is_err());
        let escaped = Value::String("\u{0000}".repeat(MAX_JSON_BYTES / 6 + 1));
        assert!(Budget::default().value(&escaped, 0).is_err());
        let value = nested(MAX_JSON_DEPTH);
        assert!(Budget::default().value(&value, 0).is_ok());
        dispose(value);
        let value = nested(MAX_JSON_DEPTH + 1);
        assert!(Budget::default().value(&value, 0).is_err());
        dispose(value);
        for value in [serde_json::json!({"\n\\\"": [null, true, false, 123, "é\u{0001}\t"]}), serde_json::json!([])] {
            let mut budget = Budget::default();
            budget.value(&value, 0).unwrap();
            assert_eq!(budget.bytes, serde_json::to_vec(&value).unwrap().len());
        }
    }
    #[test]
    fn upload_combo_discovery_does_not_charge_options_it_never_copies() {
        let options: Vec<String> = (0..4096).map(|id| format!("{}-{id}.png", "x".repeat(512))).collect();
        let raw = serde_json::json!({"LoadImage":{"input":{"required":{"image":[options,{"image_upload":true}]}},"output":["IMAGE"]}});
        let catalog = NodeCatalog::parse(&serde_json::to_vec(&raw).unwrap()).unwrap().0;
        let mut workflow = ApiWorkflow::default();
        for id in 0..20 {
            workflow.nodes.insert(
                id.to_string(),
                crate::ApiNode {
                    class_type: "LoadImage".into(),
                    title: None,
                    inputs: [("image".into(), InputValue::Value(Value::String("selected.png".into())))].into_iter().collect(),
                },
            );
        }
        // Shared catalog options exceed 32 MiB only if wrongly charged for every Media
        // exposure. The 2M item cap alone does not make this regression pass.
        let exposed = discover(&workflow, &catalog).unwrap();
        assert_eq!(exposed.inputs.len(), 20);
        assert!(exposed.inputs.iter().all(|input| matches!(input.kind, crate::ExposedKind::Media(crate::Media::Image))));
    }

    #[test]
    fn representative_max_node_api_and_ui_workflows_round_trip_under_interacting_caps() {
        let mut api = workflow();
        let sampler = api.nodes.get("3").unwrap().clone();
        for id in 100..100 + crate::MAX_NODES - api.nodes.len() {
            api.nodes.insert(id.to_string(), sampler.clone());
        }
        assert_eq!(api.nodes.len(), crate::MAX_NODES);
        let wire = serde_json::to_vec(&api.to_json().unwrap()).unwrap();
        assert!(wire.len() < crate::MAX_WORKFLOW_BYTES);
        assert_eq!(ApiWorkflow::parse(&wire, None).unwrap(), api);
        assert!(PromptRequest::new(api, "client").to_json().is_ok());

        let mut ui: Value = serde_json::from_str(crate::fixtures::UI_WORKFLOW).unwrap();
        let nodes = ui.get_mut("nodes").unwrap().as_array_mut().unwrap();
        let original = nodes.len();
        let sampler = nodes.iter().find(|node| node.get("type").and_then(Value::as_str) == Some("KSampler")).unwrap().clone();
        for id in original..crate::MAX_NODES {
            let mut node = sampler.clone();
            node.as_object_mut().unwrap().insert("id".into(), Value::from(id + 100_000));
            nodes.push(node);
        }
        let catalog = catalog();
        let expected = workflow().nodes.len() + crate::MAX_NODES - original;
        let source = serde_json::to_vec(&ui).unwrap();
        assert!(source.len() < crate::MAX_WORKFLOW_BYTES);
        let converted = ApiWorkflow::parse(&source, Some(&catalog)).unwrap();
        assert_eq!(converted.nodes.len(), expected);
        let wire = serde_json::to_vec(&converted.to_json().unwrap()).unwrap();
        assert_eq!(ApiWorkflow::parse(&wire, None).unwrap(), converted);
    }

    #[test]
    fn local_resource_limit_errors_do_not_blame_the_server_or_echo_values() {
        let value = nested(MAX_JSON_DEPTH + 1);
        let error = Budget::default().value(&value, 0).unwrap_err();
        assert!(matches!(error, ComfyError::InvalidInput { .. }));
        assert!(!error.to_string().contains("from ComfyUI"));
        assert!(!error.to_string().contains("fabricated-json-secret"));
        dispose(value);
    }
}
