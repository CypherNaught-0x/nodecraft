use nodecraft_nodes::{InputValue, PlanCtx, Registry};
use nodecraft_types::{Modality, NodeId, PortType, Value};

#[test]
fn note_has_no_ports_and_plans_no_work() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get("structure.note").unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
    assert!(node.inputs.is_empty() && node.outputs.is_empty());
    assert_eq!(node.params.get("text"), Some(&Value::Text(String::new())));
    assert!(kind.mode(&[]).is_some());
    assert!(kind.plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &node, inputs: &[] }).unwrap().is_empty());
    let inputs = [InputValue {
        port: "in".into(),
        port_type: PortType { modality: Modality::Text, many: false },
        value: Value::Text("not a wire destination".into()),
        origins: vec![],
    }];
    assert!(kind.mode(&[inputs[0].port_type]).is_none());
    assert!(kind.plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &node, inputs: &inputs }).is_err());
    let mut stale = node;
    stale.params.insert("text".into(), Value::Text("x".repeat(1024 * 1024 + 1)));
    assert!(kind.plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &stale, inputs: &[] }).is_err());
}
