//! Platform storage boundary. Errors are public-safe and never contain a path or document data.
use crate::asset_source::{StreamMetrics, TransferCancel};
use crate::{ArchivePlan, AssetContext, CheckpointPlan, MAX_ARCHIVE_BYTES, Snapshot, StorageCapability, invalid, journal::MAX_JOURNAL_BYTES};
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageLocation {
    /// Native APIs retain non-UTF8 paths. JSON command paths are explicitly UTF8.
    Path(PathBuf),
    /// Browser persistence adapters may interpret this as an opaque handle/key.
    Key(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProjectAccess {
    #[default]
    ReadWrite,
    ReadOnly,
}

#[derive(Debug)]
pub struct LoadedProject {
    pub bytes: Vec<u8>,
    pub access: ProjectAccess,
}

/// An admitted graph plus the exact immutable archive chosen for its first checkpoint.
/// Only storage protocol constructors can pair them; commit_open remains infallible.
pub struct PreparedSnapshot {
    snapshot: Snapshot,
    checkpoint: CheckpointPlan,
    access: ProjectAccess,
}
impl PreparedSnapshot {
    pub fn into_parts(self) -> (Snapshot, CheckpointPlan, ProjectAccess) {
        (self.snapshot, self.checkpoint, self.access)
    }
}

impl StorageLocation {
    pub fn from_command(value: String) -> Result<Self, Error> {
        if value.is_empty() || value.len() > 4096 || value.contains('\0') {
            return Err(invalid("location must contain 1 to 4096 bytes without NUL"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(Self::Path(value.into()))
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self::Key(value))
        }
    }
}

/// A storage instance belongs to one session. `save`/`checkpoint` errors MUST mean that the
/// replacement was not published. Success means published (not a guarantee against hardware
/// failure). An append error may have written a partial or complete frame; stop appending and
/// checkpoint before proceeding. Implementations must enforce the same byte limits as codecs.
pub trait ProjectStorage {
    /// Stable, private identity scope for a source location. Native hosts canonicalize
    /// aliases; opaque hosts use their exact key. No location text enters the document.
    fn project_identity_scope(&self, location: &StorageLocation) -> Result<Blake3, Error> {
        location_identity(location)
    }
    /// Explicit capability: absence preserves the frozen bounded byte implementation.
    fn capability(&self) -> StorageCapability {
        StorageCapability::SmallV1
    }
    fn asset_context(&self) -> Option<AssetContext> {
        None
    }
    fn prepare_open_snapshot(&mut self, location: &StorageLocation, cancel: &TransferCancel, _metrics: &StreamMetrics) -> Result<PreparedSnapshot, Error> {
        cancel.check()?;
        let loaded = self.prepare_open(location)?;
        let (snapshot, archive) = ArchivePlan::from_small_open(loaded.bytes)?;
        let checkpoint = CheckpointPlan::new(archive)?;
        Ok(PreparedSnapshot { snapshot, checkpoint, access: loaded.access })
    }
    /// Defaults reject a large/lazy plan BEFORE any source read or legacy write call.
    fn save_plan(&mut self, location: &StorageLocation, plan: &ArchivePlan, cancel: &TransferCancel, _metrics: &StreamMetrics) -> Result<ProjectAccess, Error> {
        cancel.check()?;
        self.save_with_access(location, plan.small_bytes()?)
    }
    fn checkpoint_plan(&mut self, plan: &CheckpointPlan, cancel: &TransferCancel, _metrics: &StreamMetrics) -> Result<(), Error> {
        cancel.check()?;
        self.checkpoint(plan.small_bytes()?)
    }
    fn append_plan(&mut self, plan: &crate::FramePlan, cancel: &TransferCancel, _metrics: &StreamMetrics) -> Result<(), Error> {
        cancel.check()?;
        self.append(plan.small_bytes()?)
    }
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error>;
    /// Stage a location without releasing the active destination. Locking/read-only hosts
    /// must implement this protocol; defaults suit writable hosts without location locks.
    fn prepare_open(&mut self, location: &StorageLocation) -> Result<LoadedProject, Error> {
        self.load(location).map(|bytes| LoadedProject { bytes, access: ProjectAccess::ReadWrite })
    }
    /// Infallible publication hook: no fallible I/O, allocation or new validation may occur here.
    fn commit_open(&mut self) {}
    /// Drop only the candidate; preserve the active destination after a failed transition.
    fn cancel_open(&mut self) {}
    fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error>;
    /// Success always means published. A locking host may lose writable capability
    /// after publication; report it without turning the completed save into an error.
    /// Locking/read-only wrappers must forward this method as well as staged open.
    fn save_with_access(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<ProjectAccess, Error> {
        self.save(location, archive).map(|()| ProjectAccess::ReadWrite)
    }
    fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error>;
    fn append(&mut self, frame: &[u8]) -> Result<(), Error>;
    /// Flush the recovery durability window. Native storage also group-syncs in the background.
    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
    /// Explicit clean-close/discard boundary; never silently delete unknown host recovery data.
    fn discard(&mut self) -> Result<(), Error> {
        Err(Error::new(ErrorKind::Unsupported, "recovery discard is not supported by this host"))
    }
}

fn location_identity(location: &StorageLocation) -> Result<Blake3, Error> {
    let (tag, bytes): (&[u8], &[u8]) = match location {
        StorageLocation::Path(path) => (b"path", path.as_os_str().as_encoded_bytes()),
        StorageLocation::Key(key) => (b"key", key.as_bytes()),
    };
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
        return Err(invalid("project identity location must contain 1 to 4096 bytes without NUL"));
    }
    let mut hash = blake3::Hasher::new_derive_key("NodeCraft legacy project location v1");
    hash.update(tag);
    hash.update(bytes);
    Ok(Blake3::from_bytes(*hash.finalize().as_bytes()))
}

/// Deterministic storage for tests and hosts that manage persistence outside Rust filesystem APIs.
#[derive(Default)]
pub struct MemoryStorage {
    pub files: BTreeMap<StorageLocation, Vec<u8>>,
    pub recovery: Vec<u8>,
}
impl ProjectStorage for MemoryStorage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        let bytes = self.files.get(location).ok_or_else(|| Error::new(ErrorKind::NotFound, "project does not exist"))?;
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(invalid("project exceeds archive size limit"));
        }
        Ok(bytes.clone())
    }
    fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error> {
        if archive.len() > MAX_ARCHIVE_BYTES {
            return Err(invalid("project exceeds archive size limit"));
        }
        self.files.insert(location.clone(), archive.to_vec());
        Ok(())
    }
    fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
        if recovery.len() > MAX_JOURNAL_BYTES {
            return Err(invalid("recovery exceeds size limit"));
        }
        self.recovery = recovery.to_vec();
        Ok(())
    }
    fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
        if self.recovery.len().saturating_add(frame.len()) > MAX_JOURNAL_BYTES {
            return Err(invalid("recovery file is full; save a checkpoint"));
        }
        self.recovery.extend_from_slice(frame);
        Ok(())
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.recovery.clear();
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native {
    use super::*;
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        path::Path,
        sync::{Arc, Mutex},
        thread::{self, JoinHandle},
        time::Duration,
    };
    mod destinations;
    pub(crate) mod durability;
    #[cfg(test)]
    mod physical147;
    mod recovery;

    pub const GROUP_SYNC_INTERVAL: Duration = Duration::from_millis(250);
    /// Private recovery paths and per-user canonical destination locks prevent concurrent writers.
    /// Journal bytes become kernel-visible before publication; the worker syncs pending bytes
    /// every 250 ms plus scheduling/filesystem latency. Explicit flush/close closes that window.
    pub struct NativeStorage {
        recovery: PathBuf,
        lock: PathBuf,
        destinations: destinations::Destinations,
        journal: Arc<durability::SharedJournal>,
        worker: Option<JoinHandle<()>>,
        context: AssetContext,
    }
    impl NativeStorage {
        /// Bounded startup maintenance only; a scan-window limit does not mean the parent is clean.
        pub fn workspace_maintenance_report(&self) -> Option<crate::native_workspace::SweepReport> {
            self.context.0.workspace.as_ref().map(crate::NativeAssetWorkspace::maintenance_report)
        }
        pub fn new(recovery: PathBuf) -> Result<Self, Error> {
            Self::with_interval(recovery, GROUP_SYNC_INTERVAL)
        }
        fn with_interval(recovery: PathBuf, interval: Duration) -> Result<Self, Error> {
            Self::with_interval_quota(recovery, interval, crate::disk_quota::DiskQuota::default())
        }
        fn with_interval_quota(recovery: PathBuf, interval: Duration, quota: crate::disk_quota::DiskQuota) -> Result<Self, Error> {
            let mut name = recovery.as_os_str().to_os_string();
            name.push(".lock");
            let lock = PathBuf::from(name);
            let file = lock_file(&lock, "recovery location is already in use")?;
            let parent = recovery.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
            let workspace = crate::NativeAssetWorkspace::new(parent, quota)?;
            let context = AssetContext::native(workspace.clone());
            let journal = Arc::new(durability::SharedJournal::new(recovery.clone(), lock.clone(), file, workspace.quota().clone())?);
            workspace.install_journal(&journal)?;
            let worker = Self::start_worker(&journal, interval)?;
            worker.thread().unpark();
            Ok(Self { recovery, lock, destinations: destinations::Destinations::new(), journal, worker: Some(worker), context })
        }
        fn start_worker(journal: &Arc<durability::SharedJournal>, interval: Duration) -> Result<JoinHandle<()>, Error> {
            let state = journal.clone();
            thread::Builder::new()
                .name("nodecraft-recovery".into())
                .spawn(move || {
                    // Reopen commits its existing owner before waking this worker.
                    thread::park();
                    loop {
                        if state.lock().map_or(true, |state| state.stop) {
                            return;
                        }
                        thread::park_timeout(interval);
                        let _ = state.flush();
                        if state.lock().map_or(true, |state| state.stop) {
                            return;
                        }
                    }
                })
                .map_err(|_| io_error("cannot start recovery durability worker"))
        }
        /// Reuse the actual retained owner and charge without stealing a live context.
        pub fn reopen(context: AssetContext) -> Result<Self, Error> {
            Self::reopen_with_worker(context, |journal| Self::start_worker(journal, GROUP_SYNC_INTERVAL))
        }
        fn reopen_with_worker(
            context: AssetContext,
            start: impl FnOnce(&Arc<durability::SharedJournal>) -> Result<JoinHandle<()>, Error>,
        ) -> Result<Self, Error> {
            let workspace = context.0.workspace.as_ref().ok_or_else(|| io_error("reopen requires a native recovery context"))?;
            let journal = workspace.journal()?;
            let (recovery, lock) = {
                let (recovery, lock) = journal.paths();
                (recovery.to_path_buf(), lock.to_path_buf())
            };
            let claim = journal.begin_reopen()?;
            let worker = start(&journal)?;
            claim.commit();
            worker.thread().unpark();
            Ok(Self { recovery, lock, destinations: destinations::Destinations::new(), journal, worker: Some(worker), context })
        }
        pub fn recover(&self) -> Result<crate::journal::Recovered, Error> {
            self.recover_with(&TransferCancel::default(), &StreamMetrics::default())
        }
        /// Stream native recovery with an explicit cancellation and measurement scope.
        pub fn recover_with(&self, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<crate::journal::Recovered, Error> {
            let workspace = self.context.0.workspace.as_ref().ok_or_else(|| io_error("native asset workspace is unavailable"))?;
            self.journal.recover(self.context.clone(), workspace, cancel, metrics)
        }
    }
    impl ProjectStorage for NativeStorage {
        fn project_identity_scope(&self, location: &StorageLocation) -> Result<Blake3, Error> {
            let StorageLocation::Path(path) = location else {
                return Err(Error::new(ErrorKind::Unsupported, "native storage requires a filesystem path"));
            };
            location_identity(&StorageLocation::Path(resolved(path)?))
        }
        fn capability(&self) -> StorageCapability {
            StorageCapability::StreamingV2
        }
        fn asset_context(&self) -> Option<AssetContext> {
            Some(self.context.clone())
        }
        fn prepare_open_snapshot(&mut self, location: &StorageLocation, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<PreparedSnapshot, Error> {
            let (path, access) = self.destinations.prepare(location, &self.recovery, &self.lock)?;
            let prepared = (|| {
                let file = File::open(path).map_err(|_| io_error("cannot open project; check location and permissions"))?;
                let workspace = self.context.0.workspace.as_ref().ok_or_else(|| io_error("native asset workspace is unavailable"))?;
                let (sealed, index) = workspace.freeze_archive(file, cancel, metrics)?;
                let snapshot = crate::decode_indexed(&index, self.context.clone())?;
                let archive = if index.version() < crate::archive::VERSION {
                    ArchivePlan::prepare(&snapshot, self.capability(), cancel, metrics)?
                } else {
                    ArchivePlan::from_native_open(sealed, &snapshot)?
                };
                let checkpoint = CheckpointPlan::new(archive)?;
                Ok(PreparedSnapshot { snapshot, checkpoint, access })
            })();
            if prepared.is_err() {
                self.destinations.cancel();
            }
            prepared
        }
        fn save_plan(
            &mut self,
            location: &StorageLocation,
            plan: &ArchivePlan,
            cancel: &TransferCancel,
            metrics: &StreamMetrics,
        ) -> Result<ProjectAccess, Error> {
            plan.check_context(&self.context)?;
            cancel.check()?;
            let workspace = self.context.0.workspace.as_ref().ok_or_else(|| io_error("native asset workspace is unavailable"))?;
            let operation = workspace.quota().stage_operation()?;
            let ready = crate::native_atomic::AtomicFile::prepare(workspace.quota())?;
            self.destinations.cancel();
            let candidate = self.destinations.save_candidate(location, &self.recovery, &self.lock)?;
            workspace
                .publish_file(&operation, &candidate.path, plan.byte_len(), false, &ready, |writer| plan.write_to(writer, cancel, metrics))
                .map_err(|failure| failure.error)?;
            let candidate = self.destinations.published(candidate);
            let access = candidate.access;
            self.destinations.adopt(candidate);
            Ok(access)
        }
        fn checkpoint_plan(&mut self, plan: &CheckpointPlan, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
            plan.archive().check_context(&self.context)?;
            cancel.check()?;
            let workspace = self.context.0.workspace.as_ref().ok_or_else(|| io_error("native asset workspace is unavailable"))?;
            self.journal.checkpoint_plan(workspace, plan, cancel, metrics)
        }
        fn append_plan(&mut self, plan: &crate::FramePlan, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
            plan.check_context(&self.context)?;
            cancel.check()?;
            self.journal.append_plan(plan, cancel, metrics)
        }
        fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
            let loaded = self.prepare_open(location)?;
            self.commit_open();
            Ok(loaded.bytes)
        }
        fn prepare_open(&mut self, location: &StorageLocation) -> Result<LoadedProject, Error> {
            let (path, access) = self.destinations.prepare(location, &self.recovery, &self.lock)?;
            match read(&path, MAX_ARCHIVE_BYTES) {
                Ok(bytes) => Ok(LoadedProject { bytes, access }),
                Err(error) => {
                    self.destinations.cancel();
                    Err(error)
                }
            }
        }
        fn commit_open(&mut self) {
            self.destinations.commit();
        }
        fn cancel_open(&mut self) {
            self.destinations.cancel();
        }
        fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error> {
            self.save_with_access(location, archive).map(|_| ())
        }
        fn save_with_access(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<ProjectAccess, Error> {
            if archive.len() > MAX_ARCHIVE_BYTES {
                return Err(invalid("project exceeds archive size limit"));
            }
            self.destinations.cancel();
            let candidate = self.destinations.save_candidate(location, &self.recovery, &self.lock)?;
            let workspace = self.context.0.workspace.as_ref().ok_or_else(|| io_error("native asset workspace is unavailable"))?;
            let operation = workspace.quota().stage_operation()?;
            let ready = crate::native_atomic::AtomicFile::prepare(workspace.quota())?;
            workspace
                .publish_file(&operation, &candidate.path, archive.len() as u64, false, &ready, |file| {
                    file.write_all(archive).map_err(|_| io_error("cannot save project; check permissions and free space"))
                })
                .map_err(|failure| failure.error)?;
            let candidate = self.destinations.published(candidate);
            let access = candidate.access;
            self.destinations.adopt(candidate);
            Ok(access)
        }
        fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
            if recovery.len() > MAX_JOURNAL_BYTES {
                return Err(invalid("recovery exceeds size limit"));
            }
            let workspace = self.context.0.workspace.as_ref().ok_or_else(|| io_error("native asset workspace is unavailable"))?;
            self.journal.checkpoint(workspace, recovery)
        }
        fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
            self.journal.append(&self.recovery, frame)
        }
        fn flush(&mut self) -> Result<(), Error> {
            self.journal.flush()
        }
        fn discard(&mut self) -> Result<(), Error> {
            self.journal.discard(&self.recovery)?;
            self.destinations.close();
            Ok(())
        }
    }
    impl Drop for NativeStorage {
        fn drop(&mut self) {
            if let Ok(mut state) = self.journal.lock() {
                state.stop = true;
            }
            if let Some(worker) = self.worker.take() {
                worker.thread().unpark();
                if worker.join().is_err() {
                    let _ = self.journal.fail();
                }
            }
            if self.journal.close()
                && let Some(workspace) = &self.context.0.workspace
            {
                workspace.remove_journal(&self.journal);
            }
        }
    }
    fn lock_file(path: &Path, message: &'static str) -> Result<File, Error> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|_| io_error("cannot access storage lock"))?;
        file.try_lock().map_err(|_| Error::new(ErrorKind::Busy, message))?;
        Ok(file)
    }
    fn resolved(path: &Path) -> Result<PathBuf, Error> {
        if let Ok(path) = path.canonicalize() {
            return Ok(path);
        }
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let parent = parent.canonicalize().map_err(|_| io_error("destination directory does not exist or is inaccessible"))?;
        Ok(parent.join(path.file_name().ok_or_else(|| invalid("destination must name a file"))?))
    }
    fn read(path: &Path, cap: usize) -> Result<Vec<u8>, Error> {
        let file = File::open(path).map_err(|_| io_error("cannot open project or recovery file; check its location and permissions"))?;
        if file.metadata().map_err(|_| io_error("cannot inspect file"))?.len() > cap as u64 {
            return Err(invalid("file exceeds size limit"));
        }
        let mut bytes = Vec::new();
        file.take(cap as u64 + 1).read_to_end(&mut bytes).map_err(|_| io_error("cannot read project or recovery file"))?;
        if bytes.len() > cap {
            return Err(invalid("file exceeds size limit"));
        }
        Ok(bytes)
    }
    fn io_error(message: &'static str) -> Error {
        Error::new(ErrorKind::Conflict, message)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn failed_reopen_worker_restores_closed_owner_and_exact_charge() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("recovery");
            let mut storage = NativeStorage::with_interval(path.clone(), Duration::from_secs(3600)).unwrap();
            let context = storage.asset_context().unwrap();
            storage.checkpoint(b"retained").unwrap();
            let usage = context.storage_usage().unwrap();
            drop(storage);
            assert!(NativeStorage::reopen_with_worker(context.clone(), |_| Err(io_error("injected worker spawn failure"))).is_err());
            assert_eq!(context.storage_usage().unwrap(), usage);
            assert_eq!(std::fs::read(&path).unwrap(), b"retained");
            assert!(NativeStorage::new(path).is_err());
            let reopened = NativeStorage::reopen(context).unwrap();
            assert_eq!(reopened.asset_context().unwrap().storage_usage().unwrap(), usage);
        }
        #[test]
        fn failed_existing_owner_quota_or_backing_adoption_leaves_no_charge_or_lock_transfer() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("recovery");
            std::fs::write(&path, b"existing").unwrap();
            let mut limits = crate::disk_quota::DiskUsage::HARD;
            limits.journals = 7;
            let quota = crate::disk_quota::DiskQuota::lower(limits, 640 * 1024 * 1024 * 1024).unwrap();
            assert!(NativeStorage::with_interval_quota(path.clone(), Duration::from_secs(3600), quota.clone()).is_err());
            assert_eq!(quota.usage().unwrap().total().unwrap(), 0);
            let quota = crate::disk_quota::DiskQuota::default();
            let permits: Vec<_> = (0..32).map(|_| quota.reserve_backing().unwrap()).collect();
            assert!(NativeStorage::with_interval_quota(path.clone(), Duration::from_secs(3600), quota.clone()).is_err());
            assert_eq!(quota.backing_count().unwrap(), 32);
            assert_eq!(quota.usage().unwrap().total().unwrap(), 0);
            drop(permits);
            assert_eq!(std::fs::read(&path).unwrap(), b"existing");
            assert!(NativeStorage::new(path).is_ok());
        }
        #[test]
        fn many_appends_share_one_explicit_durability_boundary() {
            let dir = tempfile::tempdir().unwrap();
            let mut storage = NativeStorage::with_interval(dir.path().join("recovery"), Duration::from_secs(3600)).unwrap();
            storage.checkpoint(b"checkpoint").unwrap();
            for _ in 0..100 {
                storage.append(b"small frame").unwrap();
            }
            assert_eq!(storage.journal.lock().unwrap().syncs, 0);
            storage.flush().unwrap();
            assert_eq!(storage.journal.lock().unwrap().syncs, 1);
            storage.flush().unwrap();
            assert_eq!(storage.journal.lock().unwrap().syncs, 1);
            storage.append(b"another").unwrap();
            storage.discard().unwrap();
            assert_eq!(storage.journal.lock().unwrap().syncs, 2);
            assert!(!storage.recovery.exists());
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::NativeStorage;
