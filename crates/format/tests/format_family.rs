#![cfg(not(target_arch = "wasm32"))]
//! Historical graph families remain readable through native streaming-capable hosts.
use nodecraft_format::{
    ArchivePlan, CheckpointPlan, NativeStorage, ProjectStorage, StorageLocation,
    asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel},
    zip_index::ArchiveIndex,
};
use std::io::{Cursor, Read, Write};
use std::sync::Arc;

fn current_v3() -> nodecraft_format::Snapshot {
    use nodecraft_graph::{Change, Field, FieldRect, Position, Tag};
    use nodecraft_types::{FieldId, TagId};
    let mut snapshot = nodecraft_format::decode(include_bytes!("fixtures/v2/comments-v2.ncraft")).unwrap();
    let canvas = *snapshot.project.canvases().keys().next().unwrap();
    let tag = TagId::from_bytes([73; 16]);
    let field = FieldId::from_bytes([7; 16]);
    Change::PutTag { canvas, tag: Tag { id: tag, title: "Graph-v3 tag".into(), color: Some("blue".into()) } }.apply(&mut snapshot.project).unwrap();
    let mut zone = Field::new(field, "Graph-v3 field");
    zone.rect = FieldRect { pos: Position { x: -120.0, y: 40.0 }, width: 640.0, height: 480.0 };
    zone.color = Some("orange".into());
    zone.collapsed = true;
    Change::PutField { canvas, field: zone }.apply(&mut snapshot.project).unwrap();
    let mut node = snapshot.project.canvases().get(&canvas).unwrap().nodes().values().next().unwrap().clone();
    node.tag = Some(tag);
    node.field = Some(field);
    Change::SetNode { canvas, node }.apply(&mut snapshot.project).unwrap();
    assert_v3_features(&snapshot);
    snapshot
}

fn assert_v3_features(snapshot: &nodecraft_format::Snapshot) {
    use nodecraft_types::{CanvasId, CommentId, FieldId, NodeId, TagId};
    let canvas = snapshot.project.canvases().get(&CanvasId::from_bytes([1; 16])).unwrap();
    let tag = TagId::from_bytes([73; 16]);
    let field = FieldId::from_bytes([7; 16]);
    let node = NodeId::from_bytes([2; 16]);
    assert_eq!(snapshot.epoch, 11);
    assert_eq!(canvas.tags().get(&tag).unwrap().color.as_deref(), Some("blue"));
    let zone = canvas.fields().get(&field).unwrap();
    assert_eq!(zone.title, "Graph-v3 field");
    assert_eq!((zone.rect.pos.x, zone.rect.pos.y, zone.rect.width, zone.rect.height), (-120.0, 40.0, 640.0, 480.0));
    assert_eq!(zone.color.as_deref(), Some("orange"));
    assert!(zone.collapsed);
    assert_eq!(canvas.nodes().get(&node).unwrap().tag, Some(tag));
    assert_eq!(canvas.nodes().get(&node).unwrap().field, Some(field));
    let comment = canvas.comments().get(&CommentId::from_bytes([4; 16])).unwrap();
    assert_eq!(comment.text, "Review this variation");
    assert_eq!(comment.node, Some(node));
    assert_eq!((comment.pos.x, comment.pos.y), (340.0, 24.0));
    assert!(comment.resolved);
    assert_eq!(comment.replies.len(), 1);
    assert_eq!(comment.replies[0].id, CommentId::from_bytes([5; 16]));
    assert_eq!(comment.replies[0].text, "Keep the muted palette");
}

fn manifest(bytes: &[u8]) -> serde_json::Value {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut json = Vec::new();
    zip.by_name("project.json").unwrap().read_to_end(&mut json).unwrap();
    serde_json::from_slice(&json).unwrap()
}

/// Synthetic negative/extension controls only; genuine interoperability bytes use plans below.
fn rewrite_manifest(bytes: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut json = manifest(bytes);
    edit(&mut json);
    let mut input = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..input.len() {
        let mut entry = input.by_index(index).unwrap();
        let name = entry.name().unwrap().into_owned();
        output.start_file(&name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        if name == "project.json" {
            output.write_all(&serde_json::to_vec(&json).unwrap()).unwrap();
        } else {
            std::io::copy(&mut entry, &mut output).unwrap();
        }
    }
    output.finish().unwrap().into_inner()
}

fn assert_native_asset(snapshot: &nodecraft_format::Snapshot, asset: &nodecraft_types::AssetRef, expected: &[u8]) {
    assert!(snapshot.assets.get(&asset.hash).is_none());
    let verified = snapshot.assets.open_verified(asset, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(verified.byte_len(), expected.len() as u64);
    let mut reader = verified.reader(StreamMetrics::default());
    let mut hash = blake3::Hasher::new();
    let mut crc = crc32fast::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    loop {
        let n = reader.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        bytes += n;
        hash.update(&buffer[..n]);
        crc.update(&buffer[..n]);
    }
    assert_eq!(bytes, expected.len());
    assert_eq!(hash.finalize().as_bytes(), blake3::hash(expected).as_bytes());
    assert_eq!(crc.finalize(), crc32fast::hash(expected));
}

#[test]
fn genuine_comments_v2_and_current_v3_open_and_native_save_decode() {
    let original = include_bytes!("fixtures/v2/comments-v2.ncraft");
    assert_eq!(original.as_slice(), include_bytes!("fixtures/format-family/comments-v2.ncraft"));
    let v3 = nodecraft_format::encode(&current_v3()).unwrap();
    assert_eq!(manifest(&v3)["version"], 3, "real current writer remains graph-v3, independently of named constants");
    for (bytes, version) in [(original.as_slice(), 2), (v3.as_slice(), 3)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.ncraft");
        std::fs::write(&path, bytes).unwrap();
        let mut storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64).unwrap();
        let index = ArchiveIndex::read(&range, &cancel, &metrics).unwrap();
        assert_eq!(index.version(), version);
        let (snapshot, checkpoint, _) = storage.prepare_open_snapshot(&StorageLocation::Path(path), &cancel, &metrics).unwrap().into_parts();
        assert_eq!(snapshot.project, nodecraft_format::decode(bytes).unwrap().project);
        assert!(snapshot.project.canvases().values().any(|canvas| !canvas.comments().is_empty()));
        storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
        storage.commit_open();
        let plan = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
        let save = dir.path().join("new.ncraft");
        storage.save_plan(&StorageLocation::Path(save.clone()), &plan, &cancel, &metrics).unwrap();
        let saved = std::fs::read(save).unwrap();
        assert_eq!(manifest(&saved)["version"], 3, "genuine small native ArchivePlan keeps classic wire3");
        let decoded = nodecraft_format::decode(&saved).unwrap();
        assert_eq!(decoded.project, snapshot.project);
        if version == 3 {
            assert_v3_features(&decoded);
        }
    }
}

#[test]
fn native_recovers_real_ncrj2_with_original_identity_then_migrates_before_append() {
    use nodecraft_graph::{Change, Changeset};
    let bytes = include_bytes!("fixtures/v2/comments-v2.ncrj");
    assert_eq!(bytes.as_slice(), include_bytes!("fixtures/format-family/comments-v2.ncrj"));
    let expected = nodecraft_format::journal::recover(bytes).unwrap();
    assert_eq!(expected.applied, 3);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    std::fs::write(&path, bytes).unwrap();
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let recovered = storage.recover().unwrap();
    assert!(recovered.legacy);
    assert_eq!(recovered.applied, 3);
    assert_eq!(recovered.snapshot.project, expected.snapshot.project);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut snapshot = recovered.snapshot;
    snapshot.assets = snapshot.assets.in_context(storage.asset_context().unwrap(), &cancel, &metrics).unwrap();
    let plan = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(plan).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    assert_eq!(&std::fs::read(&path).unwrap()[..8], b"NCRJ0004");
    let changes = Changeset::new(vec![Change::SetProjectTitle { title: "Recovered and edited".into() }]);
    let admitted = nodecraft_format::journal::AdmittedReplay::new(changes.clone(), &snapshot.assets).unwrap();
    let recorded = nodecraft_format::RecordedAssets::new(checkpoint.asset_lengths()).unwrap();
    let (frame, _) = admitted.into_plan(checkpoint.identity(), 1, &recorded, storage.capability(), &cancel, &metrics).unwrap();
    storage.append_plan(&frame, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    changes.apply(&mut snapshot.project).unwrap();
    assert_eq!(storage.recover().unwrap().snapshot.project, snapshot.project);
}

#[test]
#[ignore = "writes actual v4 interop controls to NODECRAFT_FORMAT_FAMILY_EVIDENCE"]
fn write_streaming_family_interop_controls() {
    use nodecraft_format::{ArchivePlan, AssetStore, CheckpointPlan, ProjectStorage, Snapshot};
    use nodecraft_graph::{Canvas, Change, Node, Project};
    use nodecraft_types::{AssetRef, CanvasId, MediaMeta, NodeId, ProjectId, Value};
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_FORMAT_FAMILY_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let mut assets = AssetStore::with_context(storage.asset_context().unwrap());
    let bytes: Arc<[u8]> = vec![b'x'; nodecraft_format::MAX_ASSET_BYTES + 1].into();
    let length = bytes.len() as u64;
    let source = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, length).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let hash = assets.import_stream(&source, &cancel, &metrics).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Public format-family control").unwrap();
    let canvas = CanvasId::from_bytes([2; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "source.text");
    node.params.insert("content".into(), Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let plan = ArchivePlan::prepare(&Snapshot { project, assets, epoch: 0 }, storage.capability(), &cancel, &metrics).unwrap();
    let mut archive = std::fs::File::create(folder.join("streaming-v4.ncraft")).unwrap();
    plan.write_to(&mut archive, &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(plan).unwrap();
    let mut journal = std::fs::File::create(folder.join("streaming-v4.ncrj")).unwrap();
    checkpoint.write_to(&mut journal, &cancel, &metrics).unwrap();
}

#[test]
fn genuine_streaming_plans_refuse_byte_hosts_and_preserve_graph_v3_natively() {
    use nodecraft_graph::Change;
    use nodecraft_types::{AssetRef, ErrorKind, MediaMeta, Value};
    let dir = tempfile::tempdir().unwrap();
    let mut storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut snapshot = current_v3();
    snapshot.assets = snapshot.assets.in_context(storage.asset_context().unwrap(), &cancel, &metrics).unwrap();
    let bytes: Arc<[u8]> = vec![b'x'; nodecraft_format::MAX_ASSET_BYTES + 1].into();
    let range = AssetRange::new(Arc::new(MemorySource::new(bytes.clone())), 0, bytes.len() as u64).unwrap();
    let hash = snapshot.assets.import_stream(&range, &cancel, &metrics).unwrap();
    let asset = AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() };
    let canvas = *snapshot.project.canvases().keys().next().unwrap();
    let id = *snapshot.project.canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    Change::SetParam { canvas, id, name: "large-native".into(), value: Some(Value::Media(asset.clone())) }.apply(&mut snapshot.project).unwrap();
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let mut archive_bytes = Vec::new();
    archive.write_to(&mut archive_bytes, &cancel, &metrics).unwrap();
    assert_eq!(archive_bytes.len() as u64, archive.byte_len());
    assert!(archive_bytes.len() <= nodecraft_format::MAX_ARCHIVE_BYTES);
    assert_eq!(manifest(&archive_bytes)["version"], 4);
    assert_eq!(nodecraft_format::decode(&archive_bytes).unwrap_err().kind, ErrorKind::Unsupported);
    let public = StorageLocation::Path(dir.path().join("streaming.ncraft"));
    storage.save_plan(&public, &archive, &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive).unwrap();
    let mut checkpoint_bytes = Vec::new();
    checkpoint.write_to(&mut checkpoint_bytes, &cancel, &metrics).unwrap();
    assert_eq!(checkpoint_bytes.len() as u64, checkpoint.byte_len());
    assert!(checkpoint_bytes.len() <= nodecraft_format::MAX_ARCHIVE_BYTES);
    assert_eq!(&checkpoint_bytes[..8], b"NCRJ0004");
    assert_eq!(nodecraft_format::journal::recover(&checkpoint_bytes).err().unwrap().kind, ErrorKind::Unsupported);
    let embedded = AssetRange::new(Arc::new(MemorySource::new(Arc::from(checkpoint_bytes))), 80, checkpoint.archive().byte_len()).unwrap();
    assert_eq!(ArchiveIndex::read(&embedded, &cancel, &metrics).unwrap().version(), 4);
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let recovered = storage.recover().unwrap();
    assert!(!recovered.legacy);
    assert_eq!(recovered.snapshot.project, snapshot.project);
    assert_v3_features(&recovered.snapshot);
    assert_native_asset(&recovered.snapshot, &asset, &bytes);
    // Release the saving session's destination lock before the independent Open.
    drop(storage);
    let mut opener = NativeStorage::new(dir.path().join("other-recovery")).unwrap();
    let (opened, checkpoint, _) = opener.prepare_open_snapshot(&public, &cancel, &metrics).unwrap().into_parts();
    assert_eq!(opened.project, snapshot.project);
    assert_v3_features(&opened);
    assert_native_asset(&opened, &asset, &bytes);
    opener.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    opener.commit_open();
    assert_eq!(opener.recover().unwrap().snapshot.project, snapshot.project);
}

#[test]
fn graph_v3_typed_vocabulary_is_checked_in_classic_and_streaming_families() {
    use nodecraft_types::ErrorKind;
    let original = nodecraft_format::encode(&current_v3()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let canvas = "01010101-0101-0101-0101-010101010101";
    let node = "02020202-0202-0202-0202-020202020202";
    let comment = "04040404-0404-0404-0404-040404040404";
    for version in [3, 4] {
        // These synthetic controls exercise typed admission, not writer interoperability.
        let valid = rewrite_manifest(&original, |m| {
            m["version"] = version.into();
            m["harmless_extension"] = true.into();
        });
        let decode = |bytes: &[u8]| {
            if version == 3 {
                nodecraft_format::decode(bytes)
            } else {
                let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64).unwrap();
                let index = ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
                nodecraft_format::decode_indexed(&index, storage.asset_context().unwrap())
            }
        };
        assert_v3_features(&decode(&valid).unwrap());
        for case in 0..4 {
            let invalid = rewrite_manifest(&valid, |m| {
                let c = &mut m["project"]["canvases"][canvas];
                match case {
                    0 => c["nodes"][node]["params"]["text"]["type"] = "future_value".into(),
                    1 => c["nodes"][node]["outputs"] = serde_json::json!([{"name":"future", "port_type":{"modality":"future_modality", "many":false}}]),
                    2 => c["comments"][comment]["resolved"] = "future_state".into(),
                    _ => c["comments"][comment]["replies"] = serde_json::json!({"future":"shape"}),
                }
            });
            assert_eq!(decode(&invalid).unwrap_err().kind, ErrorKind::InvalidInput, "wire {version}, vocabulary case {case}");
        }
    }
    let future = rewrite_manifest(&original, |m| {
        m["version"] = 5.into();
        m["project"] = serde_json::json!({"future":"not graph-v3"});
    });
    assert_eq!(nodecraft_format::decode(&future).unwrap_err().kind, ErrorKind::Unsupported);
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(future.clone()))), 0, future.len() as u64).unwrap();
    assert_eq!(ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).err().unwrap().kind, ErrorKind::Unsupported);
}

#[test]
fn small_host_open_migrates_classic_graph_before_current_checkpoint() {
    for bytes in [include_bytes!("fixtures/v1/comments-v1.ncraft").as_slice(), include_bytes!("fixtures/v2/comments-v2.ncraft").as_slice()] {
        let snapshot = nodecraft_format::decode(bytes).unwrap();
        let mut storage = nodecraft_format::MemoryStorage::default();
        let location = StorageLocation::Path("classic.ncraft".into());
        storage.save(&location, bytes).unwrap();
        let (_, checkpoint, _) = storage.prepare_open_snapshot(&location, &TransferCancel::default(), &StreamMetrics::default()).unwrap().into_parts();
        let recovered = nodecraft_format::journal::recover(checkpoint.small_bytes().unwrap()).unwrap();
        assert_eq!(recovered.snapshot.project, snapshot.project);
        assert!(!recovered.legacy);
    }
}

// Preserve current main's byte-host boundary: supporting native v4 must not cause a
// legacy byte reader to parse a streaming-family graph or 80-byte checkpoint.
#[test]
fn streaming_family_is_unsupported_before_graph_or_checkpoint_parsing() {
    use nodecraft_types::ErrorKind;
    use std::io::{Cursor, Write};
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("project.json", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
    zip.write_all(br#"{"version":4,"project":{"future":"not a graph"}}"#).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    assert_eq!(nodecraft_format::decode(&bytes).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(nodecraft_format::journal::recover(b"NCRJ0004").err().unwrap().kind, ErrorKind::Unsupported);
}
