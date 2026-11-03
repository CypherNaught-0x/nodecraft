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
    assert_eq!(model.prepared.layout().edge_words, 10);
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
    // Over budget, a model drawn last frame stays (it is on screen); an older one goes.
    meshes.bytes = MAX_MODEL_BYTES + 1;
    meshes.poll();
    meshes.evict();
    assert!(meshes.entries.contains_key(&hash(2)), "a model on screen is not decoded again every frame");
    meshes.poll();
    meshes.evict();
    assert!(!meshes.entries.contains_key(&hash(2)));
}

#[test]
fn a_busy_read_is_asked_again_and_a_corrupt_one_is_not() {
    use nodecraft_format::PreviewReadError as E;
    let mut meshes = Meshes::default();
    meshes.fail_read(hash(1), E::Busy);
    meshes.fail_read(hash(2), E::Corrupt);
    assert!(matches!(meshes.entries.get(&hash(1)), Some(Entry::Retry { .. })));
    assert!(matches!(meshes.entries.get(&hash(2)), Some(Entry::Failed(_))));
    let Some(Entry::Retry { at, .. }) = meshes.entries.get(&hash(1)) else { panic!("retry") };
    assert_eq!(*at, RETRY_FRAMES, "not asked again on the very next frame");
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

#[test]
fn ply1011_model_preview_decodes_authored_rgba_geometry() {
    let bytes = include_bytes!("../../../media/src/mesh/fixtures/original-rgba.ply");
    let model = Model::decode(bytes).unwrap();
    assert_eq!(model.triangles(), 1);
    assert_eq!(model.center, [0.5, 0.5, 0.]);
    let mut meshes = Meshes::default();
    meshes.inline(hash(11), bytes);
    assert!(matches!(meshes.entries.get(&hash(11)), Some(Entry::Ready { .. })));
}

#[test]
fn worker1013_scope_changes_discard_models_but_ordinary_frame_binding_reuses_them() {
    use nodecraft_types::{CanvasId, ProjectId};
    let identity = Arc::new(crate::navigation::SessionIdentity);
    let context = nodecraft_format::AssetContext::default();
    let project = ProjectId::from_bytes([1; 16]);
    let canvas = CanvasId::from_bytes([2; 16]);
    let mut meshes = Meshes::default();
    meshes.bind_session(identity.clone(), project, canvas, &context);
    meshes.inline(hash(90), QUAD);
    meshes.waiting.push(hash(91), nodecraft_types::NodeId::from_bytes([4; 16]));
    let size = meshes.bytes();
    assert!(size > 0);
    meshes.bind_session(identity.clone(), project, canvas, &context.clone());
    assert_eq!(meshes.bytes(), size, "pan/zoom/orbit and unrelated edits do not replace session/context scope");
    meshes.bind_session(identity.clone(), project, CanvasId::from_bytes([3; 16]), &context);
    assert_eq!(meshes.bytes(), 0);
    assert!(meshes.entries.is_empty());
    assert!(meshes.waiting.0.is_empty());
    meshes.inline(hash(90), QUAD);
    meshes.bind_session(identity.clone(), project, CanvasId::from_bytes([3; 16]), &nodecraft_format::AssetContext::default());
    assert_eq!(meshes.bytes(), 0, "same content identity in another asset context does not retain pending authority");
    meshes.inline(hash(90), QUAD);
    meshes.bind_session(identity.clone(), ProjectId::from_bytes([4; 16]), canvas, &context);
    assert_eq!(meshes.bytes(), 0);
    meshes.inline(hash(90), QUAD);
    meshes.bind_session(Arc::new(crate::navigation::SessionIdentity), ProjectId::from_bytes([4; 16]), canvas, &context);
    assert_eq!(meshes.bytes(), 0);
}

#[test]
fn worker1013_waiting_identities_are_bounded_deduplicated_and_revalidated_in_order() {
    use nodecraft_types::NodeId;
    let owner = |n| NodeId::from_bytes([n; 16]);
    let asset = |n| AssetRef { hash: hash(n), mime: "model/obj".parse().unwrap(), meta: Default::default() };
    let assets: Vec<_> = (0..10).map(asset).collect();
    let mut waiting = BrowserQueue::default();
    waiting.push(hash(0), owner(0));
    waiting.push(hash(0), owner(9)); // A second visible card with the same original.
    for n in 1..10 {
        waiting.push(hash(n), owner(n));
    }
    assert_eq!(waiting.0.len(), MAX_WAITING_MODELS);
    let current = |id| match id {
        id if id == owner(0) => None,          // The queued card was deleted.
        id if id == owner(1) => assets.get(9), // The queued card's model was replaced.
        id => (2..10).find(|&n| owner(n) == id).and_then(|n| assets.get(usize::from(n))),
    };
    for n in 2..8 {
        let (id, value) = waiting.next_current(current).unwrap();
        assert_eq!((id, value.hash), (owner(n), hash(n)));
    }
    assert!(waiting.next_current(current).is_none(), "identities beyond the cap were never retained");
}

#[test]
fn worker1013_budget_refusal_retries_only_after_unused_capacity_is_reserved() {
    assert_eq!(MAX_BROWSER_MODEL_BYTES, 64 * 1024 * 1024);
    let mut meshes = Meshes::default();
    meshes.inline(hash(1), QUAD);
    let one = meshes.bytes();
    meshes.entries.insert(hash(2), Entry::Budget { required: one });
    meshes.entries.insert(hash(3), Entry::Failed("corrupt"));
    for _ in 0..3 {
        assert!(!meshes.retry_budget(hash(2), one));
        assert!(matches!(meshes.entries.get(&hash(2)), Some(Entry::Budget { .. })));
        assert_eq!(meshes.bytes(), one, "current model is retained; no decode or capacity changes");
    }
    meshes.poll();
    meshes.poll();
    assert!(meshes.retry_budget(hash(2), one), "offscreen old model can now be evicted before restarting");
    assert_eq!(meshes.bytes(), 0);
    assert!(!meshes.entries.contains_key(&hash(1)));
    assert!(!meshes.entries.contains_key(&hash(2)), "one retry admitted with exclusive headroom");
    assert!(matches!(meshes.entries.get(&hash(3)), Some(Entry::Failed("corrupt"))), "invalid data stays failed");
    meshes.inline(hash(2), QUAD);
    let Entry::Ready { model: first, .. } = meshes.entries.get(&hash(2)).unwrap() else { panic!("ready") };
    let first = first.clone();
    assert!(meshes.retry_budget(hash(2), one));
    assert!(matches!(meshes.entries.get(&hash(2)), Some(Entry::Ready { model, .. }) if Arc::ptr_eq(&first, model)), "resident model is not restarted");
}
