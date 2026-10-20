use nodecraft_nodes::{
    ModeId, NodeKind,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{Contract, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, Value};

#[test]
fn a_generation_fans_out_over_its_prompt_and_collects_its_references() {
    let role = |role: &str, modality, min, max| GenerationRole { role: role.into(), modality, min, max, inline: false };
    let kind = GenerationKind::new(
        vec![GenerationModel {
            id: "fixture/model".into(),
            provider: "fal".into(),
            remote: "remote".into(),
            parameters: Contract::from_schema("fixture/model", ParamOwner::Model, ParamSchema::default()),
            modes: vec![GenerationMode {
                id: ModeId::new("image-to-image").unwrap(),
                inputs: vec![role("prompt", Modality::Text, 1, 1), role("image_reference", Modality::Image, 1, 4)],
            }],
        }],
        [1; 32],
    )
    .unwrap();
    let supplied: ParamValues = [("model".into(), Value::Text("fixture/model".into()))].into();
    let node = kind.instantiate_instance(NodeId::from_bytes([51; 16]), Default::default(), &supplied).unwrap();
    let contract = kind.instance_contract(&node).unwrap();
    let fans: Vec<(&str, bool)> = contract.inputs.iter().map(|input| (input.port.as_str(), input.fans_out)).collect();
    assert_eq!(fans, [("prompt", true), ("image_reference", false)]);
}
