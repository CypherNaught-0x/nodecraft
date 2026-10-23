//! PDF input (#591): a mode that reads wired PDFs refuses more pages than it reads at once before
//! anything is sent, and a kind refuses a mode that describes PDFs it doesn't take.
use nodecraft_nodes::{
    InputValue, JobSpec, ModeId, NodeKind, PlanCtx,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole},
};
use nodecraft_types::{
    AssetRef, Blake3, Contract, DocumentUse, ErrorKind, MediaMeta, Modality, NodeId, ParamOwner, ParamSchema, ParamValues, PortType, ProviderTarget,
    TargetSelection, Value,
};

const ID: NodeId = NodeId::from_bytes([91; 16]);
const MODEL: &str = "fixture.reader";

fn role(role: &str, modality: Modality, min: u16, max: u16) -> GenerationRole {
    GenerationRole { role: role.into(), modality, min, max, inline: role == "prompt" }
}
fn reads() -> DocumentUse {
    DocumentUse { role: "document".into(), max_pages: 100, tokens_per_page: 3000 }
}
fn model(document: Option<DocumentUse>) -> GenerationModel {
    let mode = GenerationMode {
        id: ModeId::new("document-to-text").unwrap(),
        inputs: vec![role("prompt", Modality::Text, 1, 1), role("document", Modality::Document, 1, 5)],
        params: None,
        audio: None,
        edit: None,
        document,
    };
    GenerationModel {
        style_family: None,
        id: MODEL.into(),
        provider: "mock".into(),
        remote: "mock-reader".into(),
        parameters: Contract::from_schema(MODEL, ParamOwner::Model, ParamSchema::default()),
        modes: vec![mode],
    }
}
fn pdf(n: u8, pages: Option<u32>) -> InputValue {
    InputValue {
        port: "document".into(),
        port_type: PortType { modality: Modality::Document, many: true },
        value: Value::Media(AssetRef {
            hash: Blake3::from_bytes([n; 32]),
            mime: "application/pdf".parse().unwrap(),
            meta: MediaMeta { page_count: pages, ..Default::default() },
        }),
        origins: vec![],
    }
}
fn plan(inputs: &[InputValue]) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
    let kind = GenerationKind::text(vec![model(Some(reads()))], [91; 32]).unwrap();
    let supplied: ParamValues = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Summarise".into()))].into();
    let node = kind.instantiate_instance(ID, Default::default(), &supplied).unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(ID, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
    kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })
}

#[test]
fn pdfs_within_the_page_limit_are_sent_beside_the_prompt() {
    let jobs = plan(&[pdf(1, Some(60)), pdf(2, Some(40))]).unwrap();
    let [JobSpec::Generate { inputs, .. }] = jobs.as_slice() else { panic!("one generation job: {jobs:?}") };
    let roles: Vec<&str> = inputs.iter().map(|i| i.role.as_str()).collect();
    assert_eq!(roles, ["prompt", "document", "document"]);
}

#[test]
fn too_many_pages_are_refused_before_anything_is_sent() {
    let over = plan(&[pdf(1, Some(60)), pdf(2, Some(41))]).unwrap_err();
    assert_eq!(over.kind, ErrorKind::InvalidInput);
    assert!(over.message.contains("101 pages") && over.message.contains("PDF to text"), "{}", over.message);
    // A PDF nobody counted goes through; the provider still checks it.
    plan(&[pdf(1, None)]).unwrap();
    assert!(plan(&[pdf(1, Some(101)), pdf(2, None)]).is_err());
    assert!(plan(&[pdf(1, Some(u32::MAX)), pdf(2, Some(u32::MAX))]).is_err(), "hostile counts are refused, not overflowed");
}

#[test]
fn a_mode_describing_pdfs_it_doesnt_take_is_refused() {
    let wrong_role = DocumentUse { role: "prompt".into(), ..reads() };
    assert!(GenerationKind::text(vec![model(Some(wrong_role))], [91; 32]).is_err());
    let bad_limit = DocumentUse { max_pages: 0, ..reads() };
    assert!(GenerationKind::text(vec![model(Some(bad_limit))], [91; 32]).is_err());
    // A mode that takes PDFs without a page limit still builds: only the catalog insists on one.
    assert!(GenerationKind::text(vec![model(None)], [91; 32]).is_ok());
}
