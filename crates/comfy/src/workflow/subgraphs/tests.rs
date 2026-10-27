use super::*;
use crate::discover;
use serde_json::json;
fn catalog() -> NodeCatalog {
    NodeCatalog::parse(br#"{
  "MakeImage":{"input":{"required":{"text":["STRING",{}],"seed":["INT",{"control_after_generate":true}],"reference":["IMAGE",{}]}},"output":["IMAGE"],"api_node":true},
  "PrimitiveStringMultiline":{"input":{"required":{"value":["STRING",{"multiline":true}]}},"output":["STRING"]},
  "PrimitiveInt":{"input":{"required":{"value":["INT",{"control_after_generate":true}]}},"output":["INT"]},
  "LoadImage":{"input":{"required":{"image":[["example.png"],{"image_upload":true}]}},"output":["IMAGE"]},
  "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true}
 }"#).unwrap().0
}
fn flow() -> Value {
    json!({
     "nodes":[
     {"id":1,"type":"LoadImage","outputs":[{"type":"IMAGE","links":[10]}],"widgets_values":["example.png","image"]},
     {"id":42,"type":"bundle","inputs":[{"name":"reference","type":"IMAGE","link":10}],"outputs":[{"type":"IMAGE","links":[11]}],"widgets_values":[]},
     {"id":9,"type":"SaveImage","inputs":[{"name":"images","type":"IMAGE","link":11}],"widgets_values":["NodeCraft"]}],
     "links":[[10,1,0,42,2,"IMAGE"],[11,42,0,9,0,"IMAGE"]],
     "definitions":{"subgraphs":[{
      "id":"bundle","inputNode":{"id":-10},"outputNode":{"id":-20},
      "inputs":[{"name":"text","type":"STRING","linkIds":[100]},{"name":"seed","type":"INT","linkIds":[101]},{"name":"reference","type":"IMAGE","linkIds":[102]}],
      "outputs":[{"name":"IMAGE","type":"IMAGE","linkIds":[103]}],
      "nodes":[{"id":7,"type":"MakeImage","inputs":[{"name":"text","type":"STRING","link":100},{"name":"seed","type":"INT","link":101},{"name":"reference","type":"IMAGE","link":102}],"outputs":[{"type":"IMAGE","links":[103]}],"widgets_values":["original synthetic prompt",7,"randomize"]}],
      "links":[{"id":100,"origin_id":-10,"origin_slot":0,"target_id":7,"target_slot":0},{"id":101,"origin_id":-10,"origin_slot":1,"target_id":7,"target_slot":1},{"id":102,"origin_id":-10,"origin_slot":2,"target_id":7,"target_slot":2},{"id":103,"origin_id":7,"origin_slot":0,"target_id":-20,"target_slot":0}]
     }]}
    })
}
fn convert(v: &Value) -> Result<Conversion, ComfyError> {
    ApiWorkflow::convert(&serde_json::to_vec(v).unwrap(), Some(&catalog()))
}
#[test]
fn templates948_subgraph_boundaries_keep_defaults_media_and_seed_modes() {
    let c = convert(&flow()).unwrap();
    let n = &c.workflow.nodes["42:7"];
    assert_eq!(n.inputs["text"], InputValue::Value(json!("original synthetic prompt")));
    assert_eq!(n.inputs["seed"], InputValue::Value(json!(7)));
    assert_eq!(n.inputs["reference"], InputValue::Link { node: "1".into(), output: 0 });
    assert_eq!(c.workflow.nodes["9"].inputs["images"], InputValue::Link { node: "42:7".into(), output: 0 });
    assert_eq!(c.controls[&("42:7".into(), "seed".into())], "randomize");
    assert_eq!(discover(&c.workflow, &catalog()).unwrap().partners.len(), 1);
}
#[test]
fn templates948_instance_values_and_named_proxies_override_only_unwired_widgets() {
    let mut v = flow();
    v["nodes"][1]["widgets_values"] = json!(["changed synthetic prompt", 99, "fixed"]);
    let c = convert(&v).unwrap();
    assert_eq!(c.workflow.nodes["42:7"].inputs["seed"], InputValue::Value(json!(99)));
    assert_eq!(c.controls[&("42:7".into(), "seed".into())], "fixed");
    v["nodes"][1]["properties"] = json!({"proxyWidgets":[["7","text"],["7","seed"],["7","control_after_generate"]]});
    v["nodes"][1]["widgets_values"] = json!(["proxy synthetic prompt", 123, "decrement"]);
    v["nodes"][1]["inputs"].as_array_mut().unwrap().push(json!({"name":"text","type":"STRING","link":12}));
    v["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":5,"type":"PrimitiveNode","outputs":[{"type":"STRING","links":[12]}],"widgets_values":["wired synthetic prompt"]}));
    v["links"].as_array_mut().unwrap().push(json!([12, 5, 0, 42, 0, "STRING"]));
    let c = convert(&v).unwrap();
    assert_eq!(c.workflow.nodes["42:7"].inputs["text"], InputValue::Value(json!("wired synthetic prompt")));
    assert_eq!(c.workflow.nodes["42:7"].inputs["seed"], InputValue::Value(json!(123)));
    assert_eq!(c.controls[&("42:7".into(), "seed".into())], "decrement");
}
#[test]
fn templates948_repeated_instances_have_distinct_node_ids_and_values() {
    let mut v = flow();
    let mut another = v["nodes"][1].clone();
    another["id"] = json!(43);
    another["widgets_values"] = json!(["second synthetic prompt", 88, "fixed"]);
    v["nodes"].as_array_mut().unwrap().push(another);
    let c = convert(&v).unwrap();
    assert!(c.workflow.nodes.contains_key("42:7"));
    assert!(c.workflow.nodes.contains_key("43:7"));
    assert_eq!(c.workflow.nodes["42:7"].inputs["seed"], InputValue::Value(json!(7)));
    assert_eq!(c.workflow.nodes["43:7"].inputs["seed"], InputValue::Value(json!(88)));
}
#[test]
fn templates948_muted_and_bypassed_instances_do_not_activate_internal_paid_nodes() {
    for mode in [MODE_MUTED, MODE_BYPASS] {
        let mut v = flow();
        v["nodes"][1]["mode"] = json!(mode);
        let c = convert(&v).unwrap();
        assert!(!c.workflow.nodes.contains_key("42:7"));
        assert!(discover(&c.workflow, &catalog()).unwrap().partners.is_empty());
        if mode == MODE_BYPASS {
            assert_eq!(c.workflow.nodes["9"].inputs["images"], InputValue::Link { node: "1".into(), output: 0 });
        }
    }
}
#[test]
fn templates948_ambiguous_widgets_recursive_definitions_and_namespace_collisions_are_errors() {
    let mut v = flow();
    v["nodes"][1]["widgets_values"] = json!(["only one"]);
    assert!(convert(&v).unwrap_err().to_string().contains("widget"));
    let mut v = flow();
    v["definitions"]["subgraphs"][0]["nodes"].as_array_mut().unwrap().push(json!({"id":8,"type":"bundle"}));
    assert!(convert(&v).unwrap_err().to_string().contains("cycle"));
    let mut v = flow();
    v["nodes"].as_array_mut().unwrap().push(json!({"id":"42:7","type":"MakeImage","widgets_values":["collision",1,"fixed"]}));
    assert!(convert(&v).unwrap_err().to_string().contains("reuse"));
}
#[test]
fn templates948_shared_definition_amplification_is_bounded_before_retention() {
    let mut v = flow();
    v["definitions"]["subgraphs"][0]["nodes"][0]["widgets_values"][0] = json!("x".repeat(64 * 1024));
    let template = v["nodes"][1].clone();
    for id in 100..900 {
        let mut instance = template.clone();
        instance["id"] = json!(id);
        v["nodes"].as_array_mut().unwrap().push(instance);
    }
    assert!(convert(&v).unwrap_err().to_string().contains("32 MiB"));
}

fn fanout() -> Value {
    let mut v = flow();
    let g = &mut v["definitions"]["subgraphs"][0];
    let mut another = g["nodes"][0].clone();
    another["id"] = json!(8);
    for (slot, link) in [(0, 104), (1, 105), (2, 106)] {
        another["inputs"][slot]["link"] = json!(link);
        g["inputs"][slot]["linkIds"].as_array_mut().unwrap().push(json!(link));
    }
    g["nodes"].as_array_mut().unwrap().push(another);
    for (slot, link) in [(0, 104), (1, 105), (2, 106)] {
        g["links"].as_array_mut().unwrap().push(json!({"id":link,"origin_id":-10,"origin_slot":slot,"target_id":8,"target_slot":slot}));
    }
    v
}
#[test]
fn templates948_shared_prompts_and_seeds_are_single_editable_free_primitives() {
    let c = convert(&fanout()).unwrap();
    for name in ["text", "seed"] {
        let a = &c.workflow.nodes["42:7"].inputs[name];
        let b = &c.workflow.nodes["42:8"].inputs[name];
        assert_eq!(a, b);
        let InputValue::Link { node, output } = a else { panic!("shared value lost its common driver") };
        assert_eq!(*output, 0);
        let driver = &c.workflow.nodes[node];
        assert!(driver.class_type.starts_with("Primitive"));
        assert!(driver.inputs.contains_key("value"));
        assert!(c.promoted.iter().any(|p| p.instance == "42" && p.name == name && p.targets == [(node.clone(), "value".into())]));
    }
    assert_eq!(discover(&c.workflow, &catalog()).unwrap().partners.len(), 2, "synthetic drivers add no paid nodes");
}
#[test]
fn templates948_different_unprovided_defaults_stay_distinct() {
    let mut v = fanout();
    v["definitions"]["subgraphs"][0]["nodes"][1]["widgets_values"][0] = json!("different original synthetic prompt");
    let c = convert(&v).unwrap();
    assert_ne!(c.workflow.nodes["42:7"].inputs["text"], c.workflow.nodes["42:8"].inputs["text"]);
    assert_eq!(c.promoted.iter().find(|p| p.name == "text").unwrap().targets.len(), 2);
}
#[test]
fn templates948_shared_widgets_refuse_a_paid_or_missing_primitive() {
    let v = fanout();
    let info = br#"{
      "MakeImage":{"input":{"required":{"text":["STRING",{}],"seed":["INT",{"control_after_generate":true}],"reference":["IMAGE",{}]}},"output":["IMAGE"]},
      "LoadImage":{"input":{"required":{"image":[["example.png"],{"image_upload":true}]}},"output":["IMAGE"]},
      "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true},
      "PrimitiveStringMultiline":{"input":{"required":{"value":["STRING",{}]}},"output":["STRING"],"api_node":true}
    }"#;
    let catalog = NodeCatalog::parse(info).unwrap().0;
    assert!(ApiWorkflow::convert(&serde_json::to_vec(&v).unwrap(), Some(&catalog)).unwrap_err().to_string().contains("compatible free Primitive"));
}

#[test]
fn templates948_promoted_roles_select_one_shared_seed_control() {
    let c = convert(&fanout()).unwrap();
    let mut exposed = discover(&c.workflow, &catalog()).unwrap();
    crate::annotate_promoted(&mut exposed, &c.promoted);
    let seeds = exposed.inputs.iter().filter(|i| i.role == crate::InputRole::Seed).collect::<Vec<_>>();
    assert_eq!(seeds.len(), 1);
    assert_eq!(seeds[0].input, "value");
    assert!(seeds[0].role.exposed_by_default());
}
#[test]
fn templates948_nested_subgraphs_forward_values_and_modes_through_both_boundaries() {
    let mut v = flow();
    let inner = v["definitions"]["subgraphs"][0].clone();
    let mut outer = inner.clone();
    outer["id"] = json!("outer");
    outer["nodes"] = json!([{"id":3,"type":"bundle","inputs":[{"name":"text","type":"STRING","link":201},{"name":"seed","type":"INT","link":202},{"name":"reference","type":"IMAGE","link":203}],"outputs":[{"type":"IMAGE","links":[204]}],"widgets_values":[]}]);
    outer["links"] = json!([[201, -10, 0, 3, 0, "STRING"], [202, -10, 1, 3, 1, "INT"], [203, -10, 2, 3, 2, "IMAGE"], [204, 3, 0, -20, 0, "IMAGE"]]);
    for (slot, id) in [(0, 201), (1, 202), (2, 203)] {
        outer["inputs"][slot]["linkIds"] = json!([id]);
    }
    outer["outputs"][0]["linkIds"] = json!([204]);
    v["definitions"]["subgraphs"] = json!([inner, outer]);
    v["nodes"][1]["type"] = json!("outer");
    v["nodes"][1]["widgets_values"] = json!(["outer synthetic prompt", 125, "fixed"]);
    let c = convert(&v).unwrap();
    assert_eq!(c.workflow.nodes["42:3:7"].inputs["text"], InputValue::Value(json!("outer synthetic prompt")));
    assert_eq!(c.workflow.nodes["42:3:7"].inputs["seed"], InputValue::Value(json!(125)));
    assert_eq!(c.workflow.nodes["9"].inputs["images"], InputValue::Link { node: "42:3:7".into(), output: 0 });
    assert_eq!(c.controls[&("42:3:7".into(), "seed".into())], "fixed");
}
#[test]
fn templates948_cyclic_boundary_connections_are_errors() {
    let mut v = flow();
    let g = &mut v["definitions"]["subgraphs"][0];
    g["links"][3]["origin_id"] = json!(-10);
    v["nodes"][1]["inputs"].as_array_mut().unwrap().push(json!({"name":"text","type":"STRING","link":12}));
    v["links"].as_array_mut().unwrap().push(json!([12, 42, 0, 42, 0, "STRING"]));
    assert!(convert(&v).unwrap_err().to_string().contains("cycle"));
}

#[test]
fn templates948_dynamic_widget_groups_are_not_misread_as_the_following_seed() {
    let catalog =
        NodeCatalog::parse(br#"{"Dynamic":{"input":{"required":{"model":["COMFY_DYNAMICCOMBO_V3",{}],"seed":["INT",{}]}},"output":["IMAGE"]}}"#).unwrap().0;
    let v = json!({"nodes":[{"id":1,"type":"Dynamic","widgets_values":["model-choice","wide",17]}],"links":[]});
    let error = ApiWorkflow::convert(&serde_json::to_vec(&v).unwrap(), Some(&catalog)).unwrap_err();
    assert!(error.to_string().contains("dynamic widgets"));
}

#[test]
fn templates948_boundary_alias_cannot_redirect_an_existing_string_id_node() {
    let mut v = flow();
    v["nodes"].as_array_mut().unwrap().extend([
        json!({"id":"42:-10","type":"PrimitiveStringMultiline","widgets_values":["Independent external text"],"outputs":[{"type":"STRING","links":[200]}]}),
        json!({"id":99,"type":"MakeImage","inputs":[{"name":"text","type":"STRING","link":200},{"name":"reference","type":"IMAGE","link":201}],"widgets_values":["Fallback text",13,"fixed"]}),
    ]);
    v["links"].as_array_mut().unwrap().extend([json!([200, "42:-10", 0, 99, 0, "STRING"]), json!([201, 1, 0, 99, 2, "IMAGE"])]);
    let error = convert(&v).unwrap_err();
    assert!(error.to_string().contains("aliases reuse an executable node id"));
}
