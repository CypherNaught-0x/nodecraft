use super::*;
use nodecraft_format::{AssetStore, MemoryStorage, Snapshot, StorageLocation};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;

#[test]
fn legacy_cli_runs_reuse_desktop_identity_without_writing_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("public.ncraft");
    let legacy = ProjectId::from_bytes([2; 16]);
    let snapshot = Snapshot { project: Project::new(legacy, "Public legacy fixture").unwrap(), assets: AssetStore::default(), epoch: 0 };
    let bytes = nodecraft_format::encode(&snapshot).unwrap();
    std::fs::write(&file, &bytes).unwrap();
    let first = open_with_assets(&file).unwrap();
    let second = open_with_assets(&dir.path().join(".").join("public.ncraft")).unwrap();
    assert_ne!(first.document().id(), legacy);
    assert_eq!(first.document().id(), second.document().id());
    let plain = open(&file).unwrap();
    assert_eq!(plain.document().id(), legacy, "inspection remains read-only");
    let desktop = Session::with_storage_origin(snapshot, Box::<MemoryStorage>::default(), StorageLocation::Path(file.canonicalize().unwrap())).unwrap();
    assert_eq!(desktop.document().id(), first.document().id());
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
}
