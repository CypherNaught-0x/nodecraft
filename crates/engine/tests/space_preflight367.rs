//! The advisory disk-space preflight on real native storage (#367): a save or open that the
//! platform says can't fit is refused before anything is written, with sizes and no paths, and
//! leaves the file, the open project and recovery exactly as they were.
use nodecraft_engine::Session;
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, CheckpointPlan, FramePlan, LoadedProject, NativeStorage, PreparedSnapshot, ProjectAccess, ProjectStorage, Snapshot,
    StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
    space::SpaceProbe,
};
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Native storage whose free-space report the test sets, as a nearly full disk would give it.
/// `None` keeps the real report.
struct Tight {
    native: NativeStorage,
    free: Free,
}
impl ProjectStorage for Tight {
    fn project_identity_scope(&self, l: &StorageLocation) -> Result<Blake3, Error> {
        self.native.project_identity_scope(l)
    }
    fn capability(&self) -> StorageCapability {
        self.native.capability()
    }
    fn asset_context(&self) -> Option<AssetContext> {
        self.native.asset_context()
    }
    fn prepare_open_snapshot(&mut self, l: &StorageLocation, c: &TransferCancel, m: &StreamMetrics) -> Result<PreparedSnapshot, Error> {
        self.native.prepare_open_snapshot(l, c, m)
    }
    fn save_plan(&mut self, l: &StorageLocation, p: &ArchivePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<ProjectAccess, Error> {
        self.native.save_plan(l, p, c, m)
    }
    fn checkpoint_plan(&mut self, p: &CheckpointPlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        self.native.checkpoint_plan(p, c, m)
    }
    fn append_plan(&mut self, p: &FramePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        self.native.append_plan(p, c, m)
    }
    fn space_probe(&self, l: &StorageLocation) -> SpaceProbe {
        let real = self.native.space_probe(l);
        match *self.free.lock().unwrap() {
            None => real,
            Some((destination_free, recovery_free, shared)) => SpaceProbe { destination_free, recovery_free, shared, existing_bytes: real.existing_bytes },
        }
    }
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.native.load(l)
    }
    fn prepare_open(&mut self, l: &StorageLocation) -> Result<LoadedProject, Error> {
        self.native.prepare_open(l)
    }
    fn commit_open(&mut self) {
        self.native.commit_open();
    }
    fn cancel_open(&mut self) {
        self.native.cancel_open();
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.native.save(l, b)
    }
    fn save_with_access(&mut self, l: &StorageLocation, b: &[u8]) -> Result<ProjectAccess, Error> {
        self.native.save_with_access(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.native.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.native.append(b)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.native.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.native.discard()
    }
}

const C: CanvasId = CanvasId::from_bytes([5; 16]);
fn project(id: u8) -> Project {
    let mut project = Project::new(ProjectId::from_bytes([id; 16]), "Space").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: Node::new(NodeId::from_bytes([id; 16]), "source.text") }.apply(&mut project).unwrap();
    project
}
type Free = Arc<Mutex<Option<(Option<u64>, Option<u64>, bool)>>>;
fn session(dir: &std::path::Path, journal: &str, id: u8) -> (Session, Free) {
    let free = Free::default();
    let storage = Tight { native: NativeStorage::new(dir.join(journal)).unwrap(), free: free.clone() };
    let session = Session::with_sendable_storage(Snapshot { project: project(id), assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    (session, free)
}
fn add(session: &mut Session, n: u8) {
    session.dispatch("node.add", &json!({"canvas":C,"node":Node::new(NodeId::from_bytes([n; 16]), "source.text")})).unwrap();
}
fn stray_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn a_save_that_cannot_fit_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    std::fs::create_dir(&projects).unwrap();
    let path = projects.join("tight.ncraft");
    let (mut session, free) = session(dir.path(), "journal", 1);
    session.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
    let on_disk = std::fs::read(&path).unwrap();
    let names = stray_files(&projects);
    add(&mut session, 2);
    let edited = session.document().clone();
    let journal = std::fs::read(dir.path().join("journal")).unwrap();

    for (probe, drive) in
        [((Some(10), Some(u64::MAX), false), "the drive you are saving to"), ((Some(10), Some(10), true), "project and NodeCraft's recovery folder")]
    {
        *free.lock().unwrap() = Some(probe);
        let error = session.dispatch("file.save", &json!({})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy);
        assert!(error.message.starts_with("Not enough free disk space to save"), "{}", error.message);
        assert!(error.message.contains(drive) && error.message.contains("10 bytes is free"), "{}", error.message);
        assert!(!error.message.contains(dir.path().to_str().unwrap()), "no private paths: {}", error.message);
        assert_eq!(std::fs::read(&path).unwrap(), on_disk, "the file is unchanged");
        assert_eq!(stray_files(&projects), names, "no staged file is left beside it");
        assert_eq!(std::fs::read(dir.path().join("journal")).unwrap(), journal, "recovery is untouched");
        assert_eq!(session.document(), &edited);
        let status = session.file_status();
        assert!(status.dirty && !status.recovery_required && status.operation.is_none());
    }
    // The refused save didn't latch anything: editing journals and a save with room works.
    add(&mut session, 3);
    *free.lock().unwrap() = Some((Some(u64::MAX), Some(u64::MAX), true));
    session.dispatch("file.save", &json!({})).unwrap();
    assert!(!session.file_status().dirty);
    assert_eq!(nodecraft_format::decode(&std::fs::read(&path).unwrap()).unwrap().project, *session.document());
}

#[test]
fn an_open_that_cannot_fit_keeps_the_current_project() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.ncraft");
    {
        let (mut other, _) = session(dir.path(), "seed", 7);
        other.dispatch("file.save_as", &json!({"location":target.to_str().unwrap()})).unwrap();
        other.close_recovery(false).unwrap();
    }
    let (mut session, free) = session(dir.path(), "journal", 1);
    add(&mut session, 2);
    let before = session.document().clone();
    *free.lock().unwrap() = Some((None, Some(1), false));
    let error = session.dispatch("file.open", &json!({"location":target.to_str().unwrap(),"discard_dirty":true})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.message.contains("open this project") && error.message.contains("recovery folder"), "{}", error.message);
    assert!(error.message.contains("current project is unchanged"), "{}", error.message);
    assert_eq!(session.document(), &before);
    assert!(session.file_status().location.is_none());
    add(&mut session, 3);
    // Unknown free space never refuses: the open goes ahead and the real writes decide.
    *free.lock().unwrap() = Some((None, None, false));
    session.dispatch("file.open", &json!({"location":target.to_str().unwrap(),"discard_dirty":true})).unwrap();
    assert_eq!(session.document(), &project(7));
}

#[test]
fn the_real_platform_report_lets_an_ordinary_save_and_open_through() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("real.ncraft");
    let (mut session, _) = session(dir.path(), "journal", 1);
    let probe = NativeStorage::new(dir.path().join("probe")).unwrap().space_probe(&StorageLocation::Path(path.clone()));
    assert!(probe.destination_free.is_some() && probe.recovery_free.is_some(), "a native folder reports its free space");
    assert!(probe.shared, "one temporary folder is one volume");
    assert_eq!(probe.existing_bytes, None);
    session.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
    let probe = NativeStorage::new(dir.path().join("probe2")).unwrap().space_probe(&StorageLocation::Path(path.clone()));
    assert_eq!(probe.existing_bytes, Some(std::fs::metadata(&path).unwrap().len()));
    session.dispatch("file.open", &json!({"location":path.to_str().unwrap()})).unwrap();
}
