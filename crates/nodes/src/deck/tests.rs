use super::*;
use crate::{InputValue, Registry};
use nodecraft_types::{Modality, NodeId, ParamValues};

#[test]
fn a_deck_holds_slides_and_never_runs() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(ID).unwrap();
    let mut node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), Default::default(), &ParamValues::default()).unwrap();
    assert_eq!(stored(&node).unwrap(), EMPTY);
    assert_eq!(node.params.get("width"), Some(&Value::Unsigned(Unsigned(1920))));
    assert!(kind.mode(&[]).is_some());
    assert!(kind.mode(&[PortType { modality: Modality::Image, many: false }]).is_none());
    let none = PlanCtx { node: &node, inputs: &[], targets: &nodecraft_types::EMPTY_TARGET_SELECTION };
    assert!(kind.plan(&none).unwrap().is_empty());
    let input =
        InputValue { port: "in".into(), port_type: PortType { modality: Modality::Text, many: false }, value: Value::Text("x".into()), origins: vec![] };
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err());
    node.config.insert(CONFIG.into(), Value::Bool(true));
    assert!(stored(&node).is_err());
}
