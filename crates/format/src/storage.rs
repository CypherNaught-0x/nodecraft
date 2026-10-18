//! Platform storage boundary. Errors are public-safe and never contain a path or document data.
use crate::{MAX_ARCHIVE_BYTES, invalid, journal::MAX_JOURNAL_BYTES};
use nodecraft_types::{Error, ErrorKind};
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
mod native {
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
    mod durability;

    pub const GROUP_SYNC_INTERVAL: Duration = Duration::from_millis(250);
    /// Private recovery paths and per-user canonical destination locks prevent concurrent writers.
    /// Journal bytes become kernel-visible before publication; the worker syncs pending bytes
    /// every 250 ms plus scheduling/filesystem latency. Explicit flush/close closes that window.
    pub struct NativeStorage {
        recovery: PathBuf,
        lock: PathBuf,
        _lock_file: File,
        destinations: destinations::Destinations,
        journal: Arc<durability::SharedJournal>,
        worker: Option<JoinHandle<()>>,
    }
    impl NativeStorage {
        pub fn new(recovery: PathBuf) -> Result<Self, Error> {
            Self::with_interval(recovery, GROUP_SYNC_INTERVAL)
        }
        fn with_interval(recovery: PathBuf, interval: Duration) -> Result<Self, Error> {
            let mut name = recovery.as_os_str().to_os_string();
            name.push(".lock");
            let lock = PathBuf::from(name);
            let file = lock_file(&lock, "recovery location is already in use")?;
            let journal = Arc::new(durability::SharedJournal::new());
            let state = journal.clone();
            let worker = thread::Builder::new()
                .name("nodecraft-recovery".into())
                .spawn(move || {
                    loop {
                        thread::park_timeout(interval);
                        // A failed sync is latched; append/flush surfaces it to the session.
                        let _ = state.flush();
                        if state.lock().map_or(true, |state| state.stop) {
                            return;
                        }
                    }
                })
                .map_err(|_| io_error("cannot start recovery durability worker"))?;
            Ok(Self { recovery, lock, _lock_file: file, destinations: destinations::Destinations::new(), journal, worker: Some(worker) })
        }
        pub fn recover(&self) -> Result<crate::journal::Recovered, Error> {
            let recovered = crate::journal::recover(&read(&self.recovery, MAX_JOURNAL_BYTES)?)?;
            if recovered.damage.is_some() {
                // Reading never repairs/truncates the evidence. No append may strand
                // new edits after the damaged frame; only a fresh checkpoint clears it.
                self.journal.fail()?;
            }
            Ok(recovered)
        }
    }
    impl ProjectStorage for NativeStorage {
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
            craft_atomic::atomic_write(&candidate.path, archive).map_err(|_| io_error("cannot save project; check destination permissions and free space"))?;
            let candidate = self.destinations.published(candidate);
            let access = candidate.access;
            self.destinations.adopt(candidate);
            Ok(access)
        }
        fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
            if recovery.len() > MAX_JOURNAL_BYTES {
                return Err(invalid("recovery exceeds size limit"));
            }
            self.journal.checkpoint(&self.recovery, recovery)
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
