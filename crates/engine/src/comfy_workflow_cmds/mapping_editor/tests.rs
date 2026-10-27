use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Project};
use nodecraft_types::{EdgeId, GenerationId, ProjectId};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
const INFO: &str = r#"{
 "LoadImage":{"input":{"required":{"image":[["front.png"],{"image_upload":true}]}},"output":["IMAGE"]},
 "FourViewModel":{"input":{"required":{"front":["IMAGE",{}],"back":["IMAGE",{}],"prompt":["STRING",{}],"reference_data":["STRING",{}],"seed":["INT",{"min":0,"max":18446744073709551615}],"steps":["INT",{"min":1,"max":100,"default":20}],"cfg":["FLOAT",{"min":0.0,"max":20.0}]}},"output":["MESH","IMAGE"]},
 "PreviewImage":{"input":{"required":{"images":["IMAGE",{}]}},"output":[],"output_node":true},
 "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true},
 "SaveGLB":{"input":{"required":{"mesh":["MESH",{}]}},"output":[],"output_node":true}
}"#;
const FLOW: &str = r#"{
 "1":{"class_type":"LoadImage","inputs":{"image":"front.png"},"_meta":{"title":"Front reference"}},
 "2":{"class_type":"LoadImage","inputs":{"image":"back.png"},"_meta":{"title":"Back reference"}},
 "10":{"class_type":"FourViewModel","inputs":{"front":["1",0],"back":["2",0],"prompt":"a clay model","reference_data":"{}","seed":7,"steps":20,"cfg":4.0}},
 "20":{"class_type":"PreviewImage","inputs":{"images":["10",1]},"_meta":{"title":"Base colour"}},
 "21":{"class_type":"PreviewImage","inputs":{"images":["10",1]},"_meta":{"title":"Normal map"}},
 "22":{"class_type":"SaveGLB","inputs":{"mesh":["10",0]},"_meta":{"title":"Mesh"}}
}"#;
fn ready() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([94; 16]), "Mapping fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    install(&mut session);
    session.dispatch("comfy.workflow.import", &json!({"canvas":C,"id":N,"pos":{"x":0,"y":0},"workflow":FLOW})).unwrap();
    session
}
fn install(s: &mut Session) {
    super::super::finish(s, 1, Ok(Arc::new(NodeCatalog::parse(INFO.as_bytes()).unwrap().0).into()));
}
fn node(s: &Session) -> &Node {
    &s.document().canvases()[&C].nodes()[&N]
}
fn request(s: &mut Session) -> Value {
    let listed = s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":N})).unwrap();
    json!({"canvas":C,"id":N,"binding":listed["binding"],
      "inputs":listed["inputs"].as_array().unwrap().iter().filter(|i|i["selected"]==true).map(|i|json!({"node":i["node"],"input":i["input"],"name":i["name"]})).collect::<Vec<_>>(),
      "outputs":listed["outputs"].as_array().unwrap().iter().filter(|i|i["selected"]==true).map(|i|json!({"node":i["node"],"label":i["label"]})).collect::<Vec<_>>()})
}
#[test]
fn mapping947_multiview_defaults_include_media_prompt_controls_and_retained_pbr_images() {
    let mut s = ready();
    let mapping = card::mapping(node(&s)).unwrap();
    assert_eq!(mapping.inputs.len(), 2);
    let inputs = mapping.params.iter().map(|p| p.input.as_str()).collect::<BTreeSet<_>>();
    assert_eq!(inputs, ["prompt", "seed", "steps", "cfg"].into());
    assert_eq!(node(&s).outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["image", "mesh"]);
    let api: Value = serde_json::from_str(&card::card(node(&s)).unwrap().workflow).unwrap();
    assert_eq!(api["20"]["class_type"], "SaveImage");
    assert_eq!(api["21"]["class_type"], "SaveImage");
    let listed = s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":N})).unwrap();
    assert!(listed["inputs"].as_array().unwrap().iter().any(|i| i["input"] == "reference_data" && i["selected"] == false));
}
#[test]
fn mapping947_hide_show_rename_and_order_survive_save_reload_and_keep_current_hidden_values() {
    let mut s = ready();
    let mapping = card::mapping(node(&s)).unwrap();
    let seed = mapping.params.iter().find(|p| p.input == "seed").unwrap().spec.name.clone();
    let mut edit = request(&mut s);
    // A current value edit and an unrelated edit while the modal is open must survive Apply.
    s.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":seed,"value":Typed::Unsigned(nodecraft_types::Unsigned(u64::MAX))})).unwrap();
    s.dispatch("node.create", &json!({"canvas":C,"id":NodeId::from_bytes([77;16]),"kind":"source.text","pos":{"x":500,"y":0}})).unwrap();
    let inputs = edit["inputs"].as_array_mut().unwrap();
    inputs.retain(|i| i["input"] != "seed");
    inputs.reverse();
    inputs.push(json!({"node":"10","input":"reference_data","name":"Reference JSON"}));
    edit["outputs"] = json!([{"node":"22","label":"Model"},{"node":"21","label":"Normal"}]);
    edit["output_names"] = json!([{"modality":"image","name":"textures"}]);
    s.dispatch("comfy.workflow.mapping_set", &edit).unwrap();
    let mapped = card::mapping(node(&s)).unwrap();
    assert!(!mapped.params.iter().any(|p| p.input == "seed"));
    assert_eq!(mapped.params.last().unwrap().spec.label, "Reference JSON");
    assert_eq!(node(&s).outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["mesh", "textures"]);
    let api: Value = serde_json::from_str(&card::card(node(&s)).unwrap().workflow).unwrap();
    assert_eq!(api["10"]["inputs"]["seed"], u64::MAX);
    let bytes = nodecraft_format::encode(&nodecraft_format::Snapshot { project: s.document().clone(), assets: s.assets().clone(), epoch: 0 }).unwrap();
    let snapshot = nodecraft_format::decode(&bytes).unwrap();
    let mut loaded = Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
    install(&mut loaded);
    assert_eq!(card::mapping(node(&loaded)).unwrap(), mapped);
    let mut restore = request(&mut loaded);
    restore["inputs"].as_array_mut().unwrap().push(json!({"node":"10","input":"seed","name":"Seed again"}));
    loaded.dispatch("comfy.workflow.mapping_set", &restore).unwrap();
    let mapped = card::mapping(node(&loaded)).unwrap();
    let seed = mapped.params.iter().find(|p| p.input == "seed").unwrap();
    assert_eq!(node(&loaded).params[&seed.spec.name], Typed::Unsigned(nodecraft_types::Unsigned(u64::MAX)));
}
#[test]
fn mapping947_renaming_ports_reconnects_wires_and_hidden_wires_need_explicit_disconnection() {
    let mut s = ready();
    let source = NodeId::from_bytes([3; 16]);
    s.dispatch("node.create", &json!({"canvas":C,"id":source,"kind":"source.image","pos":{"x":-100,"y":0}})).unwrap();
    let old = card::mapping(node(&s)).unwrap();
    let port = &old.inputs[0].port;
    let edge = EdgeId::from_bytes([4; 16]);
    s.dispatch("edge.connect", &json!({"canvas":C,"edge":{"id":edge,"from":{"node":source,"port":"out"},"to":{"node":N,"port":port},"order":0}})).unwrap();
    let before = s.document().clone();
    let mut edit = request(&mut s);
    edit["inputs"][0]["name"] = json!("front_photo");
    s.dispatch("comfy.workflow.mapping_set", &edit).unwrap();
    assert_eq!(s.document().canvases()[&C].edges()[&edge].to.port, "front_photo");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(*s.document(), before);
    let mut edit = request(&mut s);
    edit["inputs"].as_array_mut().unwrap().retain(|i| !(i["node"] == "1" && i["input"] == "image"));
    assert!(s.dispatch("comfy.workflow.mapping_set", &edit).unwrap_err().message.contains("disconnect"));
    assert_eq!(*s.document(), before);
    edit["disconnect"] = json!(true);
    let result = s.dispatch("comfy.workflow.mapping_set", &edit).unwrap();
    assert_eq!(result["disconnected"], 1);
    assert!(!s.document().canvases()[&C].edges().contains_key(&edge));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(*s.document(), before);
}
#[test]
fn mapping947_stale_duplicate_invalid_and_history_changes_are_atomic() {
    let mut s = ready();
    let mut edit = request(&mut s);
    let before = s.document().clone();
    let duplicate = edit["inputs"][0].clone();
    edit["inputs"].as_array_mut().unwrap().push(duplicate);
    assert!(s.dispatch("comfy.workflow.mapping_set", &edit).is_err());
    assert_eq!(*s.document(), before);
    let mut edit = request(&mut s);
    edit["output_names"] = json!([{"modality":"image","name":"invalid name"}]);
    assert!(s.dispatch("comfy.workflow.mapping_set", &edit).is_err());
    assert_eq!(*s.document(), before);
    let mut edit = request(&mut s);
    edit["binding"] = json!("0".repeat(64));
    assert!(s.dispatch("comfy.workflow.mapping_set", &edit).is_err());
    let mut project = s.document().clone();
    let mut with_history = node(&s).clone();
    with_history.history.push(nodecraft_graph::Generation::new(GenerationId::from_bytes([9; 16]), vec![Typed::List(Vec::new()), Typed::List(Vec::new())]));
    Change::SetNode { canvas: C, node: with_history }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    install(&mut s);
    let before = s.document().clone();
    let mut edit = request(&mut s);
    edit["outputs"] = json!([{"node":"20","label":"Image"}]);
    assert!(s.dispatch("comfy.workflow.mapping_set", &edit).unwrap_err().message.contains("clear_history"));
    assert_eq!(*s.document(), before);
    edit["clear_history"] = json!(true);
    s.dispatch("comfy.workflow.mapping_set", &edit).unwrap();
    assert!(node(&s).history.is_empty());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(*s.document(), before);
}
#[test]
fn mapping947_named_server_catalog_is_required_and_is_forgotten_when_changed() {
    let mut s = ready();
    s.dispatch("comfy.server.use", &json!({"canvas":C,"id":N,"server":"studio"})).unwrap();
    assert!(s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":N})).is_err());
    crate::comfy_server_cmds::finish(&mut s, 2, "studio", false, Ok(Arc::new(NodeCatalog::parse(INFO.as_bytes()).unwrap().0).into()));
    assert!(s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":N})).is_ok());
    crate::comfy_server_cmds::forget(&mut s, "studio", false);
    assert!(s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":N})).is_err());
}

#[test]
fn mapping947_noop_is_read_only_and_known_partner_evidence_cannot_be_hidden() {
    let mut s = ready();
    let edit = request(&mut s);
    let before = s.document().clone();
    assert_eq!(s.dispatch("comfy.workflow.mapping_set", &edit).unwrap()["changed"], false);
    assert_eq!(*s.document(), before);
    let mut project = s.document().clone();
    let mut paid = node(&s).clone();
    let mut data = card::card(&paid).unwrap();
    data.mapping.partners = vec!["FourViewModel".into()];
    paid.config.extend(data.config().unwrap());
    Change::SetNode { canvas: C, node: paid }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    install(&mut s);
    let mut edit = request(&mut s);
    edit["inputs"].as_array_mut().unwrap().retain(|i| i["input"] != "seed");
    s.dispatch("comfy.workflow.mapping_set", &edit).unwrap();
    assert!(card::mapping(node(&s)).unwrap().partners.contains(&"FourViewModel".into()));
    assert!(s.kinds().get(card::ID).unwrap().unpriced(node(&s)).is_some());
}

#[test]
fn templates948_hidden_nested_media_can_be_shown_again_with_a_valid_default_port_name() {
    let mut s = ready();
    let id = NodeId::from_bytes([42; 16]);
    let workflow = json!({"nodes":[{"id":42,"type":"bundle"}],"links":[],"definitions":{"subgraphs":[{
        "id":"bundle","inputNode":{"id":-10},"outputNode":{"id":-20},"inputs":[],"outputs":[],
        "nodes":[{"id":1,"type":"LoadImage","outputs":[{"type":"IMAGE","links":[1]}],"widgets_values":["front.png","image"]},
        {"id":2,"type":"SaveImage","inputs":[{"name":"images","type":"IMAGE","link":1}],"widgets_values":["NodeCraft"]}],
        "links":[[1,1,0,2,0,"IMAGE"]]
    }]}});
    s.dispatch("comfy.workflow.import", &json!({"canvas":C,"id":id,"pos":{"x":500,"y":0},"workflow":workflow.to_string()})).unwrap();
    let mut listed = s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":id})).unwrap();
    let outputs = listed["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["selected"] == true)
        .map(|o| json!({"node":o["node"],"label":o["label"]}))
        .collect::<Vec<_>>();
    s.dispatch("comfy.workflow.mapping_set", &json!({"canvas":C,"id":id,"binding":listed["binding"],"inputs":[],"outputs":outputs})).unwrap();
    listed = s.dispatch("comfy.workflow.mapping", &json!({"canvas":C,"id":id})).unwrap();
    let input = &listed["inputs"][0];
    let name = input["name"].as_str().unwrap();
    assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
    s.dispatch(
        "comfy.workflow.mapping_set",
        &json!({"canvas":C,"id":id,"binding":listed["binding"],"inputs":[{"node":input["node"],"input":input["input"],"name":name}],"outputs":outputs}),
    )
    .unwrap();
    assert_eq!(card::mapping(&s.document().canvases()[&C].nodes()[&id]).unwrap().inputs.len(), 1);
}
