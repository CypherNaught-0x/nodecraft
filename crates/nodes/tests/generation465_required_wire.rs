// Add as an isolated test against the frozen465 types/nodes source; no network needed.
use nodecraft_nodes::{
    ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{Contract, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, ProviderTarget, TargetSelection, Value};
#[test]
fn non_inline_required_prompt_must_not_run_from_local_text_only() {
    let id = NodeId::from_bytes([46; 16]);
    let kind = GenerationKind::new(
        vec![GenerationModel {
            style_family: None,
            id: "fixture/model".into(),
            provider: "fal".into(),
            remote: "remote".into(),
            parameters: Contract::from_schema("fixture/model", ParamOwner::Model, ParamSchema::default()),
            modes: vec![GenerationMode {
                id: ModeId::new("text-to-image").unwrap(),
                inputs: vec![GenerationRole { role: "prompt".into(), modality: Modality::Text, min: 1, max: 1, inline: false }],
                params: None,
                audio: None,
            }],
        }],
        [1; 32],
    )
    .unwrap();
    let supplied: ParamValues =
        [("model".into(), Value::Text("fixture/model".into())), ("prompt".into(), Value::Text("local text is not an admitted inline role".into()))].into();
    let node = kind.instantiate_instance(id, Default::default(), &supplied).unwrap();
    let contract = kind.instance_contract(&node).unwrap();
    assert!(contract.resolve_mode(&Default::default(), true).is_err());
    let mut targets = TargetSelection::empty();
    targets.insert(id, &ProviderTarget::default_instance("fal").unwrap()).unwrap();
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[], targets: &targets }).is_err(), "must refuse missing required wire before Generate");
    let connected = [nodecraft_nodes::InputValue {
        port: "prompt".into(),
        port_type: nodecraft_types::PortType { modality: Modality::Text, many: false },
        value: Value::Text("actual connected text".into()),
        origins: vec![],
    }];
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &connected, targets: &targets }).unwrap();
    let nodecraft_nodes::JobSpec::Generate { inputs, .. } = &jobs[0] else { panic!() };
    assert_eq!(inputs[0].value, Value::Text("actual connected text".into()));
    let empty = [nodecraft_nodes::InputValue {
        port: "prompt".into(),
        port_type: nodecraft_types::PortType { modality: Modality::Text, many: true },
        value: Value::List(vec![]),
        origins: vec![],
    }];
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &empty, targets: &targets }).is_err(), "empty wired data must not fall back to disallowed local text");
}
