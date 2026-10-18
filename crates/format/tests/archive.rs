use nodecraft_format::{AssetStore, Snapshot, decode, encode, journal};
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, GenerationId, MediaMeta, NodeId, ProjectId, Value};
use proptest::prelude::*;
use serde_json::json;
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn project() -> Project {
    Project::new(ProjectId::from_bytes([1; 16]), "Fixture").unwrap()
}
fn snapshot() -> Snapshot {
    Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 }
}
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer.start_file(*name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
fn manifest(project: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"project":project})).unwrap()
}

#[test]
fn golden_archive_round_trip_and_optional_fields() {
    let golden = include_bytes!("fixtures/minimal.ncraft");
    let loaded = decode(golden).unwrap();
    assert_eq!(loaded.project, project());
    assert_eq!(loaded.epoch, 0);
    assert_eq!(decode(&encode(&loaded).unwrap()).unwrap(), loaded);
    assert_eq!(encode(&loaded).unwrap(), encode(&loaded).unwrap());
    let value = json!({"id": ProjectId::from_bytes([1;16]), "title":"Fixture", "future":{"setting":true}});
    assert_eq!(decode(&zip(&[("project.json", &manifest(value))])).unwrap().project, project());
    let future = br#"{"version":999,"project":{"unrecognized_node_type":"future"}}"#;
    assert_eq!(decode(&zip(&[("project.json", future)])).unwrap_err().kind, ErrorKind::Unsupported);
    assert!(decode(&zip(&[("project.json", &manifest(json!({"title":"missing identity"})))])).is_err());
}

#[test]
fn assets_deduplicate_validate_hashes_and_cover_nested_generation_history() {
    let mut s = snapshot();
    let digest = s.assets.insert(b"a small media fixture").unwrap();
    assert_eq!(s.assets.insert(b"a small media fixture").unwrap(), digest);
    assert_eq!(s.assets.len(), 1);
    let unused = s.assets.insert(b"unreferenced").unwrap();
    let canvas = CanvasId::from_bytes([2; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Assets") }.apply(&mut s.project).unwrap();
    let asset = Value::Media(AssetRef { hash: digest, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() });
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "source.image");
    node.history.push(Generation { id: GenerationId::from_bytes([4; 16]), outputs: vec![Value::List(vec![asset])], input_hash: None });
    Change::AddNode { canvas, node }.apply(&mut s.project).unwrap();
    let bytes = encode(&s).unwrap();
    let loaded = decode(&bytes).unwrap();
    assert_eq!(loaded.project, s.project);
    assert_eq!(loaded.assets.get(&digest), s.assets.get(&digest));
    assert!(loaded.assets.get(&unused).is_none());
    let json = manifest(serde_json::to_value(&s.project).unwrap());
    assert!(decode(&zip(&[("project.json", &json)])).is_err());
    assert!(decode(&zip(&[("project.json", &json), (&format!("assets/{digest}"), b"wrong")])).is_err());
    s.assets = AssetStore::default();
    assert!(encode(&s).is_err());
}

#[test]
fn rejects_traversal_unknown_entries_compression_flags_sizes_offsets_crc_and_truncation() {
    let json = manifest(serde_json::to_value(project()).unwrap());
    for name in ["../project.json", "/project.json", "assets/../x", "assets\\x", "extra.txt"] {
        assert!(decode(&zip(&[("project.json", &json), (name, b"evil")])).is_err(), "{name}");
    }
    let valid = zip(&[("project.json", &json)]);
    assert!(decode(&valid).is_ok());
    for len in 0..valid.len() {
        assert!(decode(&valid[..len]).is_err());
    }
    let central = valid.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
    let end = valid.len() - 22;
    for (offset, data) in [
        (end + 10, vec![0xff, 0xff]),
        (end + 16, vec![0xff; 4]),
        (central + 24, vec![0xff; 4]),
        (central + 42, vec![0xff; 4]),
        (central + 10, vec![8, 0]),
        (central + 8, vec![1, 0]),
        (central + 38, vec![0, 0, 0, 0xa0]),
        (30, vec![b'X']),
        (30 + "project.json".len(), vec![0]),
    ] {
        let mut bad = valid.clone();
        bad[offset..offset + data.len()].copy_from_slice(&data);
        assert!(decode(&bad).is_err(), "offset {offset}");
    }
    // Duplicate central entries are rejected before ZipArchive can coalesce identical names.
    let mut duplicate = valid[..end].to_vec();
    duplicate.extend_from_slice(&valid[central..end]);
    duplicate.extend_from_slice(&valid[end..]);
    let new_end = duplicate.len() - 22;
    duplicate[new_end + 8..new_end + 10].copy_from_slice(&2u16.to_le_bytes());
    duplicate[new_end + 10..new_end + 12].copy_from_slice(&2u16.to_le_bytes());
    duplicate[new_end + 12..new_end + 16].copy_from_slice(&((end - central) * 2).to_le_bytes()[..4]);
    assert!(decode(&duplicate).is_err());
}

#[test]
fn journals_replay_edits_tolerate_only_incomplete_tail_and_reject_stale_frames() {
    let s = snapshot();
    let checkpoint = journal::checkpoint(&s).unwrap();
    let change = Changeset::new(vec![Change::SetProjectTitle { title: "Recovered".into() }]);
    let frame = journal::frame(checkpoint.identity, 1, &change, &s.assets, &Default::default()).unwrap();
    let mut bytes = checkpoint.bytes.clone();
    bytes.extend_from_slice(&frame);
    let recovered = journal::recover(&bytes).unwrap();
    assert_eq!(recovered.snapshot.project.title(), "Recovered");
    assert_eq!(recovered.applied, 1);
    assert!(!recovered.truncated_tail);
    for len in 1..frame.len() {
        let mut bytes = checkpoint.bytes.clone();
        bytes.extend_from_slice(&frame[..len]);
        let recovered = journal::recover(&bytes).unwrap();
        assert_eq!(recovered.snapshot.project, s.project);
        assert!(recovered.truncated_tail);
    }
    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() ^= 1;
    let torn = journal::recover(&bad).unwrap();
    assert_eq!(torn.applied, 0);
    assert!(torn.truncated_tail);
    let newer = Snapshot { epoch: 1, ..s };
    let mut stale = journal::checkpoint(&newer).unwrap().bytes;
    stale.extend_from_slice(&frame);
    assert!(journal::recover(&stale).is_err());
    let mut duplicate = bytes;
    duplicate.extend_from_slice(&frame);
    assert!(journal::recover(&duplicate).is_err());
}

#[test]
fn rejects_programmatically_deep_or_oversized_journal_changes() {
    let mut value = Value::Text("leaf".into());
    for _ in 0..1000 {
        value = Value::List(vec![value]);
    }
    let changes = Changeset::new(vec![Change::SetParam {
        canvas: CanvasId::from_bytes([1; 16]),
        id: NodeId::from_bytes([2; 16]),
        name: "value".into(),
        value: Some(value),
    }]);
    assert!(journal::frame(Blake3::from_bytes([0; 32]), 1, &changes, &AssetStore::default(), &Default::default()).is_err());
    // Keep destruction of the adversarial fixture iterative as well.
    let Change::SetParam { value: Some(mut value), .. } = changes.changes.into_iter().next().unwrap() else { panic!() };
    while let Value::List(mut items) = value {
        value = items.pop().unwrap();
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn arbitrary_archive_and_journal_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(),0..8192)) {
        let _=decode(&bytes); let _=journal::recover(&bytes);
    }
    #[test]
    fn mutations_of_valid_archives_are_fallible(at in any::<usize>(), byte in any::<u8>()) {
        let mut bytes=encode(&snapshot()).unwrap();let index=at%bytes.len();bytes[index]=byte;
        if let Ok(loaded)=decode(&bytes) { loaded.project.validate().unwrap(); }
    }
}

#[test]
fn torn_payload_or_zero_tail_preserves_valid_prefix_even_before_later_data() {
    let snapshot = snapshot();
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    let first =
        journal::frame(checkpoint.identity, 1, &Changeset::new(vec![Change::SetProjectTitle { title: "First".into() }]), &snapshot.assets, &Default::default())
            .unwrap();
    let second = journal::frame(
        checkpoint.identity,
        2,
        &Changeset::new(vec![Change::SetProjectTitle { title: "Second".into() }]),
        &snapshot.assets,
        &Default::default(),
    )
    .unwrap();
    let mut prefix = checkpoint.bytes.clone();
    prefix.extend_from_slice(&first);
    let mut zeroed = second.clone();
    zeroed.get_mut(40..).unwrap().fill(0);
    for tail in [zeroed, vec![0; 64]] {
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(&tail);
        let recovered = journal::recover(&bytes).unwrap();
        assert_eq!(recovered.applied, 1);
        assert_eq!(recovered.snapshot.project.title(), "First");
        assert!(recovered.truncated_tail);
    }
    let mut corrupt = first;
    *corrupt.last_mut().unwrap() ^= 1;
    let mut bytes = checkpoint.bytes;
    bytes.extend_from_slice(&corrupt);
    bytes.extend_from_slice(&second);
    let recovered = journal::recover(&bytes).unwrap();
    assert_eq!(recovered.applied, 0);
    assert_eq!(recovered.damage.unwrap().reason, journal::RecoveryDamageReason::ChecksumMismatch);
}

#[test]
fn canonical_hash_bridge_and_mime_and_image_dimension_metadata_are_checked() {
    use nodecraft_format::{canonical_hash, media_hash};
    use nodecraft_media::image::{DynamicImage, ImageFormat};
    let mut png = Cursor::new(Vec::new());
    DynamicImage::new_rgba8(2, 3).write_to(&mut png, ImageFormat::Png).unwrap();
    let bytes = png.into_inner();
    let media = nodecraft_media::ContentHash::of(&bytes);
    assert_eq!(media_hash(canonical_hash(media)), media);
    let mut snapshot = snapshot();
    let hash = snapshot.assets.insert(&bytes).unwrap();
    assert_eq!(canonical_hash(media), hash);
    let canvas = CanvasId::from_bytes([2; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Meta") }.apply(&mut snapshot.project).unwrap();
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "source.image");
    node.params.insert(
        "asset".into(),
        Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(2), height: Some(3), ..Default::default() } }),
    );
    Change::AddNode { canvas, node: node.clone() }.apply(&mut snapshot.project).unwrap();
    assert!(encode(&snapshot).is_ok());
    for (mime, width) in [("image/jpeg", 2), ("image/png", 99)] {
        node.params
            .insert("asset".into(), Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: MediaMeta { width: Some(width), ..Default::default() } }));
        Change::SetNode { canvas, node: node.clone() }.apply(&mut snapshot.project).unwrap();
        assert!(encode(&snapshot).is_err());
    }
}

#[test]
fn public_journal_payload_preflight_includes_generation_depth_counts_and_record_bytes() {
    let id = Blake3::from_bytes([0; 32]);
    let assets = AssetStore::default();
    let too_many = Changeset::new(vec![Change::SetProjectTitle { title: "x".into() }; 10001]);
    assert!(journal::frame(id, 1, &too_many, &assets, &Default::default()).is_err());
    let large = Changeset::new(
        (0..17_u8)
            .map(|n| Change::PutComment {
                canvas: CanvasId::from_bytes([1; 16]),
                comment: nodecraft_graph::Comment {
                    id: nodecraft_types::CommentId::from_bytes([n; 16]),
                    text: "x".repeat(1024 * 1024),
                    pos: Default::default(),
                },
            })
            .collect(),
    );
    assert!(large.validate_payload().is_ok());
    assert!(journal::frame(id, 1, &large, &assets, &Default::default()).is_err());
    let mut value = Value::Text("x".into());
    for _ in 0..1000 {
        value = Value::List(vec![value]);
    }
    let changes = Changeset::new(vec![Change::AddGeneration {
        canvas: CanvasId::from_bytes([1; 16]),
        id: NodeId::from_bytes([2; 16]),
        index: 0,
        generation: Generation { id: GenerationId::from_bytes([3; 16]), outputs: vec![value], input_hash: None },
    }]);
    assert!(journal::frame(id, 1, &changes, &assets, &Default::default()).is_err());
    let Change::AddGeneration { mut generation, .. } = changes.changes.into_iter().next().unwrap() else { panic!() };
    let mut value = generation.outputs.pop().unwrap();
    while let Value::List(mut values) = value {
        value = values.pop().unwrap();
    }
}

#[test]
fn no_invented_legacy_migration_or_future_data_is_silently_loaded() {
    for version in [0, 2, u32::MAX] {
        let json = serde_json::to_vec(&json!({"version":version,"project":{"unknown":"future"}})).unwrap();
        assert_eq!(decode(&zip(&[("project.json", &json)])).unwrap_err().kind, ErrorKind::Unsupported);
    }
}
