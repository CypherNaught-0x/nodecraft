//! The 3D generation kind (#22): one model per run, a prompt or images in, a mesh out. Image-only
//! modes (Tripo image and multiview, Trellis) plan without any prompt role.
use nodecraft_nodes::{
    InputValue, JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole, MESH_KIND, is_generation},
};
use nodecraft_types::{
    AssetRef, Blake3, Contract, MediaMeta, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, PortType, ProviderTarget, TargetSelection, Value,
};

const ID: NodeId = NodeId::from_bytes([22; 16]);

fn role(role: &str, modality: Modality, min: u16, max: u16, inline: bool) -> GenerationRole {
    GenerationRole { role: role.into(), modality, min, max, inline }
}
fn model(id: &str, mode: &str, inputs: Vec<GenerationRole>) -> GenerationModel {
    GenerationModel {
        id: id.into(),
        provider: "tripo".into(),
        remote: id.into(),
        parameters: Contract::from_schema(id, ParamOwner::Model, ParamSchema::default()),
        modes: vec![GenerationMode { id: ModeId::new(mode).unwrap(), inputs }],
    }
}
fn kind() -> GenerationKind {
    GenerationKind::mesh(
        vec![
            model("text", "text-to-mesh", vec![role("prompt", Modality::Text, 1, 1, true)]),
            model("image", "image-to-mesh", vec![role("image", Modality::Image, 1, 1, false)]),
            model("views", "multiview-to-mesh", vec![role("front", Modality::Image, 1, 1, false), role("back", Modality::Image, 0, 1, false)]),
        ],
        [22; 32],
    )
    .unwrap()
}
fn picture(n: u8) -> InputValue {
    InputValue {
        port: String::new(),
        port_type: PortType { modality: Modality::Image, many: false },
        value: Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() }),
        origins: vec![],
    }
}
fn plan(model: &str, prompt: Option<&str>, inputs: &[InputValue]) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
    let kind = kind();
    let mut supplied: ParamValues = [("model".into(), Value::Text(model.into()))].into();
    if let Some(prompt) = prompt {
        supplied.insert("prompt".into(), Value::Text(prompt.into()));
    }
    let node = kind.instantiate_instance(ID, Default::default(), &supplied).unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(ID, &ProviderTarget::default_instance("tripo").unwrap()).unwrap();
    kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })
}
fn roles(jobs: &[JobSpec]) -> Vec<(String, Modality)> {
    let [JobSpec::Generate { inputs, outputs, .. }] = jobs else { panic!("one generation job: {jobs:?}") };
    assert_eq!(outputs.iter().map(|o| o.modality).collect::<Vec<_>>(), [Modality::Mesh]);
    inputs.iter().map(|i| (i.role.clone(), if matches!(i.value, Value::Text(_)) { Modality::Text } else { Modality::Image })).collect()
}

#[test]
fn the_3d_kind_makes_one_model_per_run() {
    let kind = kind();
    let d = kind.descriptor();
    assert_eq!((d.id.as_str(), d.label.as_str()), (MESH_KIND, "Generate 3D"));
    assert!(is_generation(MESH_KIND));
    let [out] = d.outputs.as_slice() else { panic!("one output") };
    assert_eq!(out.port_type, PortType { modality: Modality::Mesh, many: false });
}

#[test]
fn a_text_mode_sends_its_prompt_and_image_modes_send_only_images() {
    assert_eq!(roles(&plan("text", Some("a red chair"), &[]).unwrap()), [("prompt".to_owned(), Modality::Text)]);
    assert!(plan("text", Some("   "), &[]).is_err(), "an empty prompt is refused before any job");

    let image = InputValue { port: "image".into(), ..picture(1) };
    assert_eq!(roles(&plan("image", None, &[image]).unwrap()), [("image".to_owned(), Modality::Image)]);
    assert!(plan("image", None, &[]).is_err(), "the image is required");

    let front = InputValue { port: "front".into(), ..picture(2) };
    let back = InputValue { port: "back".into(), ..picture(3) };
    let views = roles(&plan("views", None, &[back.clone(), front.clone()]).unwrap());
    assert_eq!(views.len(), 2);
    assert!(views.contains(&("front".to_owned(), Modality::Image)) && views.contains(&("back".to_owned(), Modality::Image)));
    assert!(plan("views", None, &[back]).is_err(), "the front view is required");

    // A model without a prompt can't take @mentions: the mentioned card would run for nothing.
    let mention = InputValue {
        port: nodecraft_nodes::mention::PORT.into(),
        port_type: PortType { modality: Modality::Text, many: false },
        value: Value::Text("a red chair".into()),
        origins: vec![NodeId::from_bytes([9; 16])],
    };
    let image = InputValue { port: "image".into(), ..picture(4) };
    assert!(plan("image", Some("@chair"), &[image, mention]).unwrap_err().message.contains("@mentions"));
}

#[test]
fn the_image_card_plans_an_image_only_mode_too() {
    // An upscaler (fal's ESRGAN) has no prompt role: the image card plans it with just its image.
    let upscale = GenerationModel {
        modes: vec![GenerationMode { id: ModeId::new("upscale").unwrap(), inputs: vec![role("image", Modality::Image, 1, 1, false)] }],
        ..model("esrgan", "upscale", vec![])
    };
    let kind = GenerationKind::new(vec![upscale], [5; 32]).unwrap();
    let supplied: ParamValues = [("model".into(), Value::Text("esrgan".into()))].into();
    let node = kind.instantiate_instance(ID, Default::default(), &supplied).unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(ID, &ProviderTarget::default_instance("tripo").unwrap()).unwrap();
    let image = InputValue { port: "image".into(), ..picture(5) };
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[image], targets: &targets }).unwrap();
    let [JobSpec::Generate { inputs, outputs, .. }] = jobs.as_slice() else { panic!("one generation job: {jobs:?}") };
    assert_eq!(inputs.iter().map(|i| i.role.as_str()).collect::<Vec<_>>(), ["image"]);
    assert_eq!(outputs.first().map(|o| o.modality), Some(Modality::Image));
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[], targets: &targets }).is_err(), "without media there is nothing to send");
}
