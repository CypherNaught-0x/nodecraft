//! PDF input on text cards (#591): every built-in chat model offers a document port, and swapping
//! a card with a wired PDF to a model that can't read PDFs says what to do instead.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources},
    generation_catalog::GenerationCatalog,
    run_cmds::RunCatalog,
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_types::{CanvasId, EdgeId, Modality, NodeId, ProjectId, Value};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const READER: NodeId = NodeId::from_bytes([1; 16]);
const PDF: NodeId = NodeId::from_bytes([2; 16]);

fn model(id: &str, modes: &str) -> String {
    format!(
        r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "mock-text"
label = "Fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
{modes}"#
    )
}

/// A text model that reads PDFs, and one that reads only text.
fn catalog() -> Catalog {
    let reader = model(
        "fixture.reader",
        r#"[[model.modes]]
id = "document-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "document", modality = "document", min = 1, max = 5 }]
document = { role = "document", max_pages = 100, tokens_per_page = 3000 }
"#,
    );
    Catalog::from_toml(&format!("{reader}\n{}", model("fixture.plain", ""))).unwrap()
}

#[test]
fn a_wired_pdf_names_the_way_out_when_the_model_cant_read_it() {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([91; 16]), "PDF swap").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let reader = kinds
        .get(TEXT_KIND)
        .unwrap()
        .instantiate_instance(READER, Default::default(), &[("model".into(), Value::Text("fixture.reader".into()))].into())
        .unwrap();
    Change::AddNode { canvas: C, node: reader }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    // Unwired, the card swaps freely.
    s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[READER],"model":"fixture.plain"})).unwrap();
    let pdf = s.kinds().get(nodecraft_nodes::document::SOURCE).unwrap().instantiate_instance(PDF, Default::default(), &Default::default()).unwrap();
    s.dispatch("node.add", &json!({"canvas":C,"node":pdf})).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([3; 16]),
        from: Endpoint { node: PDF, port: "out".into() },
        to: Endpoint { node: READER, port: "document".into() },
        order: 0,
    };
    s.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    let error = s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[READER],"model":"fixture.plain"})).unwrap_err();
    assert!(error.message.contains("can't read PDFs") && error.message.contains("PDF to text"), "{}", error.message);
}

#[test]
fn builtin_chat_models_read_pdfs_on_a_text_card() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let chat: Vec<_> = snapshot.models(TEXT_KIND).filter(|m| m.provider == "anthropic" || m.provider == "openai").cloned().collect();
    assert!(!chat.is_empty());
    for model in chat {
        let mode = model.modes.iter().find(|m| m.id.as_str() == "document-to-text").unwrap_or_else(|| panic!("{} has no document-to-text mode", model.id));
        assert_eq!(mode.document.as_ref().map(|d| d.role.as_str()), Some("document"), "{}", model.id);
        let node =
            kinds.get(TEXT_KIND).unwrap().instantiate_instance(READER, Default::default(), &[("model".into(), Value::Text(model.id.clone()))].into()).unwrap();
        let port = node.inputs.iter().find(|p| p.name == "document").unwrap_or_else(|| panic!("{} card has no document port", model.id));
        assert_eq!(port.port_type.modality, Modality::Document);
    }
}
