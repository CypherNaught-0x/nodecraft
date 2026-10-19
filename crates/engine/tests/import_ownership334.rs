use nodecraft_engine::{
    Session,
    import::{Incoming, import_files},
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Import ownership").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Session::new(project).unwrap()
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[127; 16], &Default::default()).unwrap()
}
fn add(s: &mut Session, canvas: CanvasId, image: &[u8], count: usize) -> nodecraft_engine::import::Imported {
    let files = (0..count).map(|_| Incoming { name: "image.png", bytes: image }).collect::<Vec<_>>();
    let ids = (0..count).map(|i| NodeId::from_bytes([u8::try_from(i + 10).unwrap(); 16])).collect::<Vec<_>>();
    import_files(s, canvas, Position { x: 0.0, y: 0.0 }, 100.0, &files, &ids).unwrap()
}
#[test]
fn rejected_paste_preserves_unrelated_prestaged_asset() {
    let mut s = session();
    let staged = s.import_asset(b"private staged asset A").unwrap();
    let before = s.document().clone();
    let image = png();
    let missing = CanvasId::from_bytes([99; 16]);
    assert!(
        import_files(&mut s, missing, Position { x: 0.0, y: 0.0 }, 100.0, &[Incoming { name: "image.png", bytes: &image }], &[NodeId::from_bytes([10; 16])])
            .is_err()
    );
    assert_eq!(s.document(), &before);
    assert!(!s.can_undo() && !s.can_redo());
    assert!(s.assets().get(&staged).is_some(), "refusing this paste must preserve unrelated pre-staged A");
    assert_eq!(s.assets().len(), 1);
}
#[test]
fn existing_media_succeeds_at_asset_count_limit() {
    let mut s = session();
    let image = png();
    let hash = s.import_asset(&image).unwrap();
    for n in 0..1023 {
        s.import_asset(format!("existing {n}").as_bytes()).unwrap();
    }
    assert_eq!(s.assets().len(), 1024);
    let imported = add(&mut s, C, &image, 1);
    assert_eq!(imported.nodes.len(), 1, "existing content needs no additional asset slot");
    assert!(imported.skipped.is_empty());
    assert_eq!(s.assets().len(), 1024);
    assert!(s.assets().get(&hash).is_some());
}
#[test]
fn repeated_media_in_one_batch_is_charged_once() {
    let mut s = session();
    for n in 0..1023 {
        s.import_asset(format!("existing {n}").as_bytes()).unwrap();
    }
    assert_eq!(s.assets().len(), 1023);
    let image = png();
    let imported = add(&mut s, C, &image, 2);
    assert_eq!(imported.nodes.len(), 2, "identical batch content needs only one new asset slot");
    assert!(imported.skipped.is_empty());
    assert_eq!(s.assets().len(), 1024);
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert!(!s.can_undo());
    s.dispatch("edit.redo", &serde_json::json!({})).unwrap();
    assert_eq!(s.document().canvases()[&C].nodes().len(), 2);
}

#[test]
fn full_byte_store_reuses_existing_content_and_later_text_survives_new_media_refusal() {
    let mut s = session();
    let mut first = png();
    first.resize(nodecraft_format::MAX_ASSET_BYTES, 1);
    s.import_asset(&first).unwrap();
    for fill in [2, 3] {
        let mut bytes = png();
        bytes.resize(nodecraft_format::MAX_ASSET_BYTES, fill);
        s.import_asset(&bytes).unwrap();
    }
    assert_eq!(s.assets().byte_len(), nodecraft_format::MAX_TOTAL_ASSET_BYTES as u64);
    let new = png();
    let files = [Incoming { name: "new.png", bytes: &new }, Incoming { name: "existing.png", bytes: &first }, Incoming { name: "text.txt", bytes: b"kept" }];
    let ids = [10, 11, 12].map(|n| NodeId::from_bytes([n; 16]));
    let imported = import_files(&mut s, C, Position { x: 2.0, y: 3.0 }, 100.0, &files, &ids).unwrap();
    assert_eq!(imported.nodes, ids[1..]);
    assert_eq!(imported.skipped, [("new.png".into(), "the project's media is full (48 MiB)".into())]);
    assert_eq!(s.assets().len(), 3);
    assert_eq!(s.document().canvases()[&C].nodes()[&ids[1]].pos.x, 2.0);
    assert_eq!(s.document().canvases()[&C].nodes()[&ids[2]].pos.x, 102.0);
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert!(s.document().canvases()[&C].nodes().is_empty());
    assert!(!s.can_undo());
}
#[test]
fn rejected_mixed_paste_preserves_staged_assets_and_preexisting_prepared_authority() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel};
    use std::sync::Arc;
    let mut s = session();
    let staged = s.import_asset(b"unrelated staged A").unwrap();
    let input = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"pending B".as_slice()))), 0, 9).unwrap();
    let receipt = s.prepare_imports(&[input], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    let status = s.file_status();
    let image = png();
    let files = [Incoming { name: "image.png", bytes: &image }, Incoming { name: "text.txt", bytes: b"text" }];
    let repeated = NodeId::from_bytes([10; 16]);
    assert!(import_files(&mut s, C, Position { x: 0.0, y: 0.0 }, 100.0, &files, &[repeated, repeated]).is_err());
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(s.file_status(), status);
    assert!(s.document().canvases()[&C].nodes().is_empty());
    assert_eq!(s.assets().len(), 1);
    assert!(s.assets().contains(&staged));
    let node = s.kinds().get("source.text").unwrap().instantiate_instance(repeated, Default::default(), &Default::default()).unwrap();
    s.dispatch_with_imports("canvas.paste", &serde_json::json!({"canvas":C,"nodes":[node]}), receipt).unwrap();
    assert_eq!(s.assets().len(), 2, "old preparation remains authorized after refusal");
}
#[test]
fn every_over_limit_file_has_a_bounded_ordered_skip_and_one_undo() {
    let mut s = session();
    let names = (0..99).map(|n| format!("{n:03}{}", "界".repeat(100))).collect::<Vec<_>>();
    let files = names.iter().map(|name| Incoming { name, bytes: b"text" }).collect::<Vec<_>>();
    let ids = (0..32).map(|n| NodeId::from_bytes([n + 10; 16])).collect::<Vec<_>>();
    let imported = import_files(&mut s, C, Position { x: 0.0, y: 0.0 }, 100.0, &files, &ids).unwrap();
    assert_eq!(imported.nodes, ids);
    assert_eq!(imported.skipped.len(), 67);
    for (actual, expected) in imported.skipped.iter().zip(&names[32..]) {
        assert_eq!(actual.0, expected.chars().take(80).collect::<String>());
        assert_eq!(actual.0.chars().count(), 80);
        assert_eq!(actual.1, "at most 32 files are added at once");
    }
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert!(!s.can_undo());
    assert!(s.document().canvases()[&C].nodes().is_empty());
}
#[test]
fn prune_compatibility_retains_live_and_all_history_roots() {
    let mut s = session();
    let image = png();
    add(&mut s, C, &image, 1);
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(&image).as_bytes());
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    let orphan = s.import_asset(b"orphan").unwrap();
    let revision = s.revision();
    let status = s.file_status();
    s.prune_assets().unwrap();
    assert!(!s.assets().contains(&orphan));
    assert!(s.assets().contains(&hash), "redo-only root");
    assert_eq!(s.revision(), revision);
    assert_eq!(s.file_status(), status);
    s.dispatch("edit.redo", &serde_json::json!({})).unwrap();
    s.dispatch("node.remove", &serde_json::json!({"canvas":C,"id":NodeId::from_bytes([10;16])})).unwrap();
    s.prune_assets().unwrap();
    assert!(s.assets().contains(&hash), "undo-only root");
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(s.document().canvases()[&C].nodes().len(), 1);
}
