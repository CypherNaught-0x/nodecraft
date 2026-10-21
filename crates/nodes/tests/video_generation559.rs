//! The video generation kind (#559): one clip per run from a prompt, ordered frames or a portrait
//! and speech. The wired roles choose the mode; a mode sends only the settings it takes, and
//! refuses speech longer than the model takes before anything is sent.
use nodecraft_nodes::{
    InputValue, JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole, VIDEO_KIND, is_generation, kind_for},
};
use nodecraft_types::{
    AssetRef, AudioDuration, AudioOverrun, AudioSound, AudioUse, Blake3, Contract, ErrorKind, MediaMeta, Modality, NodeId, ParamOwner, ParamSchema,
    ParamValues, PortType, ProviderTarget, TargetSelection, Value,
};
use serde_json::json;

const ID: NodeId = NodeId::from_bytes([59; 16]);
const MODEL: &str = "fixture.video";

fn role(role: &str, modality: Modality, min: u16, max: u16) -> GenerationRole {
    GenerationRole { role: role.into(), modality, min, max, inline: role == "prompt" }
}
fn mode(id: &str, inputs: Vec<GenerationRole>, params: Option<&[&str]>, audio: Option<AudioUse>) -> GenerationMode {
    GenerationMode { id: ModeId::new(id).unwrap(), inputs, params: params.map(|names| names.iter().map(|n| (*n).to_owned()).collect()), audio, edit: None }
}
fn speech(longer: AudioOverrun) -> AudioUse {
    AudioUse { role: "speech".into(), duration: AudioDuration::FollowsAudio, max_ms: Some(30_000), longer, sound: AudioSound::Input }
}
fn model(longer: AudioOverrun) -> GenerationModel {
    let schema: ParamSchema = serde_json::from_value(json!({"params": [
        {"name": "duration", "label": "Duration", "control": {"type": "unsigned", "min": "2", "max": "10", "step": "1"}, "default": {"type": "unsigned", "value": "5"}},
        {"name": "aspect_ratio", "label": "Aspect ratio", "control": {"type": "choice", "options": [{"value": "16:9", "label": "16:9"}, {"value": "9:16", "label": "9:16"}]}, "default": {"type": "text", "value": "16:9"}},
        {"name": "seed", "label": "Seed", "control": {"type": "unsigned", "min": "0", "max": "4294967295", "step": "1"}, "default": {"type": "unsigned", "value": "7"}},
    ]}))
    .unwrap();
    let prompt = || role("prompt", Modality::Text, 1, 1);
    GenerationModel {
        style_family: None,
        id: MODEL.into(),
        provider: "mock".into(),
        remote: "mock-video".into(),
        parameters: Contract::from_schema(MODEL, ParamOwner::Model, schema),
        modes: vec![
            mode("text-to-video", vec![prompt()], None, None),
            mode("image-to-video", vec![prompt(), role("first_frame", Modality::Image, 1, 1), role("last_frame", Modality::Image, 0, 1)], None, None),
            mode("reference-to-video", vec![prompt(), role("reference", Modality::Image, 1, 3)], Some(&["duration", "seed"]), None),
            // Lip-sync: the clip runs as long as the speech, so it takes no duration or aspect.
            mode(
                "speech-to-video",
                vec![prompt(), role("portrait", Modality::Image, 1, 1), role("speech", Modality::Audio, 1, 1)],
                Some(&["seed"]),
                Some(speech(longer)),
            ),
        ],
    }
}
fn kind_with(longer: AudioOverrun) -> GenerationKind {
    GenerationKind::video(vec![model(longer)], [59; 32]).unwrap()
}
fn kind() -> GenerationKind {
    kind_with(AudioOverrun::Refuse)
}
fn asset(port: &str, n: u8, mime: &str, meta: MediaMeta) -> InputValue {
    let modality = if mime.starts_with("audio/") { Modality::Audio } else { Modality::Image };
    InputValue {
        port: port.into(),
        port_type: PortType { modality, many: false },
        value: Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: mime.parse().unwrap(), meta }),
        origins: vec![],
    }
}
fn picture(port: &str, n: u8) -> InputValue {
    asset(port, n, "image/png", MediaMeta::default())
}
fn voice(ms: Option<u64>) -> InputValue {
    asset("speech", 90, "audio/wav", MediaMeta { duration_ms: ms, ..Default::default() })
}
fn plan_with(kind: &GenerationKind, inputs: &[InputValue]) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
    let supplied: ParamValues = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("A paper boat".into()))].into();
    let node = kind.instantiate_instance(ID, Default::default(), &supplied).unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(ID, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
    kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })
}
fn plan(inputs: &[InputValue]) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
    plan_with(&kind(), inputs)
}
/// The job's input roles in the order sent, with each asset's marker byte (0 for text), and the
/// settings it sends.
fn sent(jobs: &[JobSpec]) -> (Vec<(String, u8)>, Vec<String>) {
    let [JobSpec::Generate { inputs, outputs, params, .. }] = jobs else { panic!("one generation job: {jobs:?}") };
    assert_eq!(outputs.iter().map(|o| o.modality).collect::<Vec<_>>(), [Modality::Video], "one clip per run");
    let roles = inputs
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
        .collect();
    (roles, params.keys().cloned().collect())
}

#[test]
fn the_video_kind_makes_one_clip_per_run() {
    assert_eq!(kind_for(Modality::Video), Some(VIDEO_KIND));
    assert!(is_generation(VIDEO_KIND));
    let kind = kind();
    let d = kind.descriptor();
    assert_eq!((d.id.as_str(), d.label.as_str()), (VIDEO_KIND, "Generate video"));
    let [out] = d.outputs.as_slice() else { panic!("one output") };
    assert_eq!(out.port_type, PortType { modality: Modality::Video, many: false });
}

#[test]
fn the_wired_roles_choose_the_mode() {
    let (roles, params) = sent(&plan(&[]).unwrap());
    assert_eq!(roles, [("prompt".to_owned(), 0)], "nothing wired: text to video");
    assert_eq!(params, ["aspect_ratio", "duration", "seed"], "a mode without a list sends every setting");

    let (roles, _) = sent(&plan(&[picture("first_frame", 1)]).unwrap());
    assert_eq!(roles, [("prompt".to_owned(), 0), ("first_frame".to_owned(), 1)]);

    let (roles, params) = sent(&plan(&[picture("reference", 3), picture("reference", 4)]).unwrap());
    assert_eq!(roles, [("prompt".to_owned(), 0), ("reference".to_owned(), 3), ("reference".to_owned(), 4)], "references in wire order");
    assert_eq!(params, ["duration", "seed"], "the reference mode has no aspect ratio");

    let (roles, params) = sent(&plan(&[voice(Some(12_000)), picture("portrait", 5)]).unwrap());
    assert_eq!(roles, [("prompt".to_owned(), 0), ("portrait".to_owned(), 5), ("speech".to_owned(), 90)]);
    assert_eq!(params, ["seed"], "a clip that follows its audio sends no duration");
}

#[test]
fn frame_roles_keep_the_models_order_not_the_wiring_order() {
    // Wired last frame first: the model still gets its first frame first.
    let (roles, _) = sent(&plan(&[picture("last_frame", 2), picture("first_frame", 1)]).unwrap());
    assert_eq!(roles, [("prompt".to_owned(), 0), ("first_frame".to_owned(), 1), ("last_frame".to_owned(), 2)]);
    // A last frame alone fits no mode: the first frame is required.
    assert!(plan(&[picture("last_frame", 2)]).is_err());
    // Each frame role takes one picture.
    assert!(plan(&[picture("first_frame", 1), picture("first_frame", 2)]).is_err());
    // Reference counts are the model's: three, not a universal limit.
    assert!(plan(&[picture("reference", 1), picture("reference", 2), picture("reference", 3), picture("reference", 4)]).is_err());
}

#[test]
fn speech_longer_than_the_model_takes_is_refused_before_sending() {
    let error = plan(&[picture("portrait", 5), voice(Some(42_500))]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert_eq!(error.message, "the speech audio is 42.5 s long and this mode takes at most 30 s; trim it first");
    assert!(plan(&[picture("portrait", 5), voice(Some(30_000))]).is_ok(), "exactly the limit fits");
    let error = plan(&[picture("portrait", 5), voice(Some(30_001))]).unwrap_err();
    assert_eq!(error.message, "the speech audio is 30.1 s long and this mode takes at most 30 s; trim it first", "just over reads as over");
    // An unknown length goes through: the provider checks it.
    assert!(plan(&[picture("portrait", 5), voice(None)]).is_ok());
    // A model that trims long audio itself takes it.
    assert!(plan_with(&kind_with(AudioOverrun::ProviderTrims), &[picture("portrait", 5), voice(Some(42_500))]).is_ok());
}

#[test]
fn a_mode_needs_the_visual_input_it_declares() {
    // Speech without a portrait fits no mode.
    assert!(plan(&[voice(Some(1_000))]).is_err());
    // A sound file on a picture role is refused.
    let wrong = asset("portrait", 6, "audio/wav", MediaMeta::default());
    assert!(plan(&[wrong, voice(Some(1_000))]).is_err());
}

#[test]
fn a_mode_listing_a_setting_its_model_lacks_or_audio_it_doesnt_take_is_refused() {
    let mut bad = model(AudioOverrun::Refuse);
    bad.modes[2].params = Some(vec!["fps".into()]);
    assert!(GenerationKind::video(vec![bad], [1; 32]).is_err());
    let mut bad = model(AudioOverrun::Refuse);
    bad.modes[0].audio = Some(speech(AudioOverrun::Refuse));
    assert!(GenerationKind::video(vec![bad], [1; 32]).is_err(), "text to video has no speech input");
    let mut bad = model(AudioOverrun::Refuse);
    bad.modes[3].audio = Some(AudioUse { max_ms: Some(0), ..speech(AudioOverrun::Refuse) });
    assert!(GenerationKind::video(vec![bad], [1; 32]).is_err(), "a zero audio limit takes nothing");
}
