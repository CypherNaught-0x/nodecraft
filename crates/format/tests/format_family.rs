#![cfg(not(target_arch = "wasm32"))]
//! Real Comments-v2 writer output must never be interpreted as a streaming checkpoint.
use nodecraft_format::{
    NativeStorage,
    asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel},
    zip_index::ArchiveIndex,
};
use nodecraft_types::ErrorKind;
use std::sync::Arc;

#[test]
fn genuine_comments_v2_is_unsupported_by_streaming_readers() {
    let archive = include_bytes!("fixtures/format-family/comments-v2.ncraft");
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(archive.as_slice()))), 0, archive.len() as u64).unwrap();
    assert_eq!(ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default()).err().unwrap().kind, ErrorKind::Unsupported);
    assert_eq!(nodecraft_format::decode(archive).unwrap_err().kind, ErrorKind::Unsupported);
    let journal = include_bytes!("fixtures/format-family/comments-v2.ncrj");
    assert_eq!(nodecraft_format::journal::recover(journal).err().unwrap().kind, ErrorKind::Unsupported);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    std::fs::write(&path, journal).unwrap();
    let storage = NativeStorage::new(path.clone()).unwrap();
    assert_eq!(storage.recover().err().unwrap().kind, ErrorKind::Unsupported);
    assert_eq!(std::fs::read(path).unwrap(), journal);
}

#[test]
fn reserved_graph_journal_versions_refuse_before_streaming_header_parsing() {
    for magic in [b"NCRJ0002", b"NCRJ0003"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery");
        std::fs::write(&path, magic).unwrap();
        let storage = NativeStorage::new(path.clone()).unwrap();
        assert_eq!(storage.recover().err().unwrap().kind, ErrorKind::Unsupported);
        assert_eq!(std::fs::read(path).unwrap(), magic);
    }
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
