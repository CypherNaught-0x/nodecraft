use super::*;
use crate::{InputValue, Registry};
use nodecraft_comfy::{NodeCatalog, discover};
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, ProviderTarget, TargetSelection};

const CARD: NodeId = NodeId::from_bytes([5; 16]);
const OBJECT_INFO: &str = r#"{
  "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
  "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["CONDITIONING"]},
  "KSampler": {"input": {"required": {
    "positive": ["CONDITIONING"], "latent_image": ["LATENT"],
    "seed": ["INT", {"min": 0, "max": 18446744073709551615}], "steps": ["INT", {"min": 1, "max": 10000, "default": 20}],
    "cfg": ["FLOAT", {"min": 0.0, "max": 100.0, "step": 0.1}], "sampler_name": [["euler", "dpmpp_2m"]]
  }}, "output": ["LATENT"]},
  "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
  "PreviewImage": {"input": {"required": {"images": ["IMAGE", {}]}}, "output": [], "output_node": true}
}"#;
const FOX: &str = r#"{
  "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
  "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
  "3": {"class_type": "KSampler", "inputs": {"positive": ["2", 0], "latent_image": ["1", 0], "seed": 7, "steps": 20, "cfg": 3.5, "sampler_name": "euler"}},
  "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "nc"}}
}"#;

fn catalog() -> NodeCatalog {
    NodeCatalog::parse(OBJECT_INFO.as_bytes()).unwrap().0
}

fn workflow(json: &str) -> ApiWorkflow {
    ApiWorkflow::parse(json.as_bytes(), Some(&catalog())).unwrap()
}

/// The default proposal: file inputs, prompts and seeds.
fn exposed(workflow: &ApiWorkflow) -> Exposed {
    let mut exposed = discover(workflow, &catalog()).unwrap();
    exposed.inputs.retain(|i| i.role.exposed_by_default());
    exposed
}

fn config() -> BTreeMap<String, Value> {
    let workflow = workflow(FOX);
    propose(&workflow, &exposed(&workflow)).unwrap()
}

fn place(config: BTreeMap<String, Value>) -> Result<Node, Error> {
    Registry::builtins().unwrap().get(ID).unwrap().instantiate_instance(CARD, config, &Default::default())
}

fn image(mime: &str) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([3; 32]), mime: mime.parse().unwrap(), meta: MediaMeta::default() })
}

fn targets() -> TargetSelection {
    let mut targets = TargetSelection::default();
    targets.insert(CARD, &ProviderTarget::default_instance(PROVIDER).unwrap()).unwrap();
    targets
}

fn plan(node: &Node, inputs: &[InputValue]) -> Result<Vec<JobSpec>, Error> {
    let targets = targets();
    kind().plan(&PlanCtx { node, inputs, targets: &targets })
}

fn wired(port: &str, value: Value) -> InputValue {
    InputValue { port: port.into(), port_type: PortType { modality: Modality::Image, many: false }, value, origins: vec![] }
}

#[test]
fn a_proposed_card_takes_its_ports_and_parameters_from_the_workflow() {
    let node = place(config()).unwrap();
    let mapping = mapping(&node).unwrap();
    assert_eq!(mapping.inputs.len(), 1);
    let input = mapping.inputs.first().unwrap();
    assert_eq!((input.node.as_str(), input.input.as_str(), input.modality), ("1", "image", Modality::Image));
    assert_eq!(
        node.inputs.iter().map(|p| (p.name.clone(), p.port_type)).collect::<Vec<_>>(),
        vec![(input.port.clone(), PortType { modality: Modality::Image, many: false })]
    );
    assert_eq!(
        node.outputs.iter().map(|p| (p.name.as_str(), p.port_type)).collect::<Vec<_>>(),
        vec![("image", PortType { modality: Modality::Image, many: true })]
    );

    let targets: Vec<_> = mapping.params.iter().map(|p| (p.node.as_str(), p.input.as_str())).collect();
    assert_eq!(targets, vec![("2", "text"), ("3", "seed")]);
    let prompt = &mapping.params.first().unwrap().spec;
    assert!(matches!(prompt.control, ParamControl::Text { multiline: true, .. }));
    assert_eq!(node.params.get(&prompt.name), Some(&Value::Text("a red fox".into())), "defaults to the workflow's value");
    let seed = &mapping.params.get(1).unwrap().spec;
    assert!(matches!(seed.control, ParamControl::Seed { .. }));
    assert_eq!(node.params.get(&seed.name), Some(&Value::Unsigned(Unsigned(7))));
    assert_eq!(node.params.get(MODEL), Some(&Value::Text(MODEL_ID.into())), "the run picker routes by the stored model");

    let kind = kind();
    kind.validate_instance(&node, true).unwrap();
    assert!(kind.instance_mode(&node, &[]).unwrap().is_some());
    assert!(kind.mode(&[PortType { modality: Modality::Image, many: false }]).is_none(), "never offered by Turn into");
}

#[test]
fn literal_inputs_get_matching_controls() {
    let workflow = workflow(FOX);
    let mut exposed = discover(&workflow, &catalog()).unwrap();
    exposed.inputs.retain(|i| i.node == "3" && i.input != "seed");
    let node = place(propose(&workflow, &exposed).unwrap()).unwrap();
    let mapping = mapping(&node).unwrap();
    let control = |input: &str| mapping.params.iter().find(|p| p.input == input).map(|p| p.spec.control.clone()).unwrap();
    assert!(matches!(control("steps"), ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(10000)), .. }));
    assert!(matches!(control("cfg"), ParamControl::Slider { .. }));
    assert!(matches!(control("sampler_name"), ParamControl::Choice { ref options } if options.len() == 2));
    let cfg = mapping.params.iter().find(|p| p.input == "cfg").unwrap();
    assert_eq!(node.params.get(&cfg.spec.name), Some(&Value::Number(Number::new(3.5).unwrap())));
}

#[test]
fn its_plan_sends_the_workflow_values_and_files_to_comfyui() {
    let mut node = place(config()).unwrap();
    let mapping = mapping(&node).unwrap();
    let prompt = mapping.params.first().unwrap().spec.name.clone();
    node.params.insert(prompt, Value::Text("a blue whale".into()));
    let port = mapping.inputs.first().unwrap().port.clone();
    let jobs = plan(&node, &[wired(&port, image("image/png"))]).unwrap();
    let [JobSpec::Generate { node: id, outputs, provider, instance, model, params, inputs }] = jobs.as_slice() else { panic!("one generation job: {jobs:?}") };
    assert_eq!((*id, provider.as_str(), *instance, model.as_str()), (CARD, PROVIDER, None, REMOTE_MODEL));
    assert!(params.is_empty(), "the free workflow model takes no parameters");
    assert_eq!(outputs, &vec![GenerateOutput { port: "image".into(), modality: Modality::Image, style_family: None }]);
    let roles: Vec<_> = inputs.iter().map(|i| i.role.as_str()).collect();
    assert_eq!(roles, vec![WORKFLOW_ROLE, VALUES_ROLE, "1.image", OUTPUTS_ROLE]);
    assert_eq!(inputs.get(3).map(|i| &i.value), Some(&Value::Text(r#"["4"]"#.into())), "only the mapped saver's files come back");
    let Some(Value::Text(text)) = inputs.first().map(|i| &i.value) else { panic!("workflow text") };
    assert_eq!(ApiWorkflow::parse(text.as_bytes(), None).unwrap().nodes.len(), 4);
    let Some(Value::Text(values)) = inputs.get(1).map(|i| &i.value) else { panic!("values text") };
    let values: serde_json::Value = serde_json::from_str(values).unwrap();
    assert_eq!(values, serde_json::json!({"2.text": "a blue whale", "3.seed": 7}));
    assert_eq!(inputs.get(2).map(|i| &i.value), Some(&image("image/png")));
}

#[test]
fn its_plan_refuses_wrong_files_models_and_routes() {
    let node = place(config()).unwrap();
    let port = mapping(&node).unwrap().inputs.first().unwrap().port.clone();
    assert_eq!(plan(&node, &[wired(&port, image("audio/wav"))]).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(plan(&node, &[wired(&port, Value::Text("x".into()))]).unwrap_err().kind, ErrorKind::InvalidInput);
    let two = Value::List(vec![image("image/png"), image("image/png")]);
    assert_eq!(plan(&node, &[wired(&port, two)]).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(plan(&node, &[wired("elsewhere", image("image/png"))]).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(plan(&node, &[]).unwrap_err().message.contains("connect a file"), "an unwired input never runs on the author's file");
    let no_route = kind().plan(&PlanCtx { node: &node, inputs: &[wired(&port, image("image/png"))], targets: &nodecraft_types::EMPTY_TARGET_SELECTION });
    assert_eq!(no_route.unwrap_err().kind, ErrorKind::Unsupported);
    let mut other = node.clone();
    other.params.insert(MODEL.into(), Value::Text("comfyui.flux-dev".into()));
    assert_eq!(plan(&other, &[]).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn a_missing_or_malformed_workflow_is_an_error() {
    assert!(place(BTreeMap::new()).is_err());
    let mut newer = config();
    newer.insert(VERSION.into(), Value::Unsigned(Unsigned(2)));
    assert_eq!(place(newer).unwrap_err().kind, ErrorKind::Unsupported);
    let mut extra = config();
    extra.insert(MAPPING.into(), Value::Text(r#"{"inputs":[],"params":[],"outputs":[],"extra":1}"#.into()));
    assert!(place(extra).is_err(), "unknown mapping fields are refused");
    let mut huge = config();
    huge.insert(WORKFLOW.into(), Value::Text("x".repeat(MAX_WORKFLOW_BYTES + 1)));
    let node = place(huge).unwrap();
    assert!(plan(&node, &[]).is_err());
    let mut garbage = config();
    garbage.insert(WORKFLOW.into(), Value::Text("{not json".into()));
    assert!(plan(&place(garbage).unwrap(), &[]).is_err());
    let mut missing = config();
    missing.insert(WORKFLOW.into(), Value::Text(r#"{"9": {"class_type": "SaveImage", "inputs": {}}}"#.into()));
    assert_eq!(plan(&place(missing).unwrap(), &[]).unwrap_err().kind, ErrorKind::InvalidInput, "mapped nodes must exist");
}

#[test]
fn a_mapping_refuses_duplicates_reserved_names_and_non_media() {
    let mut mapping = mapping(&place(config()).unwrap()).unwrap();
    mapping.check().unwrap();
    let mut twice = mapping.clone();
    twice.params.push(twice.params.first().unwrap().clone());
    assert!(twice.check().is_err());
    let mut model = mapping.clone();
    model.params.first_mut().unwrap().spec.name = MODEL.into();
    assert!(model.check().is_err());
    let mut text = mapping.clone();
    text.inputs.first_mut().unwrap().modality = Modality::Text;
    assert!(text.check().is_err());
    let mut dotted = mapping.clone();
    dotted.inputs.first_mut().unwrap().node = "1.2".into();
    assert!(dotted.check().is_err(), "a dot would make the target ambiguous");
    mapping.outputs.clear();
    assert!(mapping.check().is_err());
}

#[test]
fn a_workflow_that_saves_nothing_is_not_proposed() {
    let preview = r#"{
      "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
      "2": {"class_type": "PreviewImage", "inputs": {"images": ["1", 0]}}
    }"#;
    let workflow = workflow(preview);
    assert_eq!(propose(&workflow, &exposed(&workflow)).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn the_card_and_its_mapping_round_trip_through_serde() {
    let node = place(config()).unwrap();
    let mapping = mapping(&node).unwrap();
    let json = serde_json::to_string(&mapping).unwrap();
    assert_eq!(serde_json::from_str::<Mapping>(&json).unwrap(), mapping);
    let saved = serde_json::to_string(&node).unwrap();
    let loaded: Node = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded, node);
    assert_eq!(kind().instance(&loaded).unwrap().into_owned(), kind().instance(&node).unwrap().into_owned());
}

#[test]
fn long_labels_are_cut_at_a_character_boundary() {
    let label = "é".repeat(200);
    let cut = bounded(&label, 255);
    assert_eq!(cut.len(), 254);
    assert!(cut.chars().all(|c| c == 'é'));
}
