use nodecraft_graph::{Canvas, Change, Changeset, Field, Node, NodeFieldAssignment, NodePosition, Position, Project};
use nodecraft_types::{CanvasId, FieldId, NodeId, ProjectId};
fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
fn field() -> FieldId {
    FieldId::from_bytes([2; 16])
}
fn project(count: u128) -> Project {
    let mut p = Project::new(ProjectId::from_bytes([0; 16]), "Fields").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut p).unwrap();
    Changeset::new((1..=count).map(|n| Change::AddNode { canvas: canvas(), node: Node::new(NodeId::from_bytes(n.to_be_bytes()), "test.node") }).collect())
        .apply(&mut p)
        .unwrap();
    p
}
#[test]
fn membership_final_order_and_inverse_preserve_unrelated_data() {
    let mut p = project(2);
    let before = p.clone();
    let assignments = vec![NodeFieldAssignment { node: NodeId::from_bytes(1u128.to_be_bytes()), field: Some(field()) }];
    let undo = Changeset::new(vec![
        Change::SetNodeFields { canvas: canvas(), assignments },
        Change::PutField { canvas: canvas(), field: Field::new(field(), "Group") },
    ])
    .apply(&mut p)
    .unwrap();
    let saved = p.clone();
    let redo = undo.apply(&mut p).unwrap();
    assert_eq!(p, before);
    redo.apply(&mut p).unwrap();
    assert_eq!(p, saved);
    Changeset::new(vec![
        Change::RemoveField { canvas: canvas(), id: field() },
        Change::SetNodeFields { canvas: canvas(), assignments: vec![NodeFieldAssignment { node: NodeId::from_bytes(1u128.to_be_bytes()), field: None }] },
    ])
    .apply(&mut p)
    .unwrap();
    assert_eq!(p, before);
}
#[test]
fn bulk_occurrences_are_bounded_before_staging_and_invalid_moves_atomic() {
    let mut p = project(1);
    let before = p.clone();
    let id = NodeId::from_bytes(1u128.to_be_bytes());
    let moves = vec![Change::MoveNodes { canvas: canvas(), positions: vec![NodePosition { node: id, pos: Position { x: 1.0, y: 0.0 } }] }; 10001];
    assert!(Changeset::new(moves).apply(&mut p).is_err());
    assert_eq!(p, before);
    for pos in [Position { x: f32::NAN, y: 0.0 }, Position { x: 10_000_001.0, y: 0.0 }] {
        assert!(Change::MoveNodes { canvas: canvas(), positions: vec![NodePosition { node: id, pos }] }.apply(&mut p).is_err());
        assert_eq!(p, before);
    }
}
#[test]
fn maximum_members_fit_single_bulk_move_and_undo_exactly() {
    let mut p = project(10000);
    let before = p.clone();
    let positions = (1..=10000u128).map(|n| NodePosition { node: NodeId::from_bytes(n.to_be_bytes()), pos: Position { x: 32.25, y: -5.5 } }).collect();
    let undo = Change::MoveNodes { canvas: canvas(), positions }.apply(&mut p).unwrap();
    undo.apply(&mut p).unwrap();
    assert_eq!(p, before);
}
#[test]
fn legacy_defaults_and_geometry_boundaries() {
    let f: Field = serde_json::from_value(serde_json::json!({"id":field(),"title":"Old"})).unwrap();
    assert_eq!(f, Field::new(field(), "Old"));
    for (x, width) in [(0.0, 0.0), (10_000_000.0, 1.0), (0.0, f32::INFINITY), (-10_000_000.0, 20_000_002.0)] {
        let mut f = f.clone();
        f.rect.pos.x = x;
        f.rect.width = width;
        assert!(f.validate().is_err());
    }
    let mut f = f;
    f.rect.pos.x = -10_000_000.0;
    f.rect.width = 20_000_000.0;
    f.validate().unwrap();
}

#[test]
fn aggregate_inner_occurrences_mixed_vectors_exact_limit_and_replay() {
    let mut p = project(10000);
    let before = p.clone();
    let positions: Vec<_> = (1..=5000u128).map(|n| NodePosition { node: NodeId::from_bytes(n.to_be_bytes()), pos: Position { x: 1.0, y: 2.0 } }).collect();
    let assignments: Vec<_> = (1..=5001u128).map(|n| NodeFieldAssignment { node: NodeId::from_bytes(n.to_be_bytes()), field: None }).collect();
    let invalid = Changeset::new(vec![
        Change::MoveNodes { canvas: canvas(), positions: positions.clone() },
        Change::SetNodeFields { canvas: canvas(), assignments: assignments.clone() },
    ]);
    assert!(invalid.apply(&mut p).is_err());
    assert_eq!(p, before);
    let valid = Changeset::new(vec![
        Change::MoveNodes { canvas: canvas(), positions },
        Change::SetNodeFields { canvas: canvas(), assignments: assignments.into_iter().take(5000).collect() },
    ]);
    let undo = valid.apply(&mut p).unwrap();
    let after = p.clone();
    undo.validate_payload().unwrap();
    let encoded = serde_json::to_vec(&undo).unwrap();
    let replay: Changeset = serde_json::from_slice(&encoded).unwrap();
    let redo = replay.apply(&mut p).unwrap();
    assert_eq!(p, before);
    redo.apply(&mut p).unwrap();
    assert_eq!(p, after);
}

#[test]
fn bulk_and_structural_comment_replay_keep_certified_pins() {
    use nodecraft_graph::{Comment, NodeTagAssignment};
    use nodecraft_types::CommentId;
    let mut p = project(2);
    let id = NodeId::from_bytes(1u128.to_be_bytes());
    let mut root = Comment::new(CommentId::from_bytes([6; 16]), "Pinned", Position { x: 40.0, y: 20.0 });
    root.node = Some(id);
    Change::PutComment { canvas: canvas(), comment: root }.apply(&mut p).unwrap();
    let before = p.clone();
    let changes = Changeset::new(vec![
        Change::MoveNodes { canvas: canvas(), positions: vec![NodePosition { node: id, pos: Position { x: 80.0, y: 30.0 } }] },
        Change::SetNodeTags { canvas: canvas(), assignments: vec![NodeTagAssignment { node: id, tag: None }] },
        Change::RemoveNode { canvas: canvas(), id },
    ]);
    let staged = changes.stage_certified(&p).unwrap();
    let after = staged.document;
    let redo = staged.actual_redo.unwrap();
    let mut restored = after.clone();
    staged.inverse.apply(&mut restored).unwrap();
    assert_eq!(restored, before);
    let inverse = redo.apply(&mut restored).unwrap();
    assert_eq!(restored, after);
    inverse.apply(&mut restored).unwrap();
    assert_eq!(restored, before);
}
