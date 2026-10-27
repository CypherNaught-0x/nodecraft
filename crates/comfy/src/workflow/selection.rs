//! Explicit result selection keeps the complete dependency closure, without unrelated work.
use super::{ApiWorkflow, InputValue, MAX_NODES};
use crate::ComfyError;
use std::collections::HashSet;

impl ApiWorkflow {
    /// Restrict a card request to its explicitly selected outputs and every upstream dependency.
    /// Cycles terminate; invalid/missing dependencies refuse rather than disappear. The original
    /// workflow is unchanged on failure. Generic workflow validation remains strict.
    pub fn retain_upstream(&mut self, outputs: &[&str]) -> Result<(), ComfyError> {
        let invalid = || ComfyError::InvalidWorkflow("selected outputs need existing nodes and bounded dependencies".into());
        if outputs.is_empty() || outputs.len() > MAX_NODES {
            return Err(invalid());
        }
        let mut budget = crate::json_bounds::Budget::default();
        self.validate_budget(&mut budget, 0)?;
        // Every retained id is copied once; account for those copies before allocation.
        for id in self.nodes.keys() {
            budget.text(id, 0)?;
        }
        let mut seen = HashSet::new();
        seen.try_reserve(self.nodes.len()).map_err(|_| invalid())?;
        let mut pending = Vec::new();
        pending.try_reserve(self.nodes.len()).map_err(|_| invalid())?;
        for root in outputs {
            let (id, _) = self.nodes.get_key_value(*root).ok_or_else(invalid)?;
            if seen.insert(id.clone()) {
                pending.push(id.as_str());
            }
        }
        while let Some(id) = pending.pop() {
            let node = self.nodes.get(id).ok_or_else(invalid)?;
            for input in node.inputs.values() {
                if let InputValue::Link { node: from, .. } = input {
                    let (from, _) = self.nodes.get_key_value(from).ok_or_else(invalid)?;
                    if seen.insert(from.clone()) {
                        pending.push(from.as_str());
                    }
                }
            }
        }
        self.nodes.retain(|id, _| seen.contains(id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeCatalog;
    #[test]
    fn templates948_selected_results_exclude_unreachable_helpers_and_keep_invalid_ancestors() {
        let catalog=NodeCatalog::parse(br#"{"Source":{"input":{"required":{"value":["STRING",{}]}},"output":["IMAGE"]},"Save":{"input":{"required":{"image":["IMAGE",{}]}},"output_node":true},"Helper":{"input":{"required":{"image":["IMAGE",{}]}},"output":["IMAGE"]}}"#).unwrap().0;
        let mut flow=ApiWorkflow::parse(br#"{"1":{"class_type":"Source","inputs":{"value":"original"}},"2":{"class_type":"Save","inputs":{"image":["1",0]}},"3":{"class_type":"Helper","inputs":{}},"4":{"class_type":"Save","inputs":{"image":["3",0]}}}"#,None).unwrap();
        assert!(!flow.check(&catalog).is_ok());
        flow.retain_upstream(&["2"]).unwrap();
        assert_eq!(flow.nodes.keys().map(String::as_str).collect::<Vec<_>>(), ["1", "2"]);
        assert!(flow.check(&catalog).is_ok());
        flow.nodes.get_mut("1").unwrap().inputs.clear();
        flow.retain_upstream(&["2"]).unwrap();
        assert!(!flow.check(&catalog).is_ok(), "required ancestor checks remain strict");
    }
    #[test]
    fn templates948_selection_cycles_missing_nodes_and_work_limits_are_atomic() {
        let mut cycle = ApiWorkflow::parse(
            br#"{"1":{"class_type":"A","inputs":{"x":["2",0]}},"2":{"class_type":"B","inputs":{"x":["1",0]}},"3":{"class_type":"C","inputs":{}}}"#,
            None,
        )
        .unwrap();
        let before = cycle.clone();
        for roots in [vec![], vec!["absent"], vec!["1"; MAX_NODES + 1]] {
            assert!(cycle.retain_upstream(&roots).is_err());
            assert_eq!(cycle, before);
        }
        cycle.retain_upstream(&["1"]).unwrap();
        assert_eq!(cycle.nodes.len(), 2);
        cycle.nodes.remove("2");
        let before = cycle.clone();
        assert!(cycle.retain_upstream(&["1"]).is_err());
        assert_eq!(cycle, before);
        let mut oversized = before;
        oversized
            .nodes
            .get_mut("1")
            .unwrap()
            .inputs
            .insert("large".into(), InputValue::Value(serde_json::Value::String("x".repeat(super::super::MAX_WORKFLOW_BYTES + 1))));
        assert!(oversized.retain_upstream(&["1"]).is_err());
    }
    #[test]
    fn templates948_selection_keeps_dotted_dynamic_and_autogrow_links_without_guessing_literal_arrays() {
        let mut flow=ApiWorkflow::parse(br#"{"1":{"class_type":"Dynamic","inputs":{"task":"ingredients","task.refs.first":["2",0],"values.a":["3",0],"literal":["not-a-link",{"data":0}]}},"2":{"class_type":"Image","inputs":{}},"3":{"class_type":"Number","inputs":{}},"9":{"class_type":"Unused","inputs":{}}}"#,None).unwrap();
        let literal = flow.nodes["1"].inputs["literal"].clone();
        flow.retain_upstream(&["1"]).unwrap();
        assert_eq!(flow.nodes.keys().map(String::as_str).collect::<Vec<_>>(), ["1", "2", "3"]);
        assert_eq!(flow.nodes["1"].inputs["literal"], literal);
    }
}
