#![cfg(not(target_arch = "wasm32"))]
//! Historical graph families remain readable through native streaming-capable hosts.
use nodecraft_format::{
    ArchivePlan, CheckpointPlan, NativeStorage, ProjectStorage, StorageLocation,
    asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel},
    zip_index::ArchiveIndex,
};
use std::sync::Arc;

#[test]
fn genuine_comments_v2_and_current_v3_open_and_native_save_decode() {
    let original = include_bytes!("fixtures/v2/comments-v2.ncraft");
    assert_eq!(original.as_slice(), include_bytes!("fixtures/format-family/comments-v2.ncraft"));
    let v2 = nodecraft_format::decode(original).unwrap();
    let mut tags = v2.clone();
    let canvas = *tags.project.canvases().keys().next().unwrap();
    nodecraft_graph::Change::PutTag {
        canvas,
        tag: nodecraft_graph::Tag { id: nodecraft_types::TagId::from_bytes([71; 16]), title: "V3 native interop".into(), color: Some("blue".into()) },
    }
    .apply(&mut tags.project)
    .unwrap();
    let v3 = nodecraft_format::encode(&tags).unwrap();
    for bytes in [original.as_slice(), v3.as_slice()] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.ncraft");
        std::fs::write(&path, bytes).unwrap();
        let mut storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64).unwrap();
        let index = ArchiveIndex::read(&range, &cancel, &metrics).unwrap();
        assert!([2, 3].contains(&index.version()));
        let (snapshot, checkpoint, _) = storage.prepare_open_snapshot(&StorageLocation::Path(path), &cancel, &metrics).unwrap().into_parts();
        assert_eq!(snapshot.project, nodecraft_format::decode(bytes).unwrap().project);
        assert!(snapshot.project.canvases().values().any(|canvas| !canvas.comments().is_empty()));
        storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
        storage.commit_open();
        let plan = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
        let save = dir.path().join("new.ncraft");
        storage.save_plan(&StorageLocation::Path(save.clone()), &plan, &cancel, &metrics).unwrap();
        assert_eq!(nodecraft_format::decode(&std::fs::read(save).unwrap()).unwrap().project, snapshot.project);
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
fn streaming_v4_carries_current_comment_and_tag_graph_through_recovery() {
    use nodecraft_graph::{Change, Tag};
    use nodecraft_types::{AssetRef, MediaMeta, TagId, Value};
    let dir = tempfile::tempdir().unwrap();
    let mut storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut snapshot = nodecraft_format::decode(include_bytes!("fixtures/v2/comments-v2.ncraft")).unwrap();
    snapshot.assets = snapshot.assets.in_context(storage.asset_context().unwrap(), &cancel, &metrics).unwrap();
    let bytes: Arc<[u8]> = vec![b'x'; nodecraft_format::MAX_ASSET_BYTES + 1].into();
    let range = AssetRange::new(Arc::new(MemorySource::new(bytes.clone())), 0, bytes.len() as u64).unwrap();
    let hash = snapshot.assets.import_stream(&range, &cancel, &metrics).unwrap();
    let canvas = *snapshot.project.canvases().keys().next().unwrap();
    let id = *snapshot.project.canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    Change::SetParam {
        canvas,
        id,
        name: "large-native".into(),
        value: Some(Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })),
    }
    .apply(&mut snapshot.project)
    .unwrap();
    Change::PutTag { canvas, tag: Tag { id: TagId::from_bytes([73; 16]), title: "Streaming tag".into(), color: Some("blue".into()) } }
        .apply(&mut snapshot.project)
        .unwrap();
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.snapshot.project, snapshot.project);
    assert!(!recovered.legacy);
    let mut bytes = Vec::new();
    checkpoint.write_to(&mut bytes, &cancel, &metrics).unwrap();
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 80, checkpoint.archive().byte_len()).unwrap();
    assert_eq!(ArchiveIndex::read(&range, &cancel, &metrics).unwrap().version(), 4);
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
