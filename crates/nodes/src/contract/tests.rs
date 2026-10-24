use super::*;
use crate::{Category, Registry};
use nodecraft_graph::Port;
use nodecraft_types::{Modality, NodeId, ParamSchema, PortType};

fn port(name: &str, modality: Modality, many: bool) -> Port {
    Port { name: name.into(), port_type: PortType { modality, many } }
}

/// A video generator whose mode follows its frame inputs.
fn frames() -> (NodeDescriptor, NodeContract) {
    let descriptor = NodeDescriptor {
        id: "test.frames".into(),
        label: "Frames".into(),
        category: Category::Generation,
        inputs: vec![port("prompt", Modality::Text, true), port("start", Modality::Image, false), port("end", Modality::Image, false)],
        outputs: vec![port("out", Modality::Video, false)],
        params: ParamSchema::default(),
        quick_add: None,
    };
    let mut contract = NodeContract::derive(&descriptor);
    contract.inputs[1].role = "start_frame".into();
    contract.inputs[2].role = "end_frame".into();
    let count = |port: &str, min: u16, max: u16| PortCount { port: port.into(), min, max: Some(max) };
    contract.modes = vec![
        ModeContract { mode: ModeId::new("text").unwrap(), inputs: vec![count("prompt", 1, u16::MAX), count("start", 0, 0), count("end", 0, 0)], params: None },
        ModeContract { mode: ModeId::new("first_frame").unwrap(), inputs: vec![count("start", 1, 1), count("end", 0, 0)], params: None },
        ModeContract { mode: ModeId::new("first_last").unwrap(), inputs: vec![count("start", 1, 1), count("end", 1, 1)], params: None },
    ];
    (descriptor, contract)
}

fn wires(pairs: &[(&'static str, usize)]) -> BTreeMap<&'static str, usize> {
    pairs.iter().copied().collect()
}

#[test]
fn every_builtin_kind_has_a_valid_contract_matching_its_schema() {
    let registry = Registry::builtins().unwrap();
    // A technique node has no ports or parameters until it pins a version, nor a ComfyUI
    // workflow card until it stores a workflow; their own tests cover them.
    for descriptor in registry.descriptors().filter(|d| d.id != crate::technique::ID && d.id != crate::comfy_workflow::ID) {
        let contract = registry.contract(&descriptor.id).unwrap();
        assert_eq!(contract.kind, descriptor.id);
        // The contract adds rules but resolves defaults exactly as the schema does.
        let node = registry.get(&descriptor.id).unwrap().instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
        let resolved = contract.params.resolve(&node.params, false).unwrap();
        assert!(resolved.issues.is_empty(), "{}: {:?}", descriptor.id, resolved.issues);
        let schema = descriptor.params.resolve(&node.params, false).unwrap();
        let visible: std::collections::BTreeSet<_> = resolved.params.iter().filter(|state| state.visible).map(|state| state.name.clone()).collect();
        assert_eq!(visible, schema.visible, "{}", descriptor.id);
        let instance = registry.get(&descriptor.id).unwrap().instance_contract(&node).unwrap();
        instance.validate(&registry.get(&descriptor.id).unwrap().resolve_instance(&node).unwrap()).unwrap();
    }
}

#[test]
fn source_contracts_declare_their_single_mode() {
    let registry = Registry::builtins().unwrap();
    let sources: Vec<_> = registry.descriptors().filter(|descriptor| descriptor.category == Category::Source).map(|descriptor| descriptor.id.clone()).collect();
    assert_eq!(sources.len(), 6);
    for id in sources {
        let contract = registry.contract(&id).unwrap();
        let kind = registry.get(&id).unwrap();
        assert_eq!(contract.resolve_mode(&BTreeMap::new(), true).unwrap(), kind.mode(&[]).as_ref(), "{id}");
        assert!(contract.resolve_mode(&wires(&[("in", 1)]), true).is_err(), "{id}: sources take no wires");
        assert_eq!(contract.outputs[0].role, "value");
        for spec in &contract.params.schema.params {
            let viewer = id == crate::mesh::SOURCE && crate::mesh::view::NAMES.contains(&spec.name.as_str());
            assert_eq!(contract.params.rule(&spec.name).unwrap().affects.shape, !viewer, "{id}: {}", spec.name);
        }
        assert!(contract.params.resolve(&Default::default(), true).unwrap().issues.is_empty() == (id == "source.text"), "{id}: media sources need an asset");
    }
}

#[test]
fn derived_contracts_bound_single_ports_to_one_wire() {
    let (descriptor, _) = frames();
    let contract = NodeContract::derive(&descriptor);
    contract.validate(&descriptor).unwrap();
    assert_eq!(contract.inputs[0].max, None);
    assert_eq!(contract.inputs[1].max, Some(1));
    assert!(!contract.inputs[0].required());
    assert_eq!(contract.resolve_mode(&wires(&[("prompt", 40), ("start", 1)]), true).unwrap(), None);
    let error = contract.resolve_mode(&wires(&[("start", 2)]), true).unwrap_err();
    assert_eq!(error.message, "input start takes at most 1 wires");
}

#[test]
fn modes_follow_the_connected_inputs() {
    let (descriptor, contract) = frames();
    contract.validate(&descriptor).unwrap();
    let mode = |pairs: &[(&'static str, usize)]| contract.resolve_mode(&wires(pairs), true).map(|mode| mode.map(|mode| mode.as_str().to_owned()));
    assert_eq!(mode(&[("prompt", 2)]).unwrap().as_deref(), Some("text"));
    assert_eq!(mode(&[("start", 1)]).unwrap().as_deref(), Some("first_frame"));
    assert_eq!(mode(&[("prompt", 1), ("start", 1), ("end", 1)]).unwrap().as_deref(), Some("first_last"));
    assert_eq!(mode(&[("end", 1)]).unwrap_err().kind, ErrorKind::Unsupported);
    assert!(mode(&[("other", 1)]).is_err());
    assert_eq!(contract.resolve_mode(&wires(&[("end", 1)]), false).unwrap(), None, "a node still being wired has no mode yet");

    // Registration rejects overlapping modes; resolving an unvalidated one still names both.
    let mut ambiguous = contract.clone();
    ambiguous.modes[1].inputs.retain(|count| count.port != "end");
    assert!(ambiguous.validate(&descriptor).is_err());
    let error = ambiguous.resolve_mode(&wires(&[("start", 1), ("end", 1)]), true).unwrap_err();
    assert!(error.message.contains("first_frame, first_last"), "{}", error.message);
}

#[test]
fn required_inputs_wait_until_the_node_runs() {
    let (descriptor, mut contract) = frames();
    contract.inputs[0].min = 1;
    contract.validate(&descriptor).unwrap();
    assert!(contract.inputs[0].required());
    assert!(contract.resolve_mode(&wires(&[("start", 1)]), false).is_ok());
    assert_eq!(contract.resolve_mode(&wires(&[("start", 1)]), true).unwrap_err().message, "input prompt needs at least 1 wires");
}

type Breakage = fn(&mut NodeContract);

#[test]
fn impossible_contracts_are_rejected() {
    let cases: Vec<(&str, Breakage)> = vec![
        ("wrong kind", |c| c.kind = "test.other".into()),
        ("wrong namespace", |c| c.params.namespace = "test.other".into()),
        ("schema drift", |c| c.params.schema = Registry::builtins().unwrap().get("source.text").unwrap().descriptor().params.clone()),
        ("ports reordered", |c| c.inputs.swap(1, 2)),
        ("port missing", |c| {
            c.outputs.clear();
        }),
        ("two wires on a single port", |c| c.inputs[1].max = Some(2)),
        ("unbounded single port", |c| c.inputs[1].max = None),
        ("min above max", |c| {
            c.inputs[0].min = 3;
            c.inputs[0].max = Some(2);
        }),
        ("zero max", |c| c.inputs[0].max = Some(0)),
        ("bad role", |c| c.inputs[0].role = "Prompt Text".into()),
        ("shared role", |c| c.inputs[2].role = c.inputs[1].role.clone()),
        ("collection mismatch", |c| c.outputs[0].collection = true),
        ("duplicate mode", |c| c.modes.push(c.modes[0].clone())),
        ("unknown mode port", |c| c.modes[0].inputs.push(PortCount { port: "mask".into(), min: 0, max: None })),
        ("mode port twice", |c| c.modes[1].inputs.push(PortCount { port: "start".into(), min: 1, max: Some(1) })),
        ("mode exceeds port", |c| c.modes[1].inputs[0].min = 2),
        ("newer major", |c| c.version.major = 2),
    ];
    for (case, mutate) in cases {
        let (descriptor, mut contract) = frames();
        mutate(&mut contract);
        assert!(contract.validate(&descriptor).is_err(), "{case} should be rejected");
    }
}

#[test]
fn contracts_round_trip_and_unknown_fields_are_rejected() {
    let (_, contract) = frames();
    let json = serde_json::to_value(&contract).unwrap();
    assert_eq!(serde_json::from_value::<NodeContract>(json.clone()).unwrap(), contract);
    let mut extra = json;
    extra["surprise"] = serde_json::json!(1);
    assert!(serde_json::from_value::<NodeContract>(extra).is_err());
}

#[test]
fn only_a_list_input_may_fan_out() {
    let (descriptor, mut contract) = frames();
    contract.inputs[0].fans_out = true;
    assert!(contract.validate(&descriptor).is_ok(), "the prompt list may fan out");
    contract.inputs[1].fans_out = true;
    let error = contract.validate(&descriptor).unwrap_err();
    assert!(error.message.contains("only a list input"), "{}", error.message);
}
