//! The audio generation kind (#672): one sound per run, from a prompt (speech, sound effects,
//! music) or from a recording (a voice changer). The wired roles choose the mode.
use nodecraft_nodes::{
    InputValue, JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{AUDIO_KIND, GenerationKind, GenerationMode, GenerationModel, GenerationRole, is_generation, kind_for},
};
use nodecraft_types::{
    AssetRef, Blake3, Contract, MediaMeta, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, PortType, ProviderTarget, TargetSelection, Value,
};
use serde_json::json;

const ID: NodeId = NodeId::from_bytes([67; 16]);
const SPEECH: &str = "fixture.speech";
const CHANGER: &str = "fixture.changer";

fn role(role: &str, modality: Modality) -> GenerationRole {
    GenerationRole { role: role.into(), modality, min: 1, max: 1, inline: role == "prompt" }
}
fn model(id: &str, mode: &str, inputs: Vec<GenerationRole>) -> GenerationModel {
    let schema: ParamSchema = serde_json::from_value(json!({"params": [
        {"name": "stability", "label": "Stability", "control": {"type": "slider", "min": 0.0, "max": 1.0, "step": 0.5}, "default": {"type": "number", "value": 0.5}},
    ]}))
    .unwrap();
    GenerationModel {
        style_family: None,
        id: id.into(),
        provider: "mock".into(),
        remote: format!("mock-{id}"),
        parameters: Contract::from_schema(id, ParamOwner::Model, schema),
        modes: vec![GenerationMode { id: ModeId::new(mode).unwrap(), inputs, params: None, audio: None, edit: None }],
    }
}
fn kind() -> GenerationKind {
    GenerationKind::audio(
        vec![
            // A speech model names its text mode for what it says, not "text-to-audio".
            model(SPEECH, "text-to-speech", vec![role("prompt", Modality::Text)]),
            model(CHANGER, "audio-to-audio", vec![role("audio", Modality::Audio)]),
        ],
        [67; 32],
    )
    .unwrap()
}
fn recording(n: u8, mime: &str) -> InputValue {
    let modality = if mime.starts_with("audio/") { Modality::Audio } else { Modality::Image };
    InputValue {
        port: "audio".into(),
        port_type: PortType { modality, many: false },
        value: Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: mime.parse().unwrap(), meta: MediaMeta::default() }),
        origins: vec![],
    }
}
fn plan(model: &str, prompt: &str, inputs: &[InputValue]) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
    let kind = kind();
    let supplied: ParamValues = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text(prompt.into()))].into();
    let node = kind.instantiate_instance(ID, Default::default(), &supplied).unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(ID, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
    kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })
}
/// The job's input roles in the order sent, with each asset's marker byte (0 for text).
fn sent(jobs: &[JobSpec]) -> Vec<(String, u8)> {
    let [JobSpec::Generate { inputs, outputs, .. }] = jobs else { panic!("one generation job: {jobs:?}") };
    assert_eq!(outputs.iter().map(|o| (o.port.as_str(), o.modality)).collect::<Vec<_>>(), [("out", Modality::Audio)], "one sound per run");
    inputs
        .iter()
        .map(|i| {
            (
                i.role.clone(),
                match &i.value {
                    Value::Media(asset) => asset.hash.as_bytes()[0],
                    _ => 0,
                },
            )
        })
        .collect()
}

#[test]
fn the_audio_kind_makes_one_sound_per_run() {
    assert_eq!(kind_for(Modality::Audio), Some(AUDIO_KIND));
    assert!(is_generation(AUDIO_KIND));
    let kind = kind();
    let d = kind.descriptor();
    assert_eq!((d.id.as_str(), d.label.as_str()), (AUDIO_KIND, "Generate audio"));
    let [out] = d.outputs.as_slice() else { panic!("one output") };
    assert_eq!(out.port_type, PortType { modality: Modality::Audio, many: false });
}

#[test]
fn a_speech_model_reads_the_prompt() {
    assert_eq!(sent(&plan(SPEECH, "Hello there", &[]).unwrap()), [("prompt".to_owned(), 0)]);
    assert!(plan(SPEECH, "  ", &[]).is_err(), "speech needs something to say");
}

#[test]
fn a_voice_changer_takes_the_wired_recording_alone() {
    let kind = kind();
    let instance = kind.model_descriptor(CHANGER).unwrap();
    assert!(instance.inputs.iter().any(|p| p.name == "audio" && p.port_type.modality == Modality::Audio), "{:?}", instance.inputs);
    // A typed prompt left over from another model isn't sent: the mode has no prompt.
    assert_eq!(sent(&plan(CHANGER, "left over", &[recording(9, "audio/wav")]).unwrap()), [("audio".to_owned(), 9)]);
    assert!(plan(CHANGER, "", &[]).is_err(), "nothing to change");
    assert!(plan(CHANGER, "", &[recording(9, "image/png")]).is_err(), "a picture is not a recording");
}
