//! A known media refusal is independent of a pending prompt, but unknown media can choose a mode.
use nodecraft_nodes::{
    InputFacts, InputValue, ModeId, NodeKind, PlanCtx, PreflightCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{
    AssetRef, AudioDuration, AudioOverrun, AudioSound, AudioUse, Blake3, Contract, DocumentUse, EditOperation, EditSound, MediaMeta, Modality, NodeId,
    ParamOwner, ParamSchema, PortType, ProviderTarget, TargetSelection, Value, VideoEdit,
};

const ID: NodeId = NodeId::from_bytes([95; 16]);
const MODEL: &str = "fixture.known-media";

fn role(name: &str, modality: Modality, min: u16, max: u16) -> GenerationRole {
    GenerationRole { role: name.into(), modality, min, max, inline: name == "prompt" }
}
fn mode(id: &str, roles: Vec<GenerationRole>) -> GenerationMode {
    GenerationMode { id: ModeId::new(id).unwrap(), inputs: roles, params: None, audio: None, edit: None, document: None }
}
fn kind(modes: Vec<GenerationMode>, video: bool) -> GenerationKind {
    let model = GenerationModel {
        id: MODEL.into(),
        provider: "mock".into(),
        remote: "mock-text".into(),
        parameters: Contract::from_schema(MODEL, ParamOwner::Model, ParamSchema::default()),
        modes,
        style_family: None,
    };
    if video { GenerationKind::video(vec![model], [95; 32]).unwrap() } else { GenerationKind::text(vec![model], [95; 32]).unwrap() }
}
fn pdf_mode(id: &str, min: u16, max: u16, pages: u32) -> GenerationMode {
    GenerationMode {
        document: Some(DocumentUse { role: "document".into(), max_pages: pages, tokens_per_page: 1000 }),
        ..mode(id, vec![role("prompt", Modality::Text, 1, 1), role("document", Modality::Document, min, max)])
    }
}
fn media(mime: &str, meta: MediaMeta) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([95; 32]), mime: mime.parse().unwrap(), meta })
}
fn node(kind: &GenerationKind) -> nodecraft_graph::Node {
    kind.instantiate_instance(ID, Default::default(), &[("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Summarise".into()))].into())
        .unwrap()
}
fn fact<'a>(port: &'a str, modality: Modality, known: Vec<&'a Value>, min_count: usize, max_count: Option<usize>) -> InputFacts<'a> {
    InputFacts { port, port_type: PortType { modality, many: true }, known, min_count, max_count }
}

#[test]
fn pending_prompt_refuses_pdf_audio_and_video_with_the_complete_plans_exact_error() {
    let pdf = pdf_mode("document-to-text", 1, 5, 100);
    let audio = GenerationMode {
        audio: Some(AudioUse {
            role: "audio".into(),
            duration: AudioDuration::FollowsAudio,
            max_ms: Some(10_000),
            longer: AudioOverrun::Refuse,
            sound: AudioSound::Input,
        }),
        ..mode("audio-to-text", vec![role("prompt", Modality::Text, 1, 1), role("audio", Modality::Audio, 1, 1)])
    };
    let video = GenerationMode {
        edit: Some(VideoEdit {
            operation: EditOperation::ObjectSwap,
            role: "source".into(),
            min_ms: Some(4000),
            uses_ms: Some(30_000),
            min_pixels: Some(409_600),
            formats: vec!["video/mp4".into()],
            sound: EditSound::Unknown,
        }),
        ..mode("object-swap", vec![role("prompt", Modality::Text, 1, 1), role("source", Modality::Video, 1, 1)])
    };
    for (mode, port, modality, value, video) in [
        (pdf, "document", Modality::Document, media("application/pdf", MediaMeta { page_count: Some(300), ..Default::default() }), false),
        (audio, "audio", Modality::Audio, media("audio/wav", MediaMeta { duration_ms: Some(10_001), ..Default::default() }), false),
        (video, "source", Modality::Video, media("video/mp4", MediaMeta { duration_ms: Some(1000), ..Default::default() }), true),
    ] {
        let kind = kind(vec![mode], video);
        let node = node(&kind);
        let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact(port, modality, vec![&value], 1, Some(1))];
        let partial = kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap_err();
        let inputs = [InputValue { port: port.into(), port_type: PortType { modality, many: true }, value, origins: vec![] }];
        let mut targets = TargetSelection::empty();
        targets.insert(ID, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
        let complete = kind.plan(&PlanCtx { node: &node, inputs: &inputs, targets: &targets }).unwrap_err();
        assert_eq!(partial, complete);
    }
}

#[test]
fn an_unresolved_list_can_select_a_more_permissive_mode() {
    let kind = kind(vec![pdf_mode("single-pdf", 1, 1, 100), pdf_mode("several-pdfs", 2, 5, 300)], false);
    let node = node(&kind);
    let pdf = media("application/pdf", MediaMeta { page_count: Some(150), ..Default::default() });
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&pdf], 1, None)];
    kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap();
    // Once the count is known, the same PDF can only choose the single-PDF mode.
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&pdf], 1, Some(1))];
    assert!(kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap_err().message.contains("at most 100"));
}

#[test]
fn a_common_refusal_survives_unknown_list_counts_and_sums_only_known_pages() {
    let kind = kind(vec![pdf_mode("single-pdf", 1, 1, 100), pdf_mode("several-pdfs", 2, 5, 100)], false);
    let node = node(&kind);
    let pdf = media("application/pdf", MediaMeta { page_count: Some(101), ..Default::default() });
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&pdf], 1, None)];
    let refusal = kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap_err();
    assert!(refusal.message.contains("101 pages"));
    let a = media("application/pdf", MediaMeta { page_count: Some(60), ..Default::default() });
    let b = media("application/pdf", MediaMeta { page_count: Some(41), ..Default::default() });
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&a, &b], 2, None)];
    assert_eq!(kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap_err(), refusal);
}

#[test]
fn unknown_page_counts_and_distinct_possible_refusals_wait_for_the_actual_input() {
    let kind = kind(vec![pdf_mode("single-pdf", 1, 1, 100), pdf_mode("several-pdfs", 2, 5, 300)], false);
    let node = node(&kind);
    let unknown = media("application/pdf", MediaMeta::default());
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&unknown], 1, Some(1))];
    kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap();
    let over = media("application/pdf", MediaMeta { page_count: Some(400), ..Default::default() });
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&over], 1, None)];
    kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap();
}

#[test]
fn known_impossible_media_counts_keep_the_full_plans_single_mode_role_error() {
    let kind = kind(vec![pdf_mode("document-to-text", 1, 1, 100)], false);
    let node = node(&kind);
    let pdf = media("application/pdf", MediaMeta { page_count: Some(300), ..Default::default() });
    let facts = [fact("prompt", Modality::Text, vec![], 1, Some(1)), fact("document", Modality::Document, vec![&pdf, &pdf], 2, Some(2))];
    let error = kind.check_known_inputs(&PreflightCtx { node: &node, inputs: &facts }).unwrap_err();
    assert_eq!(error.message, "input document takes at most 1 assets");
    let inputs = [InputValue {
        port: "document".into(),
        port_type: PortType { modality: Modality::Document, many: true },
        value: Value::List(vec![pdf.clone(), pdf]),
        origins: vec![],
    }];
    let targets = TargetSelection::empty();
    assert_eq!(kind.plan(&PlanCtx { node: &node, inputs: &inputs, targets: &targets }).unwrap_err(), error);
}
