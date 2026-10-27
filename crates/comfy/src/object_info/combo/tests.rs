use super::*;
use crate::{ApiWorkflow, NodeCatalog, PromptRequest, discover};
use serde_json::json;
// Original synthetic contract. Dotted API paths are demonstrated by the public reproduction
// in Comfy-Org/ComfyUI_frontend#20068; no implementation code is used.
fn catalog() -> NodeCatalog {
    NodeCatalog::parse(br#"{"Dynamic":{"input":{"required":{
      "task":["COMFY_DYNAMICCOMBO_V3",{"options":[
        {"key":"image","inputs":{"required":{"model":[["a","b"],{}]}}},
        {"key":"video","inputs":{"required":{
          "model":[["c","d"],{}],
          "reference_mode":["COMFY_DYNAMICCOMBO_V3",{"options":[
            {"key":"frames","inputs":{"optional":{"start_frame":["IMAGE",{}]}}},
            {"key":"ingredients","inputs":{"optional":{"references":["COMFY_AUTOGROW_V3",{"template":{"input":{"required":{"image":["IMAGE",{}]}},"names":["first","second"],"min":0}}]}}}
          ]}]
        }}}
      ]}],"seed":["INT",{"min":0,"max":100,"control_after_generate":true}]
    }},"output":["IMAGE"]},"LoadImage":{"input":{"required":{"image":[["fixture.png"],{"image_upload":true}]}},"output":["IMAGE"]}}"#).unwrap().0
}
fn ui() -> Value {
    json!({"nodes":[{"id":1,"type":"Dynamic","widgets_values":["video","d","ingredients",17,"fixed"]}],"links":[]})
}
#[test]
fn templates948_dynamic_combo_nested_widgets_use_dotted_api_paths_and_keep_following_seed() {
    let catalog = catalog();
    let converted = ApiWorkflow::convert(&serde_json::to_vec(&ui()).unwrap(), Some(&catalog)).unwrap();
    converted.workflow.check(&catalog).into_result().unwrap();
    let request = PromptRequest::new(converted.workflow.clone(), "fixture").to_json().unwrap();
    assert_eq!(request["prompt"]["1"]["inputs"], json!({"task":"video","task.model":"d","task.reference_mode":"ingredients","seed":17}));
    assert_eq!(converted.controls[&("1".into(), "seed".into())], "fixed");
    let fields = discover(&converted.workflow, &catalog).unwrap();
    assert_eq!(fields.inputs.iter().map(|i| i.input.as_str()).collect::<Vec<_>>(), ["task", "task.model", "task.reference_mode", "seed"]);
    assert_eq!(ApiWorkflow::parse(&serde_json::to_vec(&converted.workflow.to_json().unwrap()).unwrap(), None).unwrap(), converted.workflow);
}
#[test]
fn templates948_dynamic_combo_linked_child_keeps_its_unlinked_siblings() {
    let catalog = catalog();
    let mut root = ui();
    root["nodes"][0]["inputs"] = json!([{"name":"task.model","type":"COMBO","link":1}]);
    root["nodes"].as_array_mut().unwrap().push(json!({"id":2,"type":"PrimitiveNode","widgets_values":["c"]}));
    root["links"] = json!([[1, 2, 0, 1, 1, "STRING"]]);
    let converted = ApiWorkflow::convert(&serde_json::to_vec(&root).unwrap(), Some(&catalog)).unwrap();
    converted.workflow.check(&catalog).into_result().unwrap();
    assert_eq!(converted.workflow.nodes["1"].inputs["task.model"], InputValue::Value(json!("c")));
    assert_eq!(converted.workflow.nodes["1"].inputs["seed"], InputValue::Value(json!(17)));
    assert_eq!(converted.workflow.nodes["1"].inputs["task.reference_mode"], InputValue::Value(json!("ingredients")));
}
#[test]
fn templates948_dynamic_combo_checks_only_selected_branch_and_rejects_unknown_or_inactive_children() {
    let catalog = catalog();
    let mut workflow = ApiWorkflow::parse(br#"{"1":{"class_type":"Dynamic","inputs":{"task":"image","task.model":"a","seed":1}}}"#, None).unwrap();
    workflow.check(&catalog).into_result().unwrap();
    workflow.set_input(&catalog, "1", "task.model", json!("b")).unwrap();
    assert!(workflow.set_input(&catalog, "1", "task.model", json!("private-invalid-value")).is_err());
    workflow.nodes.get_mut("1").unwrap().inputs.insert("task.reference_mode".into(), InputValue::Value(json!("frames")));
    assert!(!workflow.check(&catalog).is_ok());
    workflow.nodes.get_mut("1").unwrap().inputs.remove("task.reference_mode");
    workflow.nodes.get_mut("1").unwrap().inputs.remove("task.model");
    assert!(!workflow.check(&catalog).is_ok(), "selected required children must be present");
    let mut root = ui();
    root["nodes"][0]["widgets_values"] = json!(["video", "d", "ingredients", 17, "fixed", 42]);
    assert!(ApiWorkflow::convert(&serde_json::to_vec(&root).unwrap(), Some(&catalog)).is_err(), "extra widgets cannot shift following values");
}
#[test]
fn templates948_dynamic_combo_nested_autogrow_names_types_and_slots_are_checked() {
    let catalog = catalog();
    let mut workflow = ApiWorkflow::convert(&serde_json::to_vec(&ui()).unwrap(), Some(&catalog)).unwrap().workflow;
    workflow.nodes.insert(
        "2".into(),
        crate::ApiNode { class_type: "LoadImage".into(), inputs: [("image".into(), InputValue::Value(json!("fixture.png")))].into(), title: None },
    );
    workflow.nodes.get_mut("1").unwrap().inputs.insert("task.reference_mode.references.first".into(), InputValue::Link { node: "2".into(), output: 0 });
    workflow.check(&catalog).into_result().unwrap();
    workflow.nodes.get_mut("1").unwrap().inputs.insert("task.reference_mode.references.third".into(), InputValue::Link { node: "2".into(), output: 0 });
    assert!(!workflow.check(&catalog).is_ok());
}
#[test]
fn templates948_dynamic_combo_deep_or_overwide_descriptor_is_refused_without_partial_compilation() {
    let mut spec = json!(["STRING", {}]);
    for _ in 0..18 {
        spec = json!(["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"next","inputs":{"required":{"child":spec}}}]}]);
    }
    let raw = json!({"N":{"input":{"required":{"task":spec}},"output":[]}});
    let catalog = NodeCatalog::parse(&serde_json::to_vec(&raw).unwrap()).unwrap().0;
    assert!(catalog.get("N").unwrap().dynamic_combos.is_empty());
    let root = json!({"nodes":[{"id":1,"type":"N","widgets_values":["next"]}],"links":[]});
    assert!(ApiWorkflow::convert(&serde_json::to_vec(&root).unwrap(), Some(&catalog)).is_err());
}

#[test]
fn templates948_dynamic_combo_numeric_or_wired_selectors_are_never_accepted_without_validating_children() {
    let catalog=NodeCatalog::parse(br#"{"N":{"input":{"required":{"mode":["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"1","inputs":{"required":{"width":["INT",{"min":1}]}}}]}]}},"output":["STRING"]}}"#).unwrap().0;
    let workflow = ApiWorkflow::parse(br#"{"1":{"class_type":"N","inputs":{"mode":1}}}"#, None).unwrap();
    assert!(!workflow.check(&catalog).is_ok());
    let workflow = ApiWorkflow::parse(br#"{"1":{"class_type":"N","inputs":{"mode":"1"}}}"#, None).unwrap();
    assert!(!workflow.check(&catalog).is_ok(), "the required active child remains required");
}

#[test]
fn templates948_recorded_dynamic_metadata_preserves_selected_types_and_ranges() {
    let catalog = NodeCatalog::parse(include_bytes!("../../templates/fixtures/catalog-facts.json")).unwrap().0;
    let info = catalog.get("BlockSparseAttention").unwrap();
    let node = crate::ApiNode { class_type: info.class_type.clone(), inputs: [("selection".into(), InputValue::Value(json!("sla")))].into(), title: None };
    let active = info.effective_inputs(&node);
    assert!(active.iter().any(|s| s.name == "selection.keep_percent"));
    assert!(!active.iter().any(|s| s.name == "selection.tau"));
    let spec = info.effective_input("selection.keep_percent", &node).unwrap();
    assert!(spec.check(&json!(50.0)).is_ok());
    assert!(spec.check(&json!(1000.0)).is_err());
}

#[test]
fn templates948_dynamic_combo_malformed_child_groups_and_conflicting_paths_are_not_partially_compiled() {
    for raw in [
        json!({"N":{"input":{"required":{"mode":["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"a","inputs":{"required":[]}}]}]}},"output":[]}}),
        json!({"N":{"input":{"required":{"mode":["COMFY_DYNAMICCOMBO_V3",{"options":[{"key":"a","inputs":{"required":{"size":["INT",{}]}}}]}],"mode.size":["INT",{}]}},"output":[]}}),
    ] {
        let (catalog, _) = NodeCatalog::parse(&serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(catalog.get("N").is_none_or(|n| n.dynamic_combos.is_empty()));
    }
}
