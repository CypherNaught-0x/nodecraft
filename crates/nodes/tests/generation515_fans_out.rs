use nodecraft_nodes::{
    ModeId, NodeKind,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{Contract, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, Value};

#[test]
fn text_and_media_generations_fan_out_over_their_prompt() {
    let role = |role: &str, modality, min, max| GenerationRole { role: role.into(), modality, min, max, inline: false };
    let model = || {
        vec![GenerationModel {
            id: "fixture/model".into(),
            provider: "fal".into(),
            remote: "remote".into(),
            parameters: Contract::from_schema("fixture/model", ParamOwner::Model, ParamSchema::default()),
            modes: vec![GenerationMode {
                id: ModeId::new("image-to-image").unwrap(),
                inputs: vec![role("prompt", Modality::Text, 1, 1), role("image_reference", Modality::Image, 1, 4)],
                params: None,
                audio: None,
            }],
        }]
    };
    let fans = |kind: GenerationKind| {
        let node = kind.instantiate_instance(NodeId::from_bytes([51; 16]), Default::default(), &supplied()).unwrap();
        let contract = kind.instance_contract(&node).unwrap();
        contract.inputs.iter().map(|input| (input.port.clone(), input.fans_out)).collect::<Vec<_>>()
    };
    let named = |pairs: &[(&str, bool)]| pairs.iter().map(|(p, f)| ((*p).to_owned(), *f)).collect::<Vec<_>>();
    // A text answer per prompt; references still collect into each request.
    assert_eq!(fans(GenerationKind::text(model(), [1; 32]).unwrap()), named(&[("prompt", true), ("image_reference", false)]));
    // An image run makes a list per prompt, which a list port downstream flattens (#519).
    assert_eq!(fans(GenerationKind::new(model(), [1; 32]).unwrap()), named(&[("prompt", true), ("image_reference", false)]));
}

fn supplied() -> ParamValues {
    [("model".into(), Value::Text("fixture/model".into()))].into()
}
