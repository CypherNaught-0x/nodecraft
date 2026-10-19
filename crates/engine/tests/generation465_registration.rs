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
