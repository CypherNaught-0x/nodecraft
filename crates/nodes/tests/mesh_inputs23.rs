//! A wired 3D model as a generation input (#23): Meshy's retexture and remesh take a mesh on the
//! Generate 3D card, through a mesh port, with or without the card's prompt.
use nodecraft_nodes::{
    InputValue, JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{
    AssetRef, Blake3, Contract, MediaMeta, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, PortType, ProviderTarget, TargetSelection, Value,
};

const ID: NodeId = NodeId::from_bytes([23; 16]);

fn role(role: &str, modality: Modality, min: u16, inline: bool) -> GenerationRole {
    GenerationRole { role: role.into(), modality, min, max: 1, inline }
}
fn model(id: &str, inputs: Vec<GenerationRole>) -> GenerationModel {
    GenerationModel {
        id: id.into(),
        provider: "meshy".into(),
        remote: id.into(),
        parameters: Contract::from_schema(id, ParamOwner::Model, ParamSchema::default()),
        modes: vec![GenerationMode { id: ModeId::new(id).unwrap(), inputs }],
    }
}
fn kind() -> GenerationKind {
    GenerationKind::mesh(
        vec![
            model("retexture", vec![role("mesh", Modality::Mesh, 1, false), role("prompt", Modality::Text, 1, true)]),
            model("remesh", vec![role("mesh", Modality::Mesh, 1, false)]),
        ],
        [23; 32],
    )
    .unwrap()
}
fn mesh() -> InputValue {
    InputValue {
        port: "mesh".into(),
        port_type: PortType { modality: Modality::Mesh, many: false },
        value: Value::Media(AssetRef { hash: Blake3::from_bytes([3; 32]), mime: "model/gltf-binary".parse().unwrap(), meta: MediaMeta::default() }),
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
    targets.insert(ID, &ProviderTarget::default_instance("meshy").unwrap()).unwrap();
    kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })
}
fn roles(jobs: &[JobSpec]) -> Vec<String> {
    let [JobSpec::Generate { inputs, outputs, .. }] = jobs else { panic!("one generation job: {jobs:?}") };
    assert_eq!(outputs.iter().map(|o| o.modality).collect::<Vec<_>>(), [Modality::Mesh]);
    let mut roles: Vec<String> = inputs.iter().map(|i| i.role.clone()).collect();
    roles.sort();
    roles
}

#[test]
fn the_3d_card_takes_a_wired_model_through_a_mesh_port() {
    let kind = kind();
    let ports = &kind.model_descriptor("remesh").unwrap().inputs;
    assert!(ports.iter().any(|p| p.name == "mesh" && p.port_type.modality == Modality::Mesh), "{ports:?}");
    assert_eq!(roles(&plan("remesh", None, &[mesh()]).unwrap()), ["mesh"]);
    assert_eq!(roles(&plan("retexture", Some("weathered bronze"), &[mesh()]).unwrap()), ["mesh", "prompt"]);
    assert!(plan("remesh", None, &[]).is_err(), "the model is required");
    assert!(plan("retexture", None, &[mesh()]).is_err(), "retexture needs its prompt");
}
