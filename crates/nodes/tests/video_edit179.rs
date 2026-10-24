//! Source video edits (#179): a mode that edits a wired source refuses one the model won't take,
//! too short or too small, before anything is sent, and takes an empty prompt as no prompt.
use nodecraft_nodes::{
    InputValue, JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{
    AssetRef, Blake3, Contract, EditOperation, EditSound, ErrorKind, MediaMeta, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, PortType,
    ProviderTarget, TargetSelection, Value, VideoEdit,
};
use serde_json::json;

const ID: NodeId = NodeId::from_bytes([79; 16]);
const MODEL: &str = "fixture.swap";

fn role(role: &str, modality: Modality, min: u16, max: u16) -> GenerationRole {
    GenerationRole { role: role.into(), modality, min, max, inline: role == "prompt" }
}
fn kind() -> GenerationKind {
    let schema: ParamSchema = serde_json::from_value(json!({"params": [
        {"name": "resolution", "label": "Resolution", "control": {"type": "choice", "options": [{"value": "720p", "label": "720p"}]}, "default": {"type": "text", "value": "720p"}},
    ]}))
    .unwrap();
    let edit = VideoEdit {
        operation: EditOperation::ObjectSwap,
        role: "source".into(),
        min_ms: Some(4_000),
        uses_ms: Some(30_000),
        min_pixels: Some(409_600),
        formats: vec!["video/mp4".into()],
        sound: EditSound::Unknown,
    };
    let mode = GenerationMode {
        id: ModeId::new("object-swap").unwrap(),
        inputs: vec![role("prompt", Modality::Text, 0, 1), role("source", Modality::Video, 1, 1), role("image", Modality::Image, 1, 8)],
        params: None,
        audio: None,
        edit: Some(edit),
        document: None,
    };
    let model = GenerationModel {
        style_family: None,
        id: MODEL.into(),
        provider: "mock".into(),
        remote: "mock-swap".into(),
        parameters: Contract::from_schema(MODEL, ParamOwner::Model, schema),
        modes: vec![mode],
    };
    GenerationKind::video(vec![model], [79; 32]).unwrap()
}
fn asset(port: &str, n: u8, modality: Modality, mime: &str, meta: MediaMeta) -> InputValue {
    InputValue {
        port: port.into(),
        port_type: PortType { modality, many: false },
        value: Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: mime.parse().unwrap(), meta }),
        origins: vec![],
    }
}
fn source(ms: Option<u64>, size: Option<(u32, u32)>) -> InputValue {
    let meta = MediaMeta { duration_ms: ms, width: size.map(|s| s.0), height: size.map(|s| s.1), ..Default::default() };
    asset("source", 7, Modality::Video, "video/mp4", meta)
}
fn picture() -> InputValue {
    asset("image", 8, Modality::Image, "image/png", MediaMeta::default())
}
fn plan(prompt: &str, inputs: &[InputValue]) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
    let kind = kind();
    let supplied: ParamValues = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(prompt.into()))].into();
    let node = kind.instantiate_instance(ID, Default::default(), &supplied).unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(ID, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
    kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })
}
fn roles(jobs: &[JobSpec]) -> Vec<String> {
    let [JobSpec::Generate { inputs, .. }] = jobs else { panic!("one generation job: {jobs:?}") };
    inputs.iter().map(|i| i.role.clone()).collect()
}

#[test]
fn a_source_the_model_takes_is_sent_and_an_empty_prompt_is_no_prompt() {
    let jobs = plan("", &[source(Some(5_000), Some((640, 640))), picture()]).unwrap();
    assert_eq!(roles(&jobs), ["source", "image"]);
    let jobs = plan("a red umbrella", &[source(Some(5_000), Some((640, 640))), picture()]).unwrap();
    assert_eq!(roles(&jobs), ["prompt", "source", "image"]);
    // A length or size nobody read goes through; the provider still checks it.
    plan("", &[source(None, None), picture()]).unwrap();
}

#[test]
fn a_source_too_short_or_too_small_is_refused_with_its_reason() {
    let short = plan("", &[source(Some(3_950), Some((640, 640))), picture()]).unwrap_err();
    assert_eq!(short.kind, ErrorKind::InvalidInput);
    assert!(short.message.contains("the source video is 3.9 s long and object swap needs at least 4 s"), "{}", short.message);
    let small = plan("", &[source(Some(5_000), Some((640, 480))), picture()]).unwrap_err();
    assert!(small.message.contains("is 640×480 and object swap needs frames of at least 409600 pixels"), "{}", small.message);
}

#[test]
fn a_source_in_a_format_the_model_doesnt_take_is_refused_before_anything_is_sent() {
    let meta = MediaMeta { duration_ms: Some(5_000), ..Default::default() };
    let mov = plan("", &[asset("source", 7, Modality::Video, "video/quicktime", meta), picture()]).unwrap_err();
    assert_eq!(mov.kind, ErrorKind::InvalidInput);
    assert!(mov.message.contains("the source video is video/quicktime and object swap takes MP4"), "{}", mov.message);
}
