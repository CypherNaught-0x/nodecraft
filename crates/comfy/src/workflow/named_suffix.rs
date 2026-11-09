//! A saved named map can prove one trailing obsolete UI widget without guessing a server input.
//! Never use stale named values to overwrite positional values, links or seed control modes.
use super::{ApiNode, InputValue};
use crate::object_info::NodeInfo;
use serde_json::{Map, Value};
use std::collections::HashSet;

pub(super) fn matches(info: &NodeInfo, node: &ApiNode, named: Option<&Map<String, Value>>, saved: &[Value], consumed: usize) -> bool {
    if consumed.checked_add(1) != Some(saved.len()) {
        return false;
    }
    let Some(named) = named else {
        return false;
    };
    let active = info.effective_inputs(node);
    let widgets = active.into_iter().filter(|spec| spec.is_widget()).collect::<Vec<_>>();
    if widgets.len().checked_add(1) != Some(named.len()) {
        return false;
    }
    let names = widgets.iter().map(|spec| spec.name.as_str()).collect::<HashSet<_>>();
    if names.len() != widgets.len() {
        return false;
    }
    for spec in widgets {
        let (Some(InputValue::Value(value)), Some(recorded)) = (node.inputs.get(&spec.name), named.get(&spec.name)) else {
            return false;
        };
        if value != recorded || spec.check(value).is_err() {
            return false;
        }
    }
    let Some((_, extra)) = named.iter().find(|(name, _)| !names.contains(name.as_str())) else {
        return false;
    };
    (extra.is_string() || extra.is_boolean() || extra.is_number()) && saved.get(consumed) == Some(extra)
}

#[cfg(test)]
mod tests {
    use crate::{ApiWorkflow, NodeCatalog, PromptRequest};
    use serde_json::{Value, json};
    fn catalog() -> NodeCatalog {
        NodeCatalog::parse(br#"{"N":{"input":{"required":{"mode":["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"a","inputs":{"required":{"size":["INT",{"min":0,"max":20}]}}}]}],"seed":["INT",{"control_after_generate":true}]}}}}"#).unwrap().0
    }
    fn ui() -> Value {
        json!({"nodes":[{"id":1,"type":"N","widgets_values":["a",17,5,"fixed","auto"],"widgets_values_named":{"mode":"a","mode.size":17,"seed":5,"old_codec":"auto"}}],"links":[]})
    }
    #[test]
    fn templates948_named_obsolete_suffix_preserves_active_values_and_seed_mode() {
        let c = catalog();
        let flow = ApiWorkflow::convert(&serde_json::to_vec(&ui()).unwrap(), Some(&c)).unwrap();
        assert_eq!(flow.controls[&("1".into(), "seed".into())], "fixed");
        let request = PromptRequest::new(flow.workflow, "own-fixture").to_json().unwrap();
        assert_eq!(request["prompt"]["1"]["inputs"], json!({"mode":"a","mode.size":17,"seed":5}));
        assert_eq!(flow.warnings.len(), 1);
    }
    #[test]
    fn templates948_named_suffix_refuses_stale_missing_unmatched_or_multiple_widgets() {
        let c = catalog();
        for variant in 0..5 {
            let mut root = ui();
            let n = &mut root["nodes"][0];
            match variant {
                0 => {
                    n.as_object_mut().unwrap().remove("widgets_values_named");
                }
                1 => n["widgets_values_named"]["mode.size"] = json!(18),
                2 => n["widgets_values_named"]["old_codec"] = json!("different"),
                3 => {
                    n["widgets_values"].as_array_mut().unwrap().push(json!("second"));
                }
                _ => {
                    n["widgets_values_named"].as_object_mut().unwrap().remove("seed");
                }
            }
            assert!(ApiWorkflow::convert(&serde_json::to_vec(&root).unwrap(), Some(&c)).is_err(), "variant {variant}");
        }
    }
    #[test]
    fn templates948_named_suffix_proof_requires_valid_types_bounds_and_no_link_override() {
        let mut c = catalog();
        for value in [json!("not-an-integer"), json!(99)] {
            let mut root = ui();
            root["nodes"][0]["widgets_values"][1] = value.clone();
            root["nodes"][0]["widgets_values_named"]["mode.size"] = value;
            assert!(
                ApiWorkflow::convert(&serde_json::to_vec(&root).unwrap(), Some(&c)).is_err(),
                "equal named/positional values still need the catalog type and bounds"
            );
        }
        c.extend(NodeCatalog::parse(br#"{"Number":{"input":{"required":{}},"output":["INT"]}}"#).unwrap().0);
        let mut root = ui();
        root["nodes"][0]["inputs"] = json!([{"name":"mode.size","type":"INT","link":1}]);
        root["nodes"].as_array_mut().unwrap().push(json!({"id":2,"type":"Number","widgets_values":[],"outputs":[{"name":"INT","type":"INT","links":[1]}]}));
        root["links"] = json!([[1, 2, 0, 1, 0, "INT"]]);
        assert!(ApiWorkflow::convert(&serde_json::to_vec(&root).unwrap(), Some(&c)).is_err(), "a named scalar must never override a live link");
    }
}
