use nodecraft_format::{AssetStore, Snapshot, journal};
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Position, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, GenerationId, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;

fn snapshot() -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Recovery fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: canvas(), node: Node::new(node(), "source.text") }.apply(&mut project).unwrap();
    Snapshot { project, assets: AssetStore::default(), epoch: 7 }
}
fn canvas() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}
fn node() -> NodeId {
    NodeId::from_bytes([3; 16])
}
fn title(checkpoint: &journal::Checkpoint, sequence: u64, text: &str, assets: &AssetStore) -> Vec<u8> {
    journal::frame(checkpoint.identity, sequence, &Changeset::new(vec![Change::SetProjectTitle { title: text.into() }]), assets, &Default::default()).unwrap()
}
fn forged(checkpoint: &journal::Checkpoint, changes: serde_json::Value) -> Vec<u8> {
    let record = json!({"checkpoint":checkpoint.identity,"sequence":1,"changes":{"changes":changes}});
    let json = serde_json::to_vec(&record).unwrap();
    let mut payload = (json.len() as u64).to_le_bytes().to_vec();
    payload.extend_from_slice(&json);
    payload.extend_from_slice(&0u64.to_le_bytes());
    physical_frame(&payload)
}
fn physical_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = (payload.len() as u64).to_le_bytes().to_vec();
    frame.extend_from_slice(blake3::hash(payload).as_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn physical_damage_reports_exact_prefix_offset_reason_and_never_replays_later_frames() {
    use journal::RecoveryDamageReason as Reason;
    let snapshot = snapshot();
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    let mut prefix = checkpoint.bytes.clone();
    prefix.extend(title(&checkpoint, 1, "Good prefix", &snapshot.assets));
    let second = title(&checkpoint, 2, "Damaged frame", &snapshot.assets);
    let later = title(&checkpoint, 3, "Must never be applied", &snapshot.assets);
    let mut zero_payload = second.clone();
    zero_payload[40..].fill(0);
    let mut checksum = second.clone();
    checksum[8] ^= 1;
    let mut length = second.clone();
    length[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    let mut short_length = second.clone();
    short_length[..8].copy_from_slice(&1u64.to_le_bytes());
    for (damaged, reason, append_later) in [
        (vec![0; second.len()], Reason::ZeroFilledHeader, true),
        (zero_payload, Reason::ChecksumMismatch, true),
        (checksum, Reason::ChecksumMismatch, true),
        (length, Reason::InvalidFrameLength, true),
        (short_length, Reason::InvalidFrameLength, true),
        (second[..20].to_vec(), Reason::IncompleteHeader, false),
        (second[..second.len() - 1].to_vec(), Reason::IncompletePayload, false),
    ] {
        let mut bytes = prefix.clone();
        bytes.extend(damaged);
        if append_later {
            bytes.extend_from_slice(&later);
        }
        let original = bytes.clone();
        let recovered = journal::recover(&bytes).unwrap();
        assert_eq!(recovered.applied, 1);
        assert_eq!(recovered.snapshot.project.title(), "Good prefix");
        assert!(recovered.truncated_tail);
        let damage = recovered.damage.unwrap();
        assert_eq!(damage.offset, prefix.len() as u64);
        assert_eq!(damage.reason, reason);
        assert!(!damage.reason.description().is_empty());
        assert_eq!(bytes, original, "pure recovery preserves original evidence");
    }
}

#[test]
fn invalid_checkpoints_and_checksum_valid_semantic_violations_stay_hard_errors() {
    let snapshot = snapshot();
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    for len in [0, 8, 16, checkpoint.bytes.len() - 1] {
        assert!(journal::recover(&checkpoint.bytes[..len]).is_err());
    }
    let mut broken = checkpoint.bytes.clone();
    broken[16] ^= 1;
    assert!(journal::recover(&broken).is_err());
    for frame in [
        journal::frame(Blake3::from_bytes([9; 32]), 1, &Changeset::default(), &snapshot.assets, &Default::default()).unwrap(),
        title(&checkpoint, 2, "Wrong sequence", &snapshot.assets),
        forged(&checkpoint, json!([{"type":"set_comment_resolved"}])),
        forged(&checkpoint, json!([{"type":"remove_node","canvas":canvas(),"id":NodeId::from_bytes([9;16])}])),
        forged(&checkpoint, json!([{"type":"set_project_title","title":42}])),
    ] {
        let mut bytes = checkpoint.bytes.clone();
        bytes.extend(frame);
        bytes.extend(title(&checkpoint, 2, "Cannot repair an invalid frame", &snapshot.assets));
        assert!(journal::recover(&bytes).is_err());
    }
    // An authenticated outer frame cannot disguise malformed inner framing as a
    // power-loss tail. Asset payload checks are semantic even with a valid outer hash.
    let mut inner_length = vec![0; 16];
    inner_length[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    let mut record = serde_json::to_vec(&json!({"checkpoint":checkpoint.identity,"sequence":1,"changes":{"changes":[]}})).unwrap();
    let mut bad_asset = (record.len() as u64).to_le_bytes().to_vec();
    bad_asset.append(&mut record);
    bad_asset.extend_from_slice(&1u64.to_le_bytes());
    bad_asset.extend_from_slice(&[9; 32]);
    bad_asset.extend_from_slice(&1u64.to_le_bytes());
    bad_asset.push(b'x');
    for payload in [inner_length, bad_asset] {
        let mut bytes = checkpoint.bytes.clone();
        bytes.extend(physical_frame(&payload));
        assert!(journal::recover(&bytes).is_err());
    }
}

#[test]
fn invalid_asset_frame_cannot_be_hidden_by_a_later_removal_in_any_asset_bearing_role() {
    let snapshot = snapshot();
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    let missing =
        Value::List(vec![Value::Media(AssetRef { hash: Blake3::from_bytes([9; 32]), mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })]);
    let mut incoming = Node::new(node(), "source.text");
    incoming.params.insert("asset".into(), missing.clone());
    let generation = Generation {
        id: GenerationId::from_bytes([5; 16]),
        outputs: vec![missing.clone()],
        input_hash: None,
        items: Vec::new(),
        settings: Vec::new(),
        source_versions: Vec::new(),
        incomplete: None,
    };
    let mut added_canvas = Canvas::new(CanvasId::from_bytes([6; 16]), "Incoming");
    let mut temporary = Project::new(ProjectId::from_bytes([7; 16]), "Temporary").unwrap();
    Change::AddCanvas { canvas: added_canvas.clone() }.apply(&mut temporary).unwrap();
    Change::AddNode { canvas: added_canvas.id(), node: incoming.clone() }.apply(&mut temporary).unwrap();
    added_canvas = temporary.canvases().get(&added_canvas.id()).unwrap().clone();
    for change in [
        Change::SetParam { canvas: canvas(), id: node(), name: "asset".into(), value: Some(missing.clone()) },
        Change::SetNode { canvas: canvas(), node: incoming.clone() },
        Change::AddGeneration { canvas: canvas(), id: node(), index: 0, generation },
        Change::AddCanvas { canvas: added_canvas },
    ] {
        let mut bytes = checkpoint.bytes.clone();
        bytes.extend(forged(&checkpoint, serde_json::to_value(vec![change]).unwrap()));
        bytes.extend(
            journal::frame(
                checkpoint.identity,
                2,
                &Changeset::new(vec![Change::RemoveNode { canvas: canvas(), id: node() }]),
                &snapshot.assets,
                &Default::default(),
            )
            .unwrap(),
        );
        assert!(journal::recover(&bytes).is_err());
    }
    // Add/restore payloads must be checked too, including generation outputs.
    let empty = Snapshot { project: Project::new(ProjectId::from_bytes([1; 16]), "Empty").unwrap(), ..snapshot };
    let mut empty = empty;
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut empty.project).unwrap();
    let checkpoint = journal::checkpoint(&empty).unwrap();
    for change in
        [Change::AddNode { canvas: canvas(), node: incoming.clone() }, Change::RestoreNode { canvas: canvas(), node: incoming, edges: vec![], pins: vec![] }]
    {
        let mut bytes = checkpoint.bytes.clone();
        bytes.extend(forged(&checkpoint, serde_json::to_value(vec![change]).unwrap()));
        assert!(journal::recover(&bytes).is_err());
    }
}

#[test]
fn changed_asset_metadata_is_validated_before_a_later_move_or_removal() {
    use nodecraft_media::image::{DynamicImage, ImageFormat};
    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::new_rgba8(2, 3).write_to(&mut png, ImageFormat::Png).unwrap();
    let mut snapshot = snapshot();
    let hash = snapshot.assets.insert(&png.into_inner()).unwrap();
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    for (mime, width) in [("image/jpeg", 2), ("image/png", 99)] {
        let value = Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: MediaMeta { width: Some(width), ..Default::default() } });
        let changes = Changeset::new(vec![Change::SetParam { canvas: canvas(), id: node(), name: "asset".into(), value: Some(value) }]);
        let mut bytes = checkpoint.bytes.clone();
        bytes.extend(journal::frame(checkpoint.identity, 1, &changes, &snapshot.assets, &std::collections::BTreeSet::from([hash])).unwrap());
        bytes.extend(
            journal::frame(
                checkpoint.identity,
                2,
                &Changeset::new(vec![Change::MoveNode { canvas: canvas(), id: node(), pos: Position { x: 1.0, y: 2.0 } }]),
                &snapshot.assets,
                &Default::default(),
            )
            .unwrap(),
        );
        assert!(journal::recover(&bytes).is_err());
    }
}
