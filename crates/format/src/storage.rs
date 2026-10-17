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
    fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error>;
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

    pub const GROUP_SYNC_INTERVAL: Duration = Duration::from_millis(250);
    #[derive(Default)]
    struct Journal {
        file: Option<File>,
        bytes: u64,
        dirty: bool,
        failed: bool,
        stop: bool,
        syncs: u64,
    }
    impl Journal {
        fn flush(&mut self) -> Result<(), Error> {
            if self.failed {
                return Err(io_error("recovery durability failed; save a new checkpoint"));
            }
            if self.dirty {
                let result = self.file.as_ref().ok_or_else(|| io_error("recovery handle is unavailable"))?.sync_all();
                if result.is_err() {
                    self.failed = true;
                    return Err(io_error("cannot sync recovery journal; save a new checkpoint"));
                }
                self.dirty = false;
                self.syncs = self.syncs.saturating_add(1);
            }
            Ok(())
        }
    }
    /// Private recovery paths and canonical sibling project locks prevent concurrent writers.
    /// Journal bytes become kernel-visible before publication; the worker syncs pending bytes
    /// every 250 ms plus scheduling/filesystem latency. Explicit flush/close closes that window.
    pub struct NativeStorage {
        recovery: PathBuf,
        lock: PathBuf,
        _lock_file: File,
        projects: BTreeMap<PathBuf, File>,
        journal: Arc<Mutex<Journal>>,
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
            let journal = Arc::new(Mutex::new(Journal::default()));
            let state = journal.clone();
            let worker = thread::Builder::new()
                .name("nodecraft-recovery".into())
                .spawn(move || {
                    loop {
                        thread::park_timeout(interval);
                        let Ok(mut state) = state.lock() else {
                            return;
                        };
                        // A failed sync is latched; append/flush surfaces it to the session.
                        let _ = state.flush();
                        if state.stop {
                            return;
                        }
                    }
                })
                .map_err(|_| io_error("cannot start recovery durability worker"))?;
            Ok(Self { recovery, lock, _lock_file: file, projects: BTreeMap::new(), journal, worker: Some(worker) })
        }
        pub fn recover(&self) -> Result<crate::journal::Recovered, Error> {
            crate::journal::recover(&read(&self.recovery, MAX_JOURNAL_BYTES)?)
        }
        fn project(&mut self, location: &StorageLocation) -> Result<PathBuf, Error> {
            let StorageLocation::Path(path) = location else {
                return Err(Error::new(ErrorKind::Unsupported, "native storage requires a filesystem path"));
            };
            let path = resolved(path)?;
            if path == resolved(&self.recovery)? || path == resolved(&self.lock)? {
                return Err(invalid("project location conflicts with session recovery files"));
            }
            if !self.projects.contains_key(&path) {
                if self.projects.len() >= 64 {
                    return Err(Error::new(ErrorKind::Busy, "session project-lock limit reached; open a new session"));
                }
                let mut name = path.as_os_str().to_os_string();
                name.push(".nodecraft.lock");
                let file = lock_file(&PathBuf::from(name), "project is already open in another session")?;
                self.projects.insert(path.clone(), file);
            }
            Ok(path)
        }
    }
    impl ProjectStorage for NativeStorage {
        fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
            read(&self.project(location)?, MAX_ARCHIVE_BYTES)
        }
        fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error> {
            if archive.len() > MAX_ARCHIVE_BYTES {
                return Err(invalid("project exceeds archive size limit"));
            }
            craft_atomic::atomic_write(&self.project(location)?, archive)
                .map_err(|_| io_error("cannot save project; check destination permissions and free space"))
        }
        fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
            if recovery.len() > MAX_JOURNAL_BYTES {
                return Err(invalid("recovery exceeds size limit"));
            }
            let mut state = self.journal.lock().map_err(|_| io_error("recovery worker lock is unavailable"))?;
            craft_atomic::WriteOptions::default()
                .with_new_file_permissions(craft_atomic::NewFilePermissions::Private)
                .atomic_write(&self.recovery, recovery)
                .map_err(|_| io_error("cannot write recovery checkpoint; check permissions and free space"))?;
            // Reopen lazily on the first append, without turning an already published checkpoint
            // into a misleading failure if a subsequent open is denied.
            state.file = None;
            state.bytes = recovery.len() as u64;
            state.dirty = false;
            state.failed = false;
            Ok(())
        }
        fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
            let mut state = self.journal.lock().map_err(|_| io_error("recovery worker lock is unavailable"))?;
            if state.failed {
                return Err(io_error("recovery durability failed; save a new checkpoint"));
            }
            if state.bytes.saturating_add(frame.len() as u64) > MAX_JOURNAL_BYTES as u64 {
                return Err(invalid("recovery journal is full; save a checkpoint"));
            }
            if state.file.is_none() {
                let file = OpenOptions::new().append(true).open(&self.recovery).map_err(|_| io_error("cannot open recovery journal; save a new checkpoint"))?;
                state.bytes = file.metadata().map_err(|_| io_error("cannot inspect recovery journal"))?.len();
                state.file = Some(file);
                if state.bytes.saturating_add(frame.len() as u64) > MAX_JOURNAL_BYTES as u64 {
                    return Err(invalid("recovery journal is full; save a checkpoint"));
                }
            }
            // Mark first: a partial write is ambiguous and must disable further appends.
            state.failed = true;
            state
                .file
                .as_mut()
                .ok_or_else(|| io_error("recovery handle is unavailable"))?
                .write_all(frame)
                .map_err(|_| io_error("cannot append recovery journal; save a checkpoint before editing"))?;
            state.bytes = state.bytes.saturating_add(frame.len() as u64);
            state.dirty = true;
            state.failed = false;
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Error> {
            self.journal.lock().map_err(|_| io_error("recovery worker lock is unavailable"))?.flush()
        }
        fn discard(&mut self) -> Result<(), Error> {
            let mut state = self.journal.lock().map_err(|_| io_error("recovery worker lock is unavailable"))?;
            state.flush()?;
            state.file = None;
            match std::fs::remove_file(&self.recovery) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(io_error("cannot discard recovery file")),
            }
            state.bytes = 0;
            state.dirty = false;
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
                if worker.join().is_err()
                    && let Ok(mut state) = self.journal.lock()
                {
                    state.failed = true;
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
