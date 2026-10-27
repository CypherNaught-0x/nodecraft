use crate::{ApiWorkflow, InputValue, NodeCatalog};
use serde_json::{Value, json};

fn raw() -> Value {
    json!({
      "Number":{"input":{"required":{"value":["FLOAT",{}]}},"output":["FLOAT"]},
      "PrimitiveInt":{"input":{"required":{"value":["INT",{"control_after_generate":true}]}},"output":["INT"]},
      "PrimitiveStringMultiline":{"input":{"required":{"value":["STRING",{"multiline":true}]}},"output":["STRING"]},
      "Image":{"input":{"required":{"image":["STRING",{}]}},"output":["IMAGE"]},
      "Calculate":{"input":{"required":{"expression":["STRING",{}],"values":["COMFY_AUTOGROW_V3",{"template":{"names":["a","b"],"min":1,"input":{"required":{"value":["FLOAT,INT,BOOLEAN",{}]}}}}]}},"output":["FLOAT"]},
      "Batch":{"input":{"required":{"images":["COMFY_AUTOGROW_V3",{"template":{"prefix":"image","min":1,"max":50,"input":{"required":{"image":["IMAGE",{}]}}}}]}},"output":["IMAGE"]},
      "Switch":{"input":{"required":{"switch":["BOOLEAN",{}]},"optional":{"on_false":["COMFY_MATCHTYPE_V3",{}],"on_true":["COMFY_MATCHTYPE_V3",{}]}},"output":["COMFY_MATCHTYPE_V3"]},
      "Sink":{"input":{"required":{"image":["IMAGE",{}]}},"output":[],"output_node":true}
    })
}
fn catalog(v: &Value) -> NodeCatalog {
    NodeCatalog::parse(&serde_json::to_vec(v).unwrap()).unwrap().0
}
fn workflow(v: Value) -> ApiWorkflow {
    ApiWorkflow::parse(&serde_json::to_vec(&v).unwrap(), None).unwrap()
}
#[test]
fn templates948_autogrow_uses_one_descriptor_and_preserves_dotted_ui_connections() {
    let c = catalog(&raw());
    let info = c.get("Batch").unwrap();
    assert!(info.inputs.is_empty());
    assert_eq!(info.dynamic_inputs.len(), 1);
    assert!(info.input("images.image0").is_none(), "the ordinary widget converter must keep dynamic links");
    let ui = json!({"nodes":[
      {"id":1,"type":"Image","widgets_values":["fixture.png"],"outputs":[{"type":"IMAGE","links":[1]}]},
      {"id":2,"type":"Batch","inputs":[{"name":"images.image0","type":"IMAGE","link":1}],"widgets_values":[]}
    ],"links":[[1,1,0,2,0,"IMAGE"]]});
    let w = ApiWorkflow::parse(&serde_json::to_vec(&ui).unwrap(), Some(&c)).unwrap();
    assert_eq!(w.nodes["2"].inputs["images.image0"], InputValue::Link { node: "1".into(), output: 0 });
    assert!(w.check(&c).is_ok());
}
#[test]
fn templates948_named_autogrow_validates_supplied_ports_without_requiring_a_root_value() {
    let c = catalog(&raw());
    let w = workflow(json!({
      "1":{"class_type":"Number","inputs":{"value":2}},
      "2":{"class_type":"Calculate","inputs":{"expression":"a + 1","values.a":["1",0]}},
      "3":{"class_type":"Calculate","inputs":{"expression":"42"}}
    }));
    assert!(w.check(&c).is_ok());
    for name in ["values.c", "values", "values.a.extra"] {
        let mut bad = w.clone();
        bad.nodes.get_mut("2").unwrap().inputs.insert(name.into(), InputValue::Link { node: "1".into(), output: 0 });
        let report = bad.check(&c);
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].input, name);
    }
}
#[test]
fn templates948_autogrow_checks_types_missing_nodes_and_output_slots() {
    let c = catalog(&raw());
    let w = workflow(json!({"1":{"class_type":"Image","inputs":{"image":"fixture.png"}},"2":{"class_type":"Batch","inputs":{"images.image0":["1",0]}}}));
    assert!(w.check(&c).is_ok());
    for (source, slot) in [("missing", 0), ("1", 100)] {
        let mut bad = w.clone();
        bad.nodes.get_mut("2").unwrap().inputs.insert("images.image0".into(), InputValue::Link { node: source.into(), output: slot });
        assert_eq!(bad.check(&c).problems.len(), 1);
    }
    let mut bad = w;
    bad.nodes.get_mut("1").unwrap().class_type = "Number".into();
    bad.nodes.get_mut("1").unwrap().inputs = [("value".into(), InputValue::Value(json!(2)))].into_iter().collect();
    assert!(bad.check(&c).problems.iter().any(|p| p.input == "images.image0" && p.message.contains("expects IMAGE")));
}
#[test]
fn templates948_numbered_autogrow_bounds_names_without_expanding_the_maximum() {
    let mut v = raw();
    v["Batch"]["input"]["required"]["images"][1]["template"]["max"] = json!(u64::MAX);
    let c = catalog(&v);
    let info = c.get("Batch").unwrap();
    assert_eq!(info.dynamic_inputs.len(), 1);
    assert!(info.dynamic_input("images.image18446744073709551614").unwrap().is_some());
    for name in ["images.image18446744073709551615", "images.image18446744073709551616", "images.image-1", "images.image01", "images.image", "images.other0"] {
        assert!(info.dynamic_input(name).is_err(), "{name}");
    }
}
#[test]
fn templates948_matchtype_inputs_and_outputs_are_polymorphic_connections() {
    let w = workflow(json!({
      "1":{"class_type":"Image","inputs":{"image":"fixture.png"}},
      "2":{"class_type":"Switch","inputs":{"switch":false,"on_false":["1",0]}},
      "3":{"class_type":"Sink","inputs":{"image":["2",0]}}
    }));
    for forced in [false, true] {
        let mut info = raw();
        info["Switch"]["input"]["optional"]["on_false"][1]["forceInput"] = json!(forced);
        info["Switch"]["input"]["optional"]["on_true"][1]["forceInput"] = json!(forced);
        assert!(w.check(&catalog(&info)).is_ok(), "forceInput must not hide a polymorphic type");
    }
}
#[test]
fn templates948_scalar_or_malformed_autogrow_groups_are_not_treated_as_widgets() {
    for template in [
        json!({"prefix":"x","input":{"required":{"value":["STRING",{}]}}}),
        json!({"names":["a","a"],"input":{"required":{"value":["IMAGE",{}]}}}),
        json!({"prefix":"x","names":["a"],"input":{"required":{"value":["IMAGE",{}]}}}),
        json!({"prefix":"x","input":{"required":{"value":["IMAGE",{}],"other":["IMAGE",{}]}}}),
    ] {
        let mut v = raw();
        v["Batch"]["input"]["required"]["images"][1]["template"] = template;
        let c = catalog(&v);
        assert!(c.get("Batch").unwrap().dynamic_inputs.is_empty());
        let ui = json!({"nodes":[{"id":1,"type":"Batch","widgets_values":["must not become another input"]}],"links":[]});
        let error = ApiWorkflow::parse(&serde_json::to_vec(&ui).unwrap(), Some(&c)).unwrap_err();
        assert!(error.to_string().contains("dynamic widgets"));
    }
}

#[test]
fn templates948_dynamic_port_and_missing_source_error_context_is_bounded() {
    let c = catalog(&raw());
    let mut w = workflow(json!({"1":{"class_type":"Batch","inputs":{}}}));
    let node = w.nodes.get_mut("1").unwrap();
    node.inputs.insert(format!("images.{}", "é".repeat(100_000)), InputValue::Value(Value::Null));
    node.inputs.insert("images.image0".into(), InputValue::Link { node: "🦀".repeat(100_000), output: 0 });
    let report = w.check(&c);
    assert_eq!(report.problems.len(), 2);
    for p in report.problems {
        assert!(p.node.len() <= 128 && p.input.len() <= 128 && p.message.len() < 300);
    }
}

#[test]
fn templates948_frontend_primitive_values_survive_dynamic_connections_and_keep_modes() {
    let ui = json!({"nodes":[
      {"id":1,"type":"PrimitiveNode","outputs":[{"type":"INT","links":[1,2]}],"widgets_values":[7,"randomize"]},
      {"id":2,"type":"Calculate","inputs":[{"name":"values.a","type":"FLOAT,INT,BOOLEAN","link":1},{"name":"values.b","type":"FLOAT,INT,BOOLEAN","link":2}],"widgets_values":["a + b"]}
    ],"links":[[1,1,0,2,0,"INT"],[2,1,0,2,1,"INT"]]});
    let c = catalog(&raw());
    let converted = ApiWorkflow::convert(&serde_json::to_vec(&ui).unwrap(), Some(&c)).unwrap();
    assert_eq!(converted.workflow.nodes["1"].class_type, "PrimitiveInt");
    assert_eq!(converted.workflow.nodes["1"].inputs["value"], InputValue::Value(json!(7)));
    assert_eq!(converted.workflow.nodes["2"].inputs["values.a"], converted.workflow.nodes["2"].inputs["values.b"]);
    assert_eq!(converted.controls[&("1".into(), "value".into())], "randomize");
    assert!(converted.workflow.check(&c).is_ok());
    let mut paid = raw();
    paid["PrimitiveInt"]["api_node"] = json!(true);
    assert!(
        ApiWorkflow::convert(&serde_json::to_vec(&ui).unwrap(), Some(&catalog(&paid))).unwrap_err().to_string().contains("compatible free typed Primitive")
    );
}

#[test]
fn templates948_shared_frontend_text_uses_one_value_for_many_connection_only_consumers() {
    let mut info = raw();
    info["TextSink"] = json!({"input":{"required":{"text":["STRING",{"forceInput":true}]}},"output":[],"output_node":true});
    let c = catalog(&info);
    let mut nodes =
        vec![json!({"id":1,"type":"PrimitiveNode","outputs":[{"type":"STRING","links":(2..42).collect::<Vec<_>>()}],"widgets_values":["x".repeat(1024*1024)]})];
    let mut links = Vec::new();
    for id in 2..42 {
        nodes.push(json!({"id":id,"type":"TextSink","inputs":[{"name":"text","type":"STRING","link":id}],"widgets_values":[]}));
        links.push(json!([id, 1, 0, id, 0, "STRING"]));
    }
    let ui = json!({"nodes":nodes,"links":links});
    let converted = ApiWorkflow::convert(&serde_json::to_vec(&ui).unwrap(), Some(&c)).unwrap();
    assert_eq!(converted.workflow.nodes.len(), 41);
    for id in 2..42 {
        assert_eq!(converted.workflow.nodes[&id.to_string()].inputs["text"], InputValue::Link { node: "1".into(), output: 0 });
    }
    assert!(converted.workflow.check(&c).is_ok());
}

#[test]
fn templates948_primitive_conversion_refuses_oversized_server_type_names() {
    let mut info = raw();
    info["Sink"]["input"]["required"]["image"] = json!([format!("STRING,{}","x".repeat(100_000)),{"forceInput":true}]);
    let ui = json!({"nodes":[
        {"id":1,"type":"PrimitiveNode","outputs":[{"type":"STRING","links":[1]}],"widgets_values":["Own text"]},
        {"id":2,"type":"Sink","inputs":[{"name":"image","type":"STRING","link":1}],"widgets_values":[]}
    ],"links":[[1,1,0,2,0,"STRING"]]});
    let error = ApiWorkflow::convert(&serde_json::to_vec(&ui).unwrap(), Some(&catalog(&info))).unwrap_err();
    assert!(error.to_string().len() < 200);
}
