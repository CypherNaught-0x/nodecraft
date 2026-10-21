//! The builtin 3D models (#22) appear on a Generate 3D card, never on an image card.
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_nodes::generation::{KIND, MESH_KIND};
use nodecraft_types::Modality;

#[test]
fn builtin_3d_models_are_offered_on_a_3d_card() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let card = kinds.get(MESH_KIND).unwrap().descriptor();
    assert_eq!(card.label, "Generate 3D");
    assert_eq!(card.outputs[0].port_type.modality, Modality::Mesh);
    let models: Vec<_> = snapshot.models(MESH_KIND).map(|m| m.id.as_str()).collect();
    for id in [
        "fal.trellis2",
        "tripo.text-to-3d",
        "tripo.image-to-3d",
        "tripo.multiview-to-3d",
        "meshy.text-to-3d",
        "meshy.image-to-3d",
        "meshy.multi-image-to-3d",
        "meshy.retexture",
        "meshy.retexture-image",
        "meshy.remesh",
    ] {
        assert!(models.contains(&id), "{id} in {models:?}");
        assert!(snapshot.model(KIND, id).is_none(), "{id} is not an image model");
    }
    assert!(GenerationCatalog::is_model_selector(MESH_KIND, "model"));
}
