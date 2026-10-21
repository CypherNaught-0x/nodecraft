use nodecraft_format::{decode, encode, journal};
use nodecraft_graph::{Change, Changeset, Field, NodeFieldAssignment, NodePosition, NodeTagAssignment, Position, Project, Tag};
use nodecraft_types::{CanvasId, FieldId, NodeId, TagId};
const OLD: &[u8] = include_bytes!("fixtures/v2/comments-v2.ncraft");
const LOG: &[u8] = include_bytes!("fixtures/v2/comments-v2.ncrj");
fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
fn node() -> NodeId {
    NodeId::from_bytes([2; 16])
}
fn edits() -> Changeset {
    let field = FieldId::from_bytes([10; 16]);
    let tag = TagId::from_bytes([11; 16]);
    let mut group = Field::new(field, "Collapsed study");
    group.color = Some("future-purple".into());
    group.collapsed = true;
    Changeset::new(vec![
        Change::SetNodeFields { canvas: canvas(), assignments: vec![NodeFieldAssignment { node: node(), field: Some(field) }] },
        Change::PutField { canvas: canvas(), field: group },
        Change::PutField { canvas: canvas(), field: Field::new(FieldId::from_bytes([12; 16]), "Empty zone") },
        Change::SetNodeTags { canvas: canvas(), assignments: vec![NodeTagAssignment { node: node(), tag: Some(tag) }] },
        Change::PutTag { canvas: canvas(), tag: Tag { id: tag, title: "Lighting".into(), color: Some("future-blue".into()) } },
        Change::MoveNodes { canvas: canvas(), positions: vec![NodePosition { node: node(), pos: Position { x: 120.25, y: -32.5 } }] },
    ])
}
#[test]
fn genuine_v2_original_identity_recovery_and_both_upgrade_paths() {
    let recovered = journal::recover(LOG).unwrap();
    assert!(recovered.legacy);
    assert_eq!(recovered.applied, 3);
    assert!(!recovered.truncated_tail);
    let expected: Project = serde_json::from_slice(include_bytes!("fixtures/v2/recovered-v2-project.json")).unwrap();
    assert_eq!(recovered.snapshot.project, expected);
    let fields: Vec<_> = recovered.snapshot.project.canvases().values().flat_map(|canvas| canvas.fields().values()).collect();
    assert!(!fields.is_empty());
    for field in fields {
        assert_eq!(field.rect.pos, Position { x: 0.0, y: 0.0 });
        assert_eq!(field.rect.width, 320.0);
        assert_eq!(field.rect.height, 240.0);
        assert!(!field.collapsed);
        assert_eq!(field.color, None);
    }
    assert!(!encode(&decode(OLD).unwrap()).unwrap().is_empty());
    for old in [OLD, include_bytes!("fixtures/v1/comments-v1.ncraft").as_slice()] {
        let (snapshot, checkpoint) = journal::checkpoint_from_archive(old.to_vec()).unwrap();
        assert_eq!(&checkpoint.bytes[..8], b"NCRJ0003");
        assert_ne!(checkpoint.archive().unwrap(), old);
        assert_eq!(decode(checkpoint.archive().unwrap()).unwrap(), snapshot);
        assert!(!journal::recover(&checkpoint.bytes).unwrap().legacy);
    }
    // Every old magic must agree exactly with the embedded archive, not just old/current.
    for magic in [b"NCRJ0001", b"NCRJ0003"] {
        let mut mismatch = LOG.to_vec();
        mismatch[..8].copy_from_slice(magic);
        assert!(journal::recover(&mismatch).is_err());
    }
}
#[test]
fn v3_colours_fields_bulk_replay_and_comments_roundtrip_together() {
    let mut snapshot = journal::recover(LOG).unwrap().snapshot;
    let before = snapshot.clone();
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    let changes = edits();
    let undo = changes.apply(&mut snapshot.project).unwrap();
    let after = snapshot.clone();
    assert_eq!(decode(&encode(&snapshot).unwrap()).unwrap(), after);
    let mut bytes = checkpoint.bytes;
    bytes.extend(journal::frame(checkpoint.identity, 1, &changes, &snapshot.assets, &Default::default()).unwrap());
    let recovered = journal::recover(&bytes).unwrap();
    assert_eq!(recovered.snapshot, after);
    assert!(!recovered.legacy);
    let redo = undo.apply(&mut snapshot.project).unwrap();
    assert_eq!(snapshot, before);
    redo.apply(&mut snapshot.project).unwrap();
    assert_eq!(snapshot, after);
    // Undo and redo are real serializable journal payloads too.
    bytes.extend(journal::frame(checkpoint.identity, 2, &undo, &snapshot.assets, &Default::default()).unwrap());
    assert_eq!(journal::recover(&bytes).unwrap().snapshot, before);
    bytes.extend(journal::frame(checkpoint.identity, 3, &redo, &snapshot.assets, &Default::default()).unwrap());
    assert_eq!(journal::recover(&bytes).unwrap().snapshot, after);
    let mut torn = bytes.clone();
    torn.extend([1, 2, 3]);
    let recovered = journal::recover(&torn).unwrap();
    assert!(recovered.truncated_tail);
    assert_eq!(recovered.snapshot, after);
    // Project skills (#168) save, journal and undo like any v3 edit.
    let skill: nodecraft_graph::Skill =
        serde_json::from_value(serde_json::json!({"id": "05050505-0505-0505-0505-050505050505", "version": 1, "name": "Storyboard", "instructions": "Six panels."})).unwrap();
    let put = Changeset::new(vec![Change::PutSkill { skill }]);
    let unput = put.apply(&mut snapshot.project).unwrap();
    let skilled = snapshot.clone();
    assert_eq!(decode(&encode(&skilled).unwrap()).unwrap(), skilled);
    bytes.extend(journal::frame(checkpoint.identity, 4, &put, &snapshot.assets, &Default::default()).unwrap());
    assert_eq!(journal::recover(&bytes).unwrap().snapshot, skilled);
    bytes.extend(journal::frame(checkpoint.identity, 5, &unput, &snapshot.assets, &Default::default()).unwrap());
    assert_eq!(journal::recover(&bytes).unwrap().snapshot, after);
    unput.apply(&mut snapshot.project).unwrap();
    // A valid node coordinate that sends its anchored pin out of range is atomic.
    let illegal = Change::MoveNodes { canvas: canvas(), positions: vec![NodePosition { node: node(), pos: Position { x: 10_000_000.0, y: 0.0 } }] };
    assert!(illegal.apply(&mut snapshot.project).is_err());
    assert_eq!(snapshot, after);
}
