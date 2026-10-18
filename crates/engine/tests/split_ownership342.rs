use nodecraft_engine::{
    Session,
    import::{Incoming, import_files},
    split::{Split, split_grid},
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_types::{Blake3, CanvasId, NodeId, ProjectId};
use serde_json::json;
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([1; 16]);
fn png(side: u32) -> Vec<u8> {
    nodecraft_media::encode_png(side, side, &[20, 30, 40, 255].repeat((side * side) as usize), &Default::default()).unwrap()
}
fn session() -> Session {
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Split ownership").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let mut s = Session::new(p).unwrap();
    import_files(&mut s, C, Position { x: 0.0, y: 0.0 }, 100.0, &[Incoming { name: "source.png", bytes: &png(2) }], &[SOURCE]).unwrap();
    s
}
fn split(s: &mut Session, ids: &[NodeId]) -> Result<Vec<NodeId>, nodecraft_types::Error> {
    split_grid(s, &Split { canvas: C, node: SOURCE, rows: 2, columns: 2, ids, step: (100.0, 100.0) })
}
fn ids(start: u8) -> [NodeId; 4] {
    [start, start + 1, start + 2, start + 3].map(|n| NodeId::from_bytes([n; 16]))
}
#[test]
fn rejected_split_preserves_unrelated_staged_and_existing_tile_assets() {
    let mut s = session();
    let a = s.import_asset(b"unrelated staged A").unwrap();
    let b = s.import_asset(&png(1)).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    assert!(split(&mut s, &[NodeId::from_bytes([50; 16]); 4]).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert!(s.assets().get(&a).is_some(), "refused split must retain unrelated staged A");
    assert!(s.assets().get(&b).is_some(), "refused split must retain pre-existing deduplicated B");
}
#[test]
fn identical_tiles_charge_one_count_slot_and_repeated_split_costs_zero() {
    let mut s = session();
    for n in 0..1022 {
        s.import_asset(format!("staged {n}").as_bytes()).unwrap();
    }
    assert_eq!(s.assets().len(), 1023);
    assert_eq!(split(&mut s, &ids(50)).unwrap(), ids(50));
    assert_eq!(s.assets().len(), 1024);
    let first = s.document().clone();
    assert_eq!(split(&mut s, &ids(60)).unwrap(), ids(60));
    assert_eq!(s.assets().len(), 1024);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &first);
    s.dispatch("edit.redo", &json!({})).unwrap();
}
#[test]
fn identical_tiles_fit_exact_byte_capacity_and_repeat_without_new_storage() {
    let mut s = session();
    let tile = png(1);
    let cap = nodecraft_format::MAX_TOTAL_ASSET_BYTES;
    let mut remaining = cap - s.assets().byte_len() as usize - tile.len();
    let mut n = 1u8;
    while remaining > 0 {
        let len = remaining.min(nodecraft_format::MAX_ASSET_BYTES);
        s.import_asset(&vec![n; len]).unwrap();
        remaining -= len;
        n += 1;
    }
    assert_eq!(s.assets().byte_len(), (cap - tile.len()) as u64);
    split(&mut s, &ids(50)).unwrap();
    assert_eq!(s.assets().byte_len(), cap as u64);
    let count = s.assets().len();
    split(&mut s, &ids(60)).unwrap();
    assert_eq!(s.assets().byte_len(), cap as u64);
    assert_eq!(s.assets().len(), count);
    let hash = Blake3::from_bytes(*blake3::hash(&tile).as_bytes());
    assert!(s.assets().get(&hash).is_some());
}

#[test]
fn refusal_preserves_preexisting_receipt_and_all_logical_state() {
    use nodecraft_format::asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel};
    use std::sync::Arc;
    let mut s = session();
    let a = s.import_asset(b"staged A").unwrap();
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"pending B".as_slice()))), 0, 9).unwrap();
    let receipt = s.prepare_imports(&[range], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let revision = s.revision();
    let before = s.document().clone();
    let events = s.events_since(0).unwrap();
    let status = s.file_status();
    assert!(split(&mut s, &[SOURCE; 4]).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(s.file_status(), status);
    assert!(s.assets().contains(&a));
    s.dispatch_with_imports("node.move", &json!({"canvas":C,"id":SOURCE,"pos":{"x":1,"y":0}}), receipt).unwrap();
    assert_eq!(s.assets().len(), 3);
}
#[test]
fn public_negative_step_preserves_layout_and_invalid_positions_never_publish() {
    let mut s = session();
    let made = split_grid(&mut s, &Split { canvas: C, node: SOURCE, rows: 2, columns: 2, ids: &ids(50), step: (-10.0, -20.0) }).unwrap();
    let positions = made
        .iter()
        .map(|id| {
            let p = s.document().canvases()[&C].nodes()[id].pos;
            (p.x, p.y)
        })
        .collect::<Vec<_>>();
    assert_eq!(positions, [(-10.0, 0.0), (-20.0, 0.0), (-10.0, -20.0), (-20.0, -20.0)]);
    let before = s.document().clone();
    let count = s.assets().len();
    for step in [(f32::NAN, 1.0), (f32::INFINITY, 1.0), (10_000_000.0, 1.0)] {
        assert!(split_grid(&mut s, &Split { canvas: C, node: SOURCE, rows: 2, columns: 2, ids: &ids(60), step }).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.assets().len(), count);
    }
}
#[test]
fn a_full_store_refuses_genuinely_new_tiles_without_touching_staged_content() {
    let mut s = session();
    let staged = s.import_asset(b"staged A").unwrap();
    let mut remaining = nodecraft_format::MAX_TOTAL_ASSET_BYTES - s.assets().byte_len() as usize;
    let mut fill = 1;
    while remaining > 0 {
        let n = remaining.min(nodecraft_format::MAX_ASSET_BYTES);
        s.import_asset(&vec![fill; n]).unwrap();
        remaining -= n;
        fill += 1;
    }
    let before = s.document().clone();
    let count = s.assets().len();
    assert!(split(&mut s, &ids(50)).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.assets().len(), count);
    assert!(s.assets().contains(&staged));
}
