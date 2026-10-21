use super::*;

const QUAD: &[u8] = b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";

fn hash(byte: u8) -> Blake3 {
    Blake3::from_bytes([byte; 32])
}

#[test]
fn a_model_decodes_with_its_bounds_and_edges() {
    let Ok(model) = Model::decode(QUAD) else { panic!("decode") };
    assert_eq!(model.triangles(), 2);
    assert_eq!(model.center, [0.5, 0.5, 0.0]);
    assert!(model.radius > 0.7 && model.radius < 0.71);
    // Four sides and the diagonal, each once.
    assert_eq!(model.edges.as_ref().map(Vec::len), Some(10));
    assert!(model.bytes > 0);
}

#[test]
fn hostile_bytes_fail_with_a_reason_instead_of_panicking() {
    for bytes in [&b""[..], b"glTF\x02\0\0\0\xff\xff\xff\xff", b"v 1 2\nf 1 2 3\n", b"{\"asset\":{\"version\":\"2.0\"},\"buffers\":[{\"uri\":\"http://x\"}]}"] {
        assert!(Model::decode(bytes).is_err(), "{bytes:?}");
    }
}

#[test]
fn the_cache_keeps_failures_and_stays_within_its_budget() {
    let mut meshes = Meshes::default();
    meshes.inline(hash(1), b"not a model");
    assert!(matches!(meshes.entries.get(&hash(1)), Some(Entry::Failed(_))));
    meshes.inline(hash(2), QUAD);
    let one = meshes.bytes();
    assert!(one > 0);
    meshes.inline(hash(2), QUAD);
    assert_eq!(meshes.bytes(), one, "a replaced model is not counted twice");
    // Over budget, models not used this frame go first.
    meshes.bytes = MAX_MODEL_BYTES + 1;
    meshes.poll();
    meshes.evict();
    assert!(!meshes.entries.contains_key(&hash(2)));
}

#[test]
fn only_3d_assets_are_a_cards_model() {
    let Ok(kinds) = nodecraft_nodes::Registry::builtins() else { panic!("registry") };
    let asset = |mime: &str| nodecraft_types::AssetRef { hash: hash(3), mime: mime.parse().unwrap_or_else(|_| panic!("mime")), meta: Default::default() };
    let make = |mime: &str| {
        let supplied: nodecraft_types::ParamValues = [("asset".to_owned(), Value::Media(asset(mime)))].into_iter().collect();
        kinds
            .get(nodecraft_nodes::mesh::SOURCE)
            .ok()
            .and_then(|k| k.instantiate_instance(nodecraft_types::NodeId::from_bytes([1; 16]), Default::default(), &supplied).ok())
    };
    assert!(make("model/obj").is_some_and(|node| mesh_of(&node).is_some()));
    assert!(make("model/gltf-binary").is_some_and(|node| mesh_of(&node).is_some()));
    assert!(make("image/png").is_none_or(|node| mesh_of(&node).is_none()));
}
