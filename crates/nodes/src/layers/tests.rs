use super::*;
use crate::{InputValue, Registry};
use nodecraft_types::{NodeId, ParamValues};

fn image() -> PortType {
    PortType { modality: Modality::Image, many: true }
}

#[test]
fn its_plan_carries_the_layers_and_where_each_image_came_from() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(ID).unwrap();
    let mut node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), Default::default(), &ParamValues::default()).unwrap();
    assert_eq!(stored(&node).unwrap(), EMPTY);
    assert!(kind.mode(&[image()]).is_some() && kind.mode(&[]).is_some());
    assert!(kind.mode(&[PortType { modality: Modality::Text, many: false }]).is_none());
    node.config.insert(CONFIG.into(), Value::Text(r#"{"layers":[]}"#.into()));
    let (a, b) = (NodeId::from_bytes([1; 16]), NodeId::from_bytes([2; 16]));
    let input = InputValue {
        port: INPUT.into(),
        port_type: image(),
        value: Value::List(vec![Value::Text("x".into()), Value::Text("y".into()), Value::Text("z".into())]),
        origins: vec![a, b],
    };
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
    let [JobSpec::Action { action, params, inputs, output, .. }] = jobs.as_slice() else { panic!("{jobs:?}") };
    assert_eq!((action.as_str(), output.as_str(), inputs.len()), (ID, "image", 3));
    assert_eq!(params.get(SPEC_PARAM), Some(&Value::Text(EMPTY.into())));
    assert_eq!(params.get(SOURCES_PARAM), Some(&Value::List(vec![Value::Text(a.to_string()), Value::Text(b.to_string()), Value::Text(String::new())])));
    assert_eq!(params.get("width"), Some(&Value::Unsigned(Unsigned(DEFAULT_FRAME))));

    node.config.insert(CONFIG.into(), Value::Bool(true));
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err());
}
