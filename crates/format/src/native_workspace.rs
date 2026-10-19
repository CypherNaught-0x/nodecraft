//! Versioned private-workspace maintenance. Cooperative namespace only: locks, not PIDs,
//! establish lifetime. Legacy roots and unknown contents are never deletion authority.
use crate::asset_source::TransferCancel;
use nodecraft_types::{Error, ErrorKind};
use std::{
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const PREFIX: &str = "nodecraft-assets-v1-";
const GATE: &str = ".nodecraft-assets-v1.lock";
const MARKER: &str = "workspace.v1.lock";
const MAGIC: &[u8] = b"NCASSET1\n";
const MAX_ROOT_NAME: usize = PREFIX.len() + 10 + 1 + 20;
const MAX_PATH_BYTES: usize = crate::native_cleanup::MAX_PATH_BYTES;
const MAX_PAYLOADS: usize = crate::native_cleanup::MAX_SLOTS;
const MAX_PARENT: usize = 4096;
const MAX_ATTEMPTS: u32 = 64;
const MAX_UNLINKS: u32 = 256;
const MAX_INVENTORY_BYTES: usize = 512 * 1024;

/// A bounded summary, without retained paths, filenames, payload data or OS error strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub visited: u32,
    pub live: u32,
    pub removed_roots: u32,
    pub removed_files: u32,
    pub unknown_or_legacy: u32,
    pub retained_failures: u32,
    pub inventory_attempts: u32,
    pub scan_window_exhausted: bool,
    pub cleanup_budget_exhausted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepStatus {
    /// The bounded pass/target finished; live and unknown/legacy counters may still be nonzero.
    Complete,
    ScanWindowExhausted,
    CleanupBudgetExhausted,
    RetainedFailure,
}
impl SweepReport {
    pub fn status(&self) -> SweepStatus {
        if self.retained_failures > 0 {
            SweepStatus::RetainedFailure
        } else if self.scan_window_exhausted {
            SweepStatus::ScanWindowExhausted
        } else if self.cleanup_budget_exhausted {
            SweepStatus::CleanupBudgetExhausted
        } else {
            SweepStatus::Complete
        }
    }
}
fn error(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}
fn reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}
fn ordinary(metadata: &Metadata, directory: bool) -> bool {
    !metadata.file_type().is_symlink() && !reparse(metadata) && if directory { metadata.is_dir() } else { metadata.is_file() }
}
fn private(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}
fn path_buffer(base: &Path, suffix: usize) -> Result<PathBuf, Error> {
    let length =
        base.as_os_str().len().checked_add(suffix).filter(|n| *n <= MAX_PATH_BYTES).ok_or_else(|| error("private workspace path exceeds ownership limit"))?;
    let mut path = PathBuf::new();
    path.try_reserve_exact(length).map_err(|_| error("cannot allocate bounded workspace path"))?;
    if path.capacity() > MAX_PATH_BYTES {
        return Err(error("workspace path backing exceeds ownership limit"));
    }
    path.push(base);
    Ok(path)
}
fn open_file(path: &Path, create: bool, exclusive: bool) -> Result<File, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if !ordinary(&m, false) || !private(&m) => return Err(error("private workspace lock type or permissions are unsafe")),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(error("cannot inspect private workspace lock")),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).create_new(exclusive).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| error("cannot open private workspace lock"))
}
fn open_regular(path: &Path, create: bool, exclusive: bool) -> Result<same_file::Handle, Error> {
    let file = open_file(path, create, exclusive)?;
    let handle = same_file::Handle::from_file(file).map_err(|_| error("cannot inspect opened workspace lock"))?;
    if !matches_path(&handle, path) {
        return Err(error("private workspace lock identity changed"));
    }
    Ok(handle)
}
fn matches_path(handle: &same_file::Handle, path: &Path) -> bool {
    // Same-file is only a mismatch diagnostic. Cleanup authority still requires the
    // versioned marker AND held lock under admission; hostile namespace replacement is excluded.
    let safe = std::fs::symlink_metadata(path).is_ok_and(|m| ordinary(&m, false) && private(&m))
        && handle.as_file().metadata().is_ok_and(|m| ordinary(&m, false) && private(&m));
    safe && same_file::Handle::from_path(path).is_ok_and(|current| current == *handle)
}
struct Admission {
    parent: PathBuf,
    _file: same_file::Handle,
}
impl Admission {
    fn acquire(parent: &Path) -> Result<Self, Error> {
        let _ = path_buffer(parent, MAX_ROOT_NAME + 28 + 2)?;
        let parent = parent.canonicalize().map_err(|_| error("cannot resolve workspace parent"))?;
        if parent.capacity() > MAX_PATH_BYTES {
            return Err(error("canonical workspace parent backing exceeds limit"));
        }
        let mut path = path_buffer(&parent, MAX_ROOT_NAME + 28 + 2)?;
        if !std::fs::symlink_metadata(&parent).is_ok_and(|m| ordinary(&m, true)) {
            return Err(error("workspace parent is not an ordinary directory"));
        }
        path.push(GATE);
        let file = open_regular(&path, true, false)?;
        #[cfg(test)]
        pause("after-gate-open", &parent, None);
        file.as_file().try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => Error::new(ErrorKind::Busy, "workspace parent admission is busy; retry"),
            _ => error("workspace admission locking is unavailable"),
        })?;
        if !matches_path(&file, &path) {
            return Err(error("workspace admission identity changed"));
        }
        Ok(Self { parent, _file: file })
    }
}
fn decimal(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) || text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}
fn root_name(name: &str) -> bool {
    if name.len() > MAX_ROOT_NAME {
        return false;
    }
    let Some((pid, id)) = name.strip_prefix(PREFIX).and_then(|s| s.split_once('-')) else { return false };
    decimal(pid).is_some_and(|n| u32::try_from(n).is_ok()) && decimal(id).is_some()
}
#[derive(Clone, Copy)]
enum Kind {
    Stage,
    Archive,
    Capture,
}
#[derive(Clone, Copy)]
struct Record {
    kind: Kind,
    id: u64,
}
impl Record {
    fn parse(name: &str) -> Option<Self> {
        let (kind, number) = name.split_once('-')?;
        let kind = match kind {
            "stage" => Kind::Stage,
            "archive" => Kind::Archive,
            "capture" => Kind::Capture,
            _ => return None,
        };
        Some(Self { kind, id: decimal(number)? })
    }
    fn name(self) -> Result<Name, Error> {
        let mut name = Name { bytes: [0; 28], len: 0 };
        use std::fmt::Write;
        write!(
            &mut name,
            "{}-{}",
            match self.kind {
                Kind::Stage => "stage",
                Kind::Archive => "archive",
                Kind::Capture => "capture",
            },
            self.id
        )
        .map_err(|_| error("workspace payload name exceeds its grammar"))?;
        Ok(name)
    }
}
struct Name {
    bytes: [u8; 28],
    len: usize,
}
impl std::fmt::Write for Name {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(std::fmt::Error)?;
        self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}
impl Name {
    fn text(&self) -> Result<&str, Error> {
        std::str::from_utf8(self.bytes.get(..self.len).ok_or_else(|| error("invalid payload name extent"))?).map_err(|_| error("invalid payload name encoding"))
    }
}
fn marker_bytes(name: &str) -> Result<([u8; 128], usize), Error> {
    let mut bytes = [0; 128];
    let length = MAGIC
        .len()
        .checked_add(name.len())
        .and_then(|n| n.checked_add(1))
        .filter(|n| *n <= bytes.len())
        .ok_or_else(|| error("workspace marker exceeds limit"))?;
    bytes.get_mut(..MAGIC.len()).ok_or_else(|| error("invalid workspace marker extent"))?.copy_from_slice(MAGIC);
    bytes.get_mut(MAGIC.len()..length - 1).ok_or_else(|| error("invalid workspace marker name"))?.copy_from_slice(name.as_bytes());
    *bytes.get_mut(length - 1).ok_or_else(|| error("invalid workspace marker terminator"))? = b'\n';
    Ok((bytes, length))
}
fn marker_valid(handle: &same_file::Handle, name: &str) -> Result<bool, Error> {
    let (expected, length) = marker_bytes(name)?;
    let metadata = handle.as_file().metadata().map_err(|_| error("cannot inspect workspace marker"))?;
    if metadata.len() != length as u64 {
        return Ok(false);
    }
    let mut file = handle.as_file();
    let mut actual = [0; 128];
    file.seek(SeekFrom::Start(0)).map_err(|_| error("cannot seek workspace marker"))?;
    file.read_exact(actual.get_mut(..length).ok_or_else(|| error("invalid workspace marker buffer"))?).map_err(|_| error("cannot read workspace marker"))?;
    Ok(actual.get(..length) == expected.get(..length))
}

pub(crate) struct LifetimeLock {
    marker: Option<same_file::Handle>,
}
pub(crate) struct Created {
    pub root: PathBuf,
    pub lock: LifetimeLock,
    pub report: SweepReport,
}
pub(crate) fn create(parent: &Path, mut next: impl FnMut() -> Result<u64, Error>) -> Result<Created, Error> {
    let admission = Admission::acquire(parent)?;
    let report = scan(&admission, &TransferCancel::default())?;
    for _ in 0..8 {
        let name = format!("{PREFIX}{}-{}", std::process::id(), next()?);
        let mut root = path_buffer(&admission.parent, MAX_ROOT_NAME + 29)?;
        root.push(&name);
        #[cfg(unix)]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&root) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(error("cannot create private asset workspace; check permissions and free space")),
            Ok(()) => {}
        }
        #[cfg(test)]
        pause("after-root", &admission.parent, Some(&root));
        root.push(MARKER);
        let marker = open_file(&root, false, true);
        let created_marker = marker.is_ok();
        let result = marker.and_then(|file| {
            let handle = same_file::Handle::from_file(file).map_err(|_| error("cannot inspect new workspace marker"))?;
            handle.as_file().try_lock().map_err(|_| error("cannot lock new workspace lifetime"))?;
            let (bytes, length) = marker_bytes(&name)?;
            let mut file = handle.as_file();
            file.write_all(bytes.get(..length).ok_or_else(|| error("invalid marker bytes"))?).map_err(|_| error("cannot write workspace marker"))?;
            file.sync_all().map_err(|_| error("cannot sync workspace marker"))?;
            if !matches_path(&handle, &root) || !marker_valid(&handle, &name)? {
                return Err(error("new workspace marker verification failed"));
            }
            Ok(handle)
        });
        match result {
            Ok(marker) => {
                root.pop();
                #[cfg(test)]
                pause("after-marker", &admission.parent, Some(&root));
                return Ok(Created { root, lock: LifetimeLock { marker: Some(marker) }, report });
            }
            Err(e) => {
                // Only this exclusively created directory/marker; never a collided root.
                if created_marker {
                    let _ = std::fs::remove_file(&root);
                }
                root.pop();
                let _ = std::fs::remove_dir(&root);
                return Err(e);
            }
        }
    }
    Err(error("cannot create an exclusive private asset workspace"))
}
impl LifetimeLock {
    pub fn retire_empty(&mut self, root: &Path) {
        let Some(parent) = root.parent() else { return };
        let Ok(_admission) = Admission::acquire(parent) else { return };
        let Some(name) = root.file_name().and_then(|n| n.to_str()).filter(|n| root_name(n)) else { return };
        let Ok(mut entries) = std::fs::read_dir(root) else { return };
        let only_marker = matches!(entries.next(),Some(Ok(e)) if e.file_name()==MARKER) && entries.next().is_none();
        drop(entries);
        let Ok(mut path) = path_buffer(root, MARKER.len() + 1) else { return };
        path.push(MARKER);
        if !only_marker || !self.marker.as_ref().is_some_and(|m| matches_path(m, &path) && marker_valid(m, name).is_ok_and(|valid| valid)) {
            return;
        }
        drop(self.marker.take());
        if std::fs::remove_file(path).is_ok() {
            let _ = std::fs::remove_dir(root);
        }
    }
}

/// Bounded startup maintenance of supported future-protocol roots only. A fixed scan window
/// is not an eventual-cleanup guarantee; use named maintenance for a known root outside it.
pub fn sweep_orphans(parent: &Path, cancel: &TransferCancel) -> Result<SweepReport, Error> {
    let admission = Admission::acquire(parent)?;
    scan(&admission, cancel)
}
/// Named locator, not deletion authority. Legacy, path-like and unproved names refuse.
pub fn sweep_named_orphan(parent: &Path, basename: &str, cancel: &TransferCancel) -> Result<SweepReport, Error> {
    if !root_name(basename) {
        return Err(error("maintenance requires an exact supported workspace basename"));
    }
    let admission = Admission::acquire(parent)?;
    let mut report = SweepReport { visited: 1, ..Default::default() };
    candidate(&admission, basename, cancel, &mut report)?;
    Ok(report)
}
fn scan(admission: &Admission, cancel: &TransferCancel) -> Result<SweepReport, Error> {
    let entries = std::fs::read_dir(&admission.parent).map_err(|_| error("cannot scan workspace parent"))?;
    scan_entries(admission, cancel, entries.map(|entry| entry.map(|entry| entry.file_name())))
}
fn scan_entries(
    admission: &Admission,
    cancel: &TransferCancel,
    mut entries: impl Iterator<Item = std::io::Result<std::ffi::OsString>>,
) -> Result<SweepReport, Error> {
    let mut report = SweepReport::default();
    for _ in 0..MAX_PARENT {
        cancel.check()?;
        let Some(entry) = entries.next() else { return Ok(report) };
        report.visited = report.visited.saturating_add(1);
        let name = entry.map_err(|_| error("cannot read workspace parent entry"))?;
        let Some(name) = name.to_str().filter(|n| root_name(n)) else {
            report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
            continue;
        };
        candidate(admission, name, cancel, &mut report)?;
        if report.cleanup_budget_exhausted {
            return Ok(report);
        }
    }
    // Deliberately no4097th read: exactly filling the window cannot prove exhaustion.
    report.scan_window_exhausted = true;
    Ok(report)
}
fn candidate(admission: &Admission, name: &str, cancel: &TransferCancel, report: &mut SweepReport) -> Result<(), Error> {
    cancel.check()?;
    // One reusable candidate-path buffer, not one full path per inventory record.
    let mut path = path_buffer(&admission.parent, MAX_ROOT_NAME + 29)?;
    path.push(name);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if ordinary(&m, true) && private(&m) => {}
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            report.retained_failures = report.retained_failures.saturating_add(1);
            return Ok(());
        }
        _ => {
            report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
            return Ok(());
        }
    }
    path.push(MARKER);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if ordinary(&m, false) && private(&m) => {}
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            report.retained_failures = report.retained_failures.saturating_add(1);
            return Ok(());
        }
        _ => {
            report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
            return Ok(());
        }
    }
    let marker = match open_regular(&path, false, false) {
        Ok(marker) => marker,
        Err(_) => {
            report.retained_failures = report.retained_failures.saturating_add(1);
            return Ok(());
        }
    };
    // Windows locks are mandatory for reads through another handle. Before our own lock,
    // inspect only metadata; exact bytes establish authority only AFTER exclusive admission.
    let (_, expected_length) = marker_bytes(name)?;
    if !marker.as_file().metadata().is_ok_and(|m| m.len() == expected_length as u64) {
        report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
        return Ok(());
    }
    match marker.as_file().try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            report.live = report.live.saturating_add(1);
            return Ok(());
        }
        Err(_) => {
            report.retained_failures = report.retained_failures.saturating_add(1);
            return Ok(());
        }
    }
    if !matches_path(&marker, &path) || !marker_valid(&marker, name)? {
        report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
        return Ok(());
    }
    if report.inventory_attempts >= MAX_ATTEMPTS {
        report.cleanup_budget_exhausted = true;
        return Ok(());
    }
    report.inventory_attempts += 1; // BEFORE every attempted inventory, including later refusal.
    path.pop();
    let Some(records) = inventory(&path, cancel)? else {
        report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
        return Ok(());
    };
    path.push(MARKER);
    if !matches_path(&marker, &path) || !marker_valid(&marker, name)? {
        report.unknown_or_legacy = report.unknown_or_legacy.saturating_add(1);
        return Ok(());
    }
    path.pop();
    for record in records {
        cancel.check()?;
        if report.removed_files >= MAX_UNLINKS {
            report.cleanup_budget_exhausted = true;
            return Ok(());
        }
        path.push(record.name()?.text()?);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| ordinary(&m, false)) {
            return Err(error("workspace payload changed; maintenance retained its marker"));
        }
        std::fs::remove_file(&path).map_err(|_| error("workspace cleanup failed; check permissions and retry maintenance"))?;
        report.removed_files += 1;
        path.pop();
        #[cfg(test)]
        if report.removed_files == 1 {
            pause("after-unlink", &admission.parent, Some(&path));
        }
    }
    path.push(MARKER);
    if !matches_path(&marker, &path) || !marker_valid(&marker, name)? {
        return Err(error("workspace marker changed during maintenance"));
    }
    drop(marker); // Parent admission remains held across the Windows close/unlink gap.
    path.pop();
    #[cfg(test)]
    pause("after-marker-close", &admission.parent, Some(&path));
    path.push(MARKER);
    std::fs::remove_file(&path).map_err(|_| error("cannot retire workspace marker; retry maintenance"))?;
    path.pop();
    #[cfg(test)]
    pause("after-marker-unlink", &admission.parent, Some(&path));
    std::fs::remove_dir(&path).map_err(|_| error("workspace directory remains after marker retirement; inspect it manually"))?;
    report.removed_roots += 1;
    Ok(())
}
fn inventory(root: &Path, cancel: &TransferCancel) -> Result<Option<Vec<Record>>, Error> {
    inventory_with_capacity(root, cancel, MAX_PAYLOADS)
}
fn inventory_with_capacity(root: &Path, cancel: &TransferCancel, requested: usize) -> Result<Option<Vec<Record>>, Error> {
    // Production requests exactly MAX_PAYLOADS. A private capacity control can request the
    // first rejected backing size, never an unbounded allocation.
    if requested > MAX_INVENTORY_BYTES / std::mem::size_of::<Record>() + 1 {
        return Err(error("invalid workspace inventory reservation"));
    }
    let mut records = Vec::new();
    records.try_reserve_exact(requested).map_err(|_| error("cannot allocate bounded workspace inventory"))?;
    if records.capacity().checked_mul(std::mem::size_of::<Record>()).is_none_or(|n| n > MAX_INVENTORY_BYTES) {
        return Err(error("workspace inventory backing exceeds limit"));
    }
    let mut marker_seen = false;
    let mut entries = std::fs::read_dir(root).map_err(|_| error("cannot preflight workspace inventory"))?;
    for _ in 0..MAX_PAYLOADS + 2 {
        cancel.check()?;
        let Some(entry) = entries.next() else { return Ok(marker_seen.then_some(records)) };
        let entry = entry.map_err(|_| error("cannot inspect workspace inventory entry"))?;
        if !entry.file_type().is_ok_and(|kind| kind.is_file() && !kind.is_symlink()) || !entry.metadata().is_ok_and(|m| ordinary(&m, false)) {
            return Ok(None);
        }
        let name = entry.file_name();
        if name == MARKER {
            if marker_seen {
                return Ok(None);
            }
            marker_seen = true;
            continue;
        }
        let Some(record) = name.to_str().and_then(Record::parse) else { return Ok(None) };
        if records.len() >= MAX_PAYLOADS {
            return Ok(None);
        }
        records.push(record);
    }
    Ok(None)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn pause(point: &str, parent: &Path, root: Option<&Path>) {
    if std::env::var("NODECRAFT_TEST_WORKSPACE363_PAUSE").as_deref() != Ok(point) {
        return;
    }
    let expected = std::env::var_os("NODECRAFT_TEST_WORKSPACE363_ROOT").map(PathBuf::from).unwrap();
    assert_eq!(parent, expected.canonicalize().unwrap());
    assert_eq!(std::fs::read(parent.join("fixture-owner")).unwrap(), b"public isolated workspace363 fixture");
    let name = root.and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("");
    std::fs::write(parent.join("pause-ready.tmp"), name).unwrap();
    std::fs::rename(parent.join("pause-ready.tmp"), parent.join("pause-ready")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if parent.join("pause-exit").exists() {
            std::process::exit(0);
        }
        if parent.join("pause-continue").exists() {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "bounded workspace pause expired");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Test fixtures expecting eventual success may encounter transient cooperative admission.
/// Keep all deliberate while-held Busy/refusal assertions outside this helper.
#[cfg(test)]
pub(crate) fn test_uncontended<T>(mut attempt: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut tries = 0;
    loop {
        let result = attempt();
        if matches!(&result, Err(error) if error.kind == ErrorKind::Busy) && tries < 2000 && std::time::Instant::now() < deadline {
            tries += 1;
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        return result;
    }
}
