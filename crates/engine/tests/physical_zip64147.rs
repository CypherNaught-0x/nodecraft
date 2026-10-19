#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::Session;
use nodecraft_format::{
    ArchivePlan, AssetContext, AssetStore, CheckpointPlan, FramePlan, LoadedProject, NativeStorage, PreparedSnapshot, ProjectAccess, ProjectStorage, Snapshot,
    StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId, Value};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
struct Host {
    native: NativeStorage,
    checkpoints: usize,
    appends: usize,
    saves: usize,
    opens: usize,
    metrics: StreamMetrics,
    archive_bytes: u64,
    checkpoint_bytes: u64,
    checkpoint_identity: Option<nodecraft_types::Blake3>,
    completed_output_bytes: u64,
    extent_cap: u64,
}
#[derive(Clone)]
struct TypedOnly(Rc<RefCell<Host>>);
fn legacy() -> Error {
    Error::new(ErrorKind::Internal, "legacy whole-byte route must not be used by native Session")
}
impl ProjectStorage for TypedOnly {
    fn capability(&self) -> StorageCapability {
        StorageCapability::StreamingV2
    }
    fn asset_context(&self) -> Option<AssetContext> {
        self.0.borrow().native.asset_context()
    }
    fn prepare_open_snapshot(&mut self, l: &StorageLocation, c: &TransferCancel, m: &StreamMetrics) -> Result<PreparedSnapshot, Error> {
        let mut host = self.0.borrow_mut();
        host.opens += 1;
        host.metrics = m.clone();
        host.native.prepare_open_snapshot(l, c, m)
    }
    fn save_plan(&mut self, l: &StorageLocation, p: &ArchivePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<ProjectAccess, Error> {
        let mut host = self.0.borrow_mut();
        host.saves += 1;
        host.metrics = m.clone();
        assert!(p.byte_len() <= host.extent_cap, "archive exceeded reviewed extent allowance");
        let access = host.native.save_plan(l, p, c, m)?;
        host.archive_bytes = p.byte_len();
        host.completed_output_bytes = host.completed_output_bytes.checked_add(p.byte_len()).unwrap();
        Ok(access)
    }
    fn checkpoint_plan(&mut self, p: &CheckpointPlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        let mut host = self.0.borrow_mut();
        host.checkpoints += 1;
        host.metrics = m.clone();
        assert!(p.byte_len() <= host.extent_cap, "checkpoint exceeded reviewed extent allowance");
        host.native.checkpoint_plan(p, c, m)?;
        host.checkpoint_bytes = p.byte_len();
        host.checkpoint_identity = Some(p.identity());
        host.completed_output_bytes = host.completed_output_bytes.checked_add(p.byte_len()).unwrap();
        Ok(())
    }
    fn append_plan(&mut self, p: &FramePlan, c: &TransferCancel, m: &StreamMetrics) -> Result<(), Error> {
        let mut host = self.0.borrow_mut();
        host.appends += 1;
        host.metrics = m.clone();
        assert!(p.byte_len() <= host.extent_cap, "tail frame exceeded reviewed extent allowance");
        host.native.append_plan(p, c, m)?;
        host.completed_output_bytes = host.completed_output_bytes.checked_add(p.byte_len()).unwrap();
        Ok(())
    }
    fn commit_open(&mut self) {
        self.0.borrow_mut().native.commit_open();
    }
    fn cancel_open(&mut self) {
        self.0.borrow_mut().native.cancel_open();
    }
    fn load(&mut self, _: &StorageLocation) -> Result<Vec<u8>, Error> {
        Err(legacy())
    }
    fn prepare_open(&mut self, _: &StorageLocation) -> Result<LoadedProject, Error> {
        Err(legacy())
    }
    fn save(&mut self, _: &StorageLocation, _: &[u8]) -> Result<(), Error> {
        Err(legacy())
    }
    fn checkpoint(&mut self, _: &[u8]) -> Result<(), Error> {
        Err(legacy())
    }
    fn append(&mut self, _: &[u8]) -> Result<(), Error> {
        Err(legacy())
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().native.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().native.discard()
    }
}
fn host(dir: &std::path::Path) -> TypedOnly {
    TypedOnly(Rc::new(RefCell::new(Host {
        native: NativeStorage::new(dir.join("recovery")).unwrap(),
        checkpoints: 0,
        appends: 0,
        saves: 0,
        opens: 0,
        metrics: StreamMetrics::default(),
        archive_bytes: 0,
        checkpoint_bytes: 0,
        checkpoint_identity: None,
        completed_output_bytes: 0,
        extent_cap: u64::MAX,
    })))
}
fn snapshot(assets: AssetStore) -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Lazy Session").unwrap();
    let mut node = Node::new(N, "source.text");
    node.params.insert("text".into(), Value::Text("text".into()));
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(C, "C") }, Change::AddNode { canvas: C, node }]).apply(&mut project).unwrap();
    Snapshot { project, assets, epoch: 0 }
}
fn movement(x: i32) -> serde_json::Value {
    json!({"canvas":C,"id":N,"pos":{"x":x,"y":0}})
}

// Small and heavy tests call this same path. Heavy I/O is never part of ordinary CI.
use nodecraft_format::{
    asset_source::{AssetRange, AssetSource, StreamDigest, copy_and_hash},
    zip_index::{ArchiveIndex, EntryName},
};
use nodecraft_types::{AssetRef, Blake3};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};
const LARGE: u64 = u32::MAX as u64 + 2;
const CALIBRATION: u64 = 2 * 1024 * 1024 + 1;
const M: NodeId = NodeId::from_bytes([4; 16]);

struct Generated {
    left: u64,
    actual: u64,
    maximum: usize,
    hash: blake3::Hasher,
}
impl Generated {
    fn new(bytes: u64) -> Self {
        Self { left: bytes, actual: 0, maximum: 0, hash: blake3::Hasher::new() }
    }
}
impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.maximum = self.maximum.max(out.len());
        let n = out.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        out[..n].fill(b'x');
        self.hash.update(&out[..n]);
        self.left -= n as u64;
        self.actual = self.actual.checked_add(n as u64).unwrap();
        Ok(n)
    }
}
struct FileSource {
    file: Mutex<File>,
    bytes: u64,
}
impl AssetSource for FileSource {
    fn byte_len(&self) -> u64 {
        self.bytes
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        Ok(f.read(out).unwrap())
    }
}
fn file_range(path: &Path) -> AssetRange {
    let file = File::open(path).unwrap();
    let bytes = file.metadata().unwrap().len();
    AssetRange::new(Arc::new(FileSource { file: Mutex::new(file), bytes }), 0, bytes).unwrap()
}
struct ReaderSource {
    reader: Mutex<nodecraft_format::VerifiedReader>,
    bytes: u64,
}
impl AssetSource for ReaderSource {
    fn byte_len(&self) -> u64 {
        self.bytes
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        let mut reader = self.reader.lock().unwrap();
        reader.seek(SeekFrom::Start(at)).unwrap();
        Ok(reader.read(out).unwrap())
    }
}
fn reference(hash: Blake3) -> AssetRef {
    AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }
}
fn extent_inventory(root: &Path) -> (u64, u64, usize) {
    fn walk(path: &Path, logical: &mut u64, allocated: &mut u64, count: &mut usize) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let m = entry.metadata().unwrap();
            if m.is_dir() {
                walk(&entry.path(), logical, allocated, count);
            } else {
                *count += 1;
                assert!(*count <= 256, "bounded owned fixture inventory");
                *logical = logical.checked_add(m.len()).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    *allocated = allocated.checked_add(m.blocks().checked_mul(512).unwrap()).unwrap();
                }
                #[cfg(not(unix))]
                {
                    let _ = allocated;
                }
            }
        }
    }
    let (mut logical, mut allocated, mut count) = (0, 0, 0);
    walk(root, &mut logical, &mut allocated, &mut count);
    (logical, allocated, count)
}
fn phase(name: &str, root: &Path, host: &TypedOnly, metrics: &StreamMetrics, start: Instant) {
    let host = host.0.borrow();
    let context = host.native.asset_context().unwrap();
    let usage = context.storage_usage().unwrap();
    let (logical, allocated, files) = extent_inventory(root);
    let stats = metrics.stats();
    println!(
        "PHYSICAL147 {}",
        json!({"phase":name,"elapsed_ms":start.elapsed().as_millis(),"archive_bytes":host.archive_bytes,
        "checkpoint_bytes":host.checkpoint_bytes,"checkpoint_identity":host.checkpoint_identity,
        "completed_plan_output_bytes":host.completed_output_bytes,"note":"completed plan lengths, not an instrumented device-write counter",
        "owned_file_lengths":logical,"allocated_blocks_bytes_unix":allocated,"files":files,
        "quota":{"verified":usage.verified,"bases":usage.bases,"journals":usage.journals,"staging":usage.staging,"metadata":usage.metadata},
        "retained":context.retained_usage().unwrap(),"source_bytes":stats.source_bytes,"max_read":stats.maximum_read,"peak_transfer":stats.peak_transfer_bytes,
        "host_metrics":format!("{:?}",host.metrics.stats())})
    );
    assert!(logical <= host.extent_cap.checked_mul(10).unwrap().checked_add(16 * 1024 * 1024 * 1024).unwrap());
    assert_eq!(usage.staging, 0);
    // An outer recovery/copy buffer may overlap one inner source/verification buffer.
    assert!(stats.peak_transfer_bytes <= 2 * 64 * 1024);
    assert!(stats.maximum_read <= 64 * 1024);
}
fn zip_checks(path: &Path, large: Blake3, tiny: Blake3, bytes: u64, heavy: bool, metrics: &StreamMetrics) -> u32 {
    let input = file_range(path);
    let index = ArchiveIndex::read(&input, &TransferCancel::default(), metrics).unwrap();
    // Small calibration uses current graph3; the real >4GiB proof uses streaming4.
    assert_eq!(index.version(), if heavy { 4 } else { 3 });
    let large_entry = index.entries().iter().find(|e| e.name == EntryName::Asset(large)).unwrap();
    let tiny_entry = index.entries().iter().find(|e| e.name == EntryName::Asset(tiny)).unwrap();
    assert_eq!(large_entry.data.byte_len(), bytes);
    assert!(tiny_entry.local_offset > large_entry.local_offset);
    if heavy {
        assert!(tiny_entry.local_offset > u32::MAX as u64);
        let mut header = [0; 30];
        input.read_exact_at(large_entry.local_offset, &mut header, metrics).unwrap();
        assert_eq!(&header[18..26], &[255; 8]);
        let name_len = u16::from_le_bytes(header[26..28].try_into().unwrap());
        assert_eq!(u16::from_le_bytes(header[28..30].try_into().unwrap()), 20);
        let mut extra = [0; 20];
        input.read_exact_at(large_entry.local_offset + 30 + u64::from(name_len), &mut extra, metrics).unwrap();
        assert_eq!(&extra[..4], &[1, 0, 16, 0]);
        assert_eq!(u64::from_le_bytes(extra[4..12].try_into().unwrap()), bytes);
        assert_eq!(u64::from_le_bytes(extra[12..20].try_into().unwrap()), bytes);
        let mut footer = [0; 98];
        input.read_exact_at(input.byte_len() - 98, &mut footer, metrics).unwrap();
        assert_eq!(&footer[..4], b"PK\x06\x06");
        assert_eq!(&footer[56..60], b"PK\x06\x07");
        assert_eq!(&footer[76..80], b"PK\x05\x06");
        assert_eq!(&footer[92..96], &[255; 4]);
        assert_eq!(u64::from_le_bytes(footer[48..56].try_into().unwrap()), index.identity().directory_offset);
        let directory = input
            .slice(index.identity().directory_offset, input.byte_len() - 98 - index.identity().directory_offset)
            .unwrap()
            .read_small(512 * 1024, metrics)
            .unwrap();
        let wanted = format!("assets/{tiny}");
        let mut at = 0usize;
        let mut found = false;
        while at < directory.len() {
            let h = &directory[at..at + 46];
            assert_eq!(&h[..4], b"PK\x01\x02");
            let name_len = u16::from_le_bytes(h[28..30].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(h[30..32].try_into().unwrap()) as usize;
            let name = &directory[at + 46..at + 46 + name_len];
            if name == wanted.as_bytes() {
                assert_eq!(&h[42..46], &[255; 4]);
                assert_eq!(extra_len, 12);
                let extra = &directory[at + 46 + name_len..at + 46 + name_len + extra_len];
                assert_eq!(&extra[..4], &[1, 0, 8, 0]);
                assert_eq!(u64::from_le_bytes(extra[4..12].try_into().unwrap()), tiny_entry.local_offset);
                found = true;
            }
            at = at.checked_add(46 + name_len + extra_len).unwrap();
        }
        assert!(found, "tiny central directory must physically carry the ZIP64 offset");
    }
    println!("PHYSICAL147 {}", json!({"phase":"index","large_bytes":bytes,"tiny_local_offset":tiny_entry.local_offset,"zip64_executed":heavy}));
    large_entry.crc32
}
fn run_roundtrip(bytes: u64, heavy: bool) {
    assert_eq!(bytes, if heavy { LARGE } else { CALIBRATION });
    let root = if heavy {
        assert_eq!(std::env::var("NODECRAFT_PHYSICAL147").as_deref(), Ok("1"), "large writes require separate resource approval");
        let parent = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PHYSICAL147_ROOT").expect("operator-selected existing owned parent"));
        assert!(parent.is_dir());
        tempfile::Builder::new().prefix("nodecraft147-session-").tempdir_in(parent).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let start = Instant::now();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let first = root.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let original = root.path().join("original.ncraft");
    let host_a = host(&first);
    host_a.0.borrow_mut().extent_cap = bytes.checked_add(16 * 1024 * 1024).unwrap();
    let mut session = Session::with_storage(snapshot(AssetStore::default()), Box::new(host_a.clone())).unwrap();
    let mut generated = Generated::new(bytes);
    let large = session.import_asset_reader(&mut generated, &cancel, &metrics).unwrap();
    assert_eq!(generated.actual, bytes);
    assert!(generated.maximum <= 64 * 1024);
    assert_eq!(large.as_bytes(), generated.hash.finalize().as_bytes());
    // Search only tiny inputs; never regenerate or buffer the large body to force ordering.
    let (tiny_bytes, tiny) = (0..65536u32)
        .find_map(|n| {
            let text = format!("physical147 tiny {n}");
            let hash = Blake3::from_bytes(*blake3::hash(text.as_bytes()).as_bytes());
            (hash > large).then_some((text, hash))
        })
        .expect("bounded tiny hash ordering search exhausted; no large re-generation");
    assert!(tiny_bytes.len() <= 64);
    assert_eq!(session.import_asset(tiny_bytes.as_bytes()).unwrap(), tiny);
    let mut media = Node::new(M, "test.media");
    media.params.insert("assets".into(), Value::List(vec![Value::Media(reference(large)), Value::Media(reference(tiny))]));
    session.dispatch("node.add", &json!({"canvas":C,"node":media})).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    session.dispatch("edit.redo", &json!({})).unwrap();
    session.dispatch("file.save_as", &json!({"location":original})).unwrap();
    let a = host_a.0.borrow().archive_bytes;
    let j = host_a.0.borrow().checkpoint_bytes;
    assert_eq!(j, a + 80);
    assert!(a <= bytes + 16 * 1024 * 1024 && j <= bytes + 16 * 1024 * 1024);
    let crc = zip_checks(&original, large, tiny, bytes, heavy, &metrics);
    phase("first-save", root.path(), &host_a, &metrics, start);
    session.close_recovery(false).unwrap();
    drop((session, host_a, generated));
    assert_eq!(extent_inventory(&first).0, 0, "no original private source/reader survives close");
    let second = root.path().join("second");
    std::fs::create_dir(&second).unwrap();
    let host_b = host(&second);
    host_b.0.borrow_mut().extent_cap = bytes.checked_add(16 * 1024 * 1024).unwrap();
    let mut session = Session::with_storage(snapshot(AssetStore::default()), Box::new(host_b.clone())).unwrap();
    session.dispatch("file.open", &json!({"location":original,"discard_dirty":true})).unwrap();
    let expected_verified = if heavy { 0 } else { bytes.checked_add(tiny_bytes.len() as u64).unwrap() };
    assert_eq!(session.assets().context().storage_usage().unwrap().verified, expected_verified, "small v1 Open is eager; heavy v2 bodies remain deferred");
    std::fs::remove_file(&original).unwrap();
    phase("open-private-external-deleted", root.path(), &host_b, &metrics, start);
    session.dispatch("node.move", &movement(9)).unwrap();
    host_b.0.borrow_mut().native.flush().unwrap();
    let recovery = second.join("recovery");
    let mut envelope = [0; 80];
    File::open(&recovery).unwrap().read_exact(&mut envelope).unwrap();
    assert_eq!(&envelope[..8], b"NCRJ0004");
    assert_eq!(u64::from_le_bytes(envelope[8..16].try_into().unwrap()), a);
    assert!(std::fs::metadata(&recovery).unwrap().len() > j);
    {
        let recovered = host_b.0.borrow().native.recover_with(&cancel, &metrics).unwrap();
        assert_eq!(recovered.applied, 1);
        assert!(!recovered.truncated_tail);
        assert_eq!(recovered.snapshot.project, *session.document());
    }
    // Republish the privately retained large source through another public Save As.
    let repaired = root.path().join("repaired.ncraft");
    session.dispatch("file.save_as", &json!({"location":repaired})).unwrap();
    std::fs::remove_file(&repaired).unwrap();
    let lease = session.assets().open_verified(&reference(large), &cancel, &metrics).unwrap();
    let reader = lease.reader(metrics.clone());
    drop(lease);
    session.dispatch("node.remove", &json!({"canvas":C,"id":M})).unwrap();
    let smaller = root.path().join("smaller.ncraft");
    session.dispatch("file.save_as", &json!({"location":smaller})).unwrap();
    assert!(std::fs::metadata(&smaller).unwrap().len() < bytes);
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(session.document().canvases()[&C].nodes().contains_key(&M));
    {
        let recovered = host_b.0.borrow().native.recover_with(&cancel, &metrics).unwrap();
        assert_eq!(recovered.snapshot.project, *session.document());
        assert_eq!(recovered.snapshot.assets.descriptor(&large).unwrap().bytes, bytes);
    }
    phase("save-as-small-undo-recovery-reader-pin", root.path(), &host_b, &metrics, start);
    session.close_recovery(true).unwrap();
    drop((session, host_b));
    // The reader retains the verified immutable source even after all Session/host owners end.
    let range = AssetRange::new(Arc::new(ReaderSource { reader: Mutex::new(reader), bytes }), 0, bytes).unwrap();
    let consumed = copy_and_hash(&range, &mut io::sink(), &cancel, &metrics).unwrap();
    assert_eq!(consumed, StreamDigest { hash: large, crc32: crc, bytes });
    drop(range);
    println!(
        "PHYSICAL147 {}",
        json!({"phase":"complete","physical_zip64":heavy,"large":large,"full_crc32":crc,"bytes":bytes,"archive":a,"checkpoint":j,
        "source_bytes":metrics.stats().source_bytes,"max_read":metrics.stats().maximum_read,"peak_transfer":metrics.stats().peak_transfer_bytes,"elapsed_ms":start.elapsed().as_millis()})
    );
    assert!(metrics.stats().maximum_read <= 64 * 1024 && metrics.stats().peak_transfer_bytes <= 2 * 64 * 1024);
    root.close().unwrap();
}
#[test]
fn physical_zip64_session_small_calibration() {
    run_roundtrip(CALIBRATION, false);
}
#[test]
#[ignore = "manual >4GiB physical writes; requires independent resource approval and NODECRAFT_PHYSICAL147=1"]
fn physical_zip64_session_large() {
    run_roundtrip(LARGE, true);
}
