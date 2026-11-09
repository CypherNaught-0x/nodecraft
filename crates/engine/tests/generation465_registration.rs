use nodecraft_engine::Session;
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
#[test]
fn production_default_session_registers_generation_image() {
    let session = Session::new(Project::new(ProjectId::from_bytes([71; 16]), "Generation registration").unwrap()).unwrap();
    assert!(session.kinds().get("generation.image").is_ok(), "default Session must register production generation.image");
}
#[test]
fn production_default_session_keeps_source_image() {
    let session = Session::new(Project::new(ProjectId::from_bytes([72; 16]), "Source registration").unwrap()).unwrap();
    assert!(session.kinds().get("source.image").is_ok());
}

/// Provider-specific settings must not shadow the generation card's model selector or prompt.
/// A collision used to make every Session::new fail when the Mira catalog was installed (#1152).
#[test]
fn every_builtin_catalog_model_keeps_node_owned_controls_and_mira_can_instantiate() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    for model in catalog.models() {
        for param in &model.params.params {
            assert!(!matches!(param.name.as_str(), "model" | "prompt"), "{}: provider parameter {} shadows a generation control", model.id, param.name);
        }
    }
    let (kinds, _) = nodecraft_engine::generation_catalog::default_registry().unwrap();
    let values = nodecraft_types::ParamValues::from([
        ("model".into(), nodecraft_types::Value::Text("mira.model".into())),
        ("mira_model".into(), nodecraft_types::Value::Text("account/model".into())),
    ]);
    let card = kinds
        .get(nodecraft_nodes::generation::TEXT_KIND)
        .unwrap()
        .instantiate_instance(nodecraft_types::NodeId::from_bytes([73; 16]), Default::default(), &values)
        .unwrap();
    assert_eq!(card.params.get("model"), values.get("model"));
    assert_eq!(card.params.get("mira_model"), values.get("mira_model"));
    Session::new(Project::new(ProjectId::from_bytes([74; 16]), "Provider parameters").unwrap()).unwrap();
}
