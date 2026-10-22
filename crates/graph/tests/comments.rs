use nodecraft_graph::{Canvas, Change, Changeset, Comment, CommentReply, Node, Position, Project};
use nodecraft_types::{CanvasId, CommentId, NodeId, ProjectId};

fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
fn node() -> NodeId {
    NodeId::from_bytes([2; 16])
}
fn root() -> CommentId {
    CommentId::from_bytes([3; 16])
}
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([9; 16]), "Comments").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut p).unwrap();
    Change::PutComment { canvas: canvas(), comment: Comment::new(root(), "Root", Position { x: 10.0, y: 20.0 }) }.apply(&mut p).unwrap();
    p
}

#[test]
fn future_anchor_certifies_actual_redo_and_preserves_valid_final_batch() {
    let before = project();
    let mut n = Node::new(node(), "source.text");
    n.pos = Position { x: 100.0, y: 200.0 };
    let forward = Changeset::new(vec![
        Change::SetCommentPin { canvas: canvas(), id: root(), node: Some(node()), pos: Position { x: 5.0, y: 6.0 } },
        Change::AddNode { canvas: canvas(), node: n },
    ]);
    let staged = forward.stage_certified(&before).unwrap();
    let redo = staged.actual_redo.as_ref().unwrap();
    assert!(redo.changes.iter().any(|change| matches!(change, Change::RestoreNode { pins, .. } if pins.len() == 1)));
    let mut restored = staged.document.clone();
    let actual_redo = staged.inverse.apply(&mut restored).unwrap();
    assert_eq!(restored, before);
    assert_eq!(&actual_redo, redo);
    let inverse_again = actual_redo.apply(&mut restored).unwrap();
    assert_eq!(restored, staged.document);
    assert_eq!(inverse_again, staged.inverse);
}

#[test]
fn deletion_detaches_thread_without_copying_its_body_into_pin_metadata() {
    let mut p = project();
    Change::AddNode { canvas: canvas(), node: Node::new(node(), "source.text") }.apply(&mut p).unwrap();
    Change::SetCommentPin { canvas: canvas(), id: root(), node: Some(node()), pos: Position { x: 3.0, y: 4.0 } }.apply(&mut p).unwrap();
    Change::MoveNode { canvas: canvas(), id: node(), pos: Position { x: 100.0, y: 200.0 } }.apply(&mut p).unwrap();
    let before = p.clone();
    let undo = Change::RemoveNode { canvas: canvas(), id: node() }.apply(&mut p).unwrap();
    let thread = p.canvases().get(&canvas()).unwrap().comments().get(&root()).unwrap();
    assert_eq!(thread.node, None);
    assert_eq!(thread.pos, Position { x: 103.0, y: 204.0 });
    assert_eq!(thread.text, "Root");
    undo.apply(&mut p).unwrap();
    assert_eq!(p, before);
}

#[test]
fn replies_restore_order_and_duplicate_ids_or_aggregate_text_reject_atomically() {
    let mut p = project();
    let r = CommentId::from_bytes([4; 16]);
    let insert = Change::InsertCommentReply { canvas: canvas(), id: root(), index: 0, reply: CommentReply::new(r, "Reply") };
    insert.apply(&mut p).unwrap();
    let before = p.clone();
    assert!(insert.apply(&mut p).is_err());
    assert_eq!(p, before);
    let undo = Change::RemoveCommentReply { canvas: canvas(), id: root(), reply: r }.apply(&mut p).unwrap();
    undo.apply(&mut p).unwrap();
    assert_eq!(p, before);
    assert!(Change::SetCommentText { canvas: canvas(), id: root(), text: "x".repeat(1024 * 1024) }.apply(&mut p).is_err());
    assert_eq!(p, before);
}

#[test]
fn moving_a_node_cannot_make_its_thread_undetachable() {
    let mut p = project();
    Change::AddNode { canvas: canvas(), node: Node::new(node(), "source.text") }.apply(&mut p).unwrap();
    Change::SetCommentPin { canvas: canvas(), id: root(), node: Some(node()), pos: Position { x: 1.0, y: 0.0 } }.apply(&mut p).unwrap();
    let before = p.clone();
    assert!(Change::MoveNode { canvas: canvas(), id: node(), pos: Position { x: 10_000_000.0, y: 0.0 } }.apply(&mut p).is_err());
    assert_eq!(p, before);
}

#[test]
fn legacy_roots_and_reply_limits_are_explicit() {
    let legacy = serde_json::json!({"id": root(), "text": "legacy", "pos": {"x": 0, "y": 0}});
    let root: Comment = serde_json::from_value(legacy).unwrap();
    assert!(root.node.is_none() && root.replies.is_empty() && !root.resolved);
    let mut full = root.clone();
    full.text = "x".repeat(1024 * 1024);
    assert!(full.validate_payload().is_ok());
    full.text.push('x');
    assert!(full.validate_payload().is_err());
    let mut reply = root;
    reply.replies.push(CommentReply::new(CommentId::from_bytes([4; 16]), "x".repeat(64 * 1024 + 1)));
    assert!(reply.validate_payload().is_err());
}

#[test]
fn transient_duplicate_reply_id_cannot_make_the_inverse_ambiguous() {
    let mut p = project();
    let id = CommentId::from_bytes([4; 16]);
    Change::InsertCommentReply { canvas: canvas(), id: root(), index: 0, reply: CommentReply::new(id, "old") }.apply(&mut p).unwrap();
    let before = p.clone();
    let changes = Changeset::new(vec![
        Change::InsertCommentReply { canvas: canvas(), id: root(), index: 1, reply: CommentReply::new(id, "new") },
        Change::RemoveCommentReply { canvas: canvas(), id: root(), reply: id },
    ]);
    assert!(changes.apply(&mut p).is_err());
    assert_eq!(p, before);
}
