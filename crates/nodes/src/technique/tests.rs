use super::*;
use crate::InputValue;
use nodecraft_graph::{Edge, Endpoint, Technique, TechniqueParam, TechniquePort};
use nodecraft_types::{EdgeId, Modality, NodeId};

const BRIEF: NodeId = NodeId::from_bytes([1; 16]);
const REPLACE: NodeId = NodeId::from_bytes([2; 16]);
const VERSION: CanvasId = CanvasId::from_bytes([9; 16]);

fn text() -> PortType {
    PortType { modality: Modality::Text, many: false }
}

/// Brief (a text input) → Replace text, whose Find is exposed and whose result is the output.
fn version(kinds: &Registry) -> Canvas {
    let mut brief = kinds.get("source.text").unwrap().instantiate_instance(BRIEF, Default::default(), &Default::default()).unwrap();
    brief.params.insert("text".into(), Value::Text("a cat".into()));
    let mut replace = kinds.get("text.replace").unwrap().instantiate_instance(REPLACE, Default::default(), &Default::default()).unwrap();
    replace.params.insert("find".into(), Value::Text("cat".into()));
    let edge = Edge {
        id: EdgeId::from_bytes([3; 16]),
        from: Endpoint { node: BRIEF, port: "out".into() },
        to: Endpoint { node: REPLACE, port: "text".into() },
        order: 0,
    };
    let technique = Technique {
        id: TechniqueId::from_bytes([7; 16]),
        version: 2,
        name: "Swap".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![TechniquePort { name: "Brief text".into(), description: String::new(), node: BRIEF, port: "out".into(), port_type: text() }],
        outputs: vec![TechniquePort { name: "Swapped".into(), description: String::new(), node: REPLACE, port: "out".into(), port_type: text() }],
        params: vec![TechniqueParam { name: "Find".into(), description: String::new(), node: REPLACE, param: "find".into() }],
        source: None,
        retired: false,
    };
    Canvas::technique(VERSION, "Swap v2", technique, vec![brief, replace], vec![edge]).unwrap()
}

#[test]
fn a_placed_node_takes_its_ports_and_parameters_from_the_version() {
    let kinds = Registry::builtins().unwrap();
    let interface = interface_of(&version(&kinds), &kinds).unwrap();
    assert_eq!((interface.version, interface.canvas), (2, VERSION));
    assert_eq!(interface.inputs, vec![Exposed { port: "brief_text".into(), name: "Brief text".into(), port_type: text() }]);
    assert_eq!(interface.outputs.first().map(|o| o.port.as_str()), Some("swapped"));
    let find = interface.params.first().unwrap();
    assert_eq!((find.name.as_str(), find.label.as_str(), find.default.clone()), ("find", "Find", Some(Value::Text("cat".into()))));

    let kind = kinds.get(ID).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), interface.config().unwrap(), &Default::default()).unwrap();
    assert_eq!(node.params.get("find"), Some(&Value::Text("cat".into())));
    assert_eq!(node.inputs.first().map(|p| p.name.as_str()), Some("brief_text"));
    kind.validate_instance(&node, true).unwrap();
    assert!(kind.instance_mode(&node, &[text()]).unwrap().is_some());
    assert!(kind.mode(&[text()]).is_none(), "never offered by Turn into");
    assert!(interface_of(&version(&kinds), &kinds).unwrap().matches(&interface));
}

#[test]
fn its_plan_gives_back_the_inlined_results_and_refuses_without_them() {
    let kinds = Registry::builtins().unwrap();
    let interface = interface_of(&version(&kinds), &kinds).unwrap();
    let kind = kinds.get(ID).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), interface.config().unwrap(), &Default::default()).unwrap();
    let ctx = |inputs: &[InputValue]| kind.plan(&PlanCtx { node: &node, inputs, targets: &nodecraft_types::EMPTY_TARGET_SELECTION });
    assert_eq!(ctx(&[]).unwrap_err().kind, ErrorKind::Unsupported);
    let inlined = InputValue { port: format!("{OUTPUT_PREFIX}swapped"), port_type: text(), value: Value::Text("a dog".into()) };
    assert_eq!(ctx(&[inlined]).unwrap(), vec![JobSpec::Source { node: node.id, output: "swapped".into(), value: Value::Text("a dog".into()) }]);
}

#[test]
fn a_malformed_interface_is_an_error() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(ID).unwrap();
    assert!(kind.instantiate_instance(NodeId::from_bytes([5; 16]), Default::default(), &Default::default()).is_err());
    let config = [(CONFIG.to_owned(), Value::Text("{\"technique\": 3}".into()))].into_iter().collect();
    assert!(kind.instantiate_instance(NodeId::from_bytes([5; 16]), config, &Default::default()).is_err());
    let config = [(CONFIG.to_owned(), Value::Bool(true))].into_iter().collect();
    assert!(kind.instantiate_instance(NodeId::from_bytes([5; 16]), config, &Default::default()).is_err());
}

#[test]
fn slugs_are_identifiers_and_unique() {
    let mut taken = BTreeSet::new();
    assert_eq!(slug("Hero image", &mut taken), "hero_image");
    assert_eq!(slug("Hero  image!", &mut taken), "hero_image_2");
    assert_eq!(slug("ÄÖÜ", &mut taken), "port");
    assert_eq!(slug("--", &mut taken), "port_2");
    assert!(crate::descriptor::identifier(&slug(&"x".repeat(500), &mut taken)));
}
