use nodecraft_graph::{Canvas, Change, Changeset, Node, NodeTagAssignment, Project, Tag};
use nodecraft_types::{CanvasId, NodeId, ProjectId, TagId};
fn fixture(n: usize) -> (Project, CanvasId, TagId, Vec<NodeTagAssignment>) {
    let c = CanvasId::from_bytes([1; 16]);
    let t = TagId::from_bytes([2; 16]);
    let mut p = Project::new(ProjectId::from_bytes([0; 16]), "Tags").unwrap();
    Change::AddCanvas { canvas: Canvas::new(c, "Canvas") }.apply(&mut p).unwrap();
    let mut assignments = Vec::new();
    let mut changes = Vec::new();
    for i in 0..n {
        let id = NodeId::from_bytes((i as u128).to_be_bytes());
        changes.push(Change::AddNode { canvas: c, node: Node::new(id, "test.text") });
        assignments.push(NodeTagAssignment { node: id, tag: Some(t) });
    }
    Changeset::new(changes).apply(&mut p).unwrap();
    (p, c, t, assignments)
}
#[test]
fn full_selection_plus_creation_undoes_with_compact_inverse() {
    let (mut p, c, t, a) = fixture(10000);
    let before = p.clone();
    let edits = Changeset::new(vec![
        Change::PutTag { canvas: c, tag: Tag { id: t, title: "Blue".into(), color: Some("blue".into()) } },
        Change::SetNodeTags { canvas: c, assignments: a },
    ]);
    let undo = edits.apply(&mut p).unwrap();
    assert_eq!(undo.changes.len(), 2);
    undo.validate_replay_budget(2 * 1024 * 1024).unwrap();
    undo.apply(&mut p).unwrap();
    assert_eq!(p, before);
}
#[test]
fn bulk_counts_and_duplicates_are_independently_bounded() {
    let (p, c, _, a) = fixture(1);
    assert!(Changeset::new(vec![Change::SetNodeTags { canvas: c, assignments: vec![a[0]; 10001] }]).validate_payload().is_err());
    assert!(Changeset::new(vec![Change::SetNodeTags { canvas: c, assignments: vec![a[0]; 2] }]).validate_payload().is_err());
    let many: Vec<_> = (0u128..6000).map(|i| NodeTagAssignment { node: NodeId::from_bytes(i.to_be_bytes()), tag: None }).collect();
    let changes = Changeset::new(vec![Change::SetNodeTags { canvas: c, assignments: many.clone() }, Change::SetNodeTags { canvas: c, assignments: many }]);
    assert!(changes.validate_payload().is_err());
    assert!(changes.validate(&p).is_err());
}
#[test]
fn legacy_and_unknown_color_fields_are_compatible_but_bounded() {
    let id = TagId::from_bytes([3; 16]);
    let t: Tag = serde_json::from_value(serde_json::json!({"id":id,"title":"Legacy","future":1})).unwrap();
    assert_eq!(t.color, None);
    assert!(serde_json::to_value(&t).unwrap().get("color").is_none());
    let (mut p, c, _, _) = fixture(1);
    let before = p.clone();
    assert!(Change::PutTag { canvas: c, tag: Tag { id, title: "x".into(), color: Some("x".repeat(65)) } }.apply(&mut p).is_err());
    assert_eq!(p, before);
}

#[test]
fn ordered_tag_references_are_validated_at_transaction_end_and_inverse_replays() {
    let (mut p, c, t, a) = fixture(1);
    let base = p.clone();
    // Assignment before definition is valid when the completed draft resolves it.
    let undo = Changeset::new(vec![
        Change::SetNodeTags { canvas: c, assignments: a.clone() },
        Change::PutTag { canvas: c, tag: Tag { id: t, title: "T".into(), color: None } },
    ])
    .apply(&mut p)
    .unwrap();
    let tagged = p.clone();
    undo.apply(&mut p).unwrap();
    assert_eq!(p, base);
    p = tagged.clone();
    // Removing the definition before clearing references must also have a replayable inverse.
    let undo = Changeset::new(vec![
        Change::RemoveTag { canvas: c, id: t },
        Change::SetNodeTags { canvas: c, assignments: vec![NodeTagAssignment { node: a[0].node, tag: None }] },
    ])
    .apply(&mut p)
    .unwrap();
    assert_eq!(p, base);
    let redo = undo.apply(&mut p).unwrap();
    assert_eq!(p, tagged);
    redo.apply(&mut p).unwrap();
    assert_eq!(p, base);
}

#[test]
fn serialized_and_programmatic_invalid_colors_reject_before_publication() {
    let (p, c, t, _) = fixture(1);
    let mut encoded = serde_json::to_value(&p).unwrap();
    encoded["canvases"][c.to_string()]["tags"] = serde_json::json!({t.to_string():{"id":t,"title":"Tag","color":"x".repeat(65)}});
    assert!(serde_json::from_value::<Project>(encoded).is_err());
}

#[test]
fn bulk_membership_keys_are_required_and_unknown_members_are_refused() {
    let node = NodeId::from_bytes([4; 16]);
    for invalid in [serde_json::json!({"node":node}), serde_json::json!({"node":node,"tag":null,"extra":1})] {
        assert!(serde_json::from_value::<NodeTagAssignment>(invalid).is_err());
    }
    for invalid in [serde_json::json!({"node":node}), serde_json::json!({"node":node,"field":null,"extra":1})] {
        assert!(serde_json::from_value::<nodecraft_graph::NodeFieldAssignment>(invalid).is_err());
    }
    assert_eq!(serde_json::from_value::<NodeTagAssignment>(serde_json::json!({"node":node,"tag":null})).unwrap().tag, None);
    assert_eq!(serde_json::from_value::<nodecraft_graph::NodeFieldAssignment>(serde_json::json!({"node":node,"field":null})).unwrap().field, None);
}

#[test]
fn palette_identifiers_refuse_empty_and_control_text_without_losing_unknown_colors() {
    let (mut p, c, t, _) = fixture(1);
    let before = p.clone();
    for color in ["", "blue\n", "Blue", "blue green", "💙"] {
        let tag = Tag { id: t, title: "Tag".into(), color: Some(color.into()) };
        assert!(Change::PutTag { canvas: c, tag }.apply(&mut p).is_err());
        assert_eq!(p, before);
        let mut field = nodecraft_graph::Field::new(nodecraft_types::FieldId::from_bytes([5; 16]), "Field");
        field.color = Some(color.into());
        assert!(Change::PutField { canvas: c, field }.apply(&mut p).is_err());
        assert_eq!(p, before);
    }
    Change::PutTag { canvas: c, tag: Tag { id: t, title: "Tag".into(), color: Some("unlisted-color7".into()) } }.apply(&mut p).unwrap();
    let roundtrip: Project = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
    assert_eq!(roundtrip, p);
}
