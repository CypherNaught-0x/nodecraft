//! Sync tickets carry a generation and a captured append boundary; sync never owns the
//! append-state mutex. A per-generation failure token also closes the write/result race.
use super::*;
use std::sync::{
    MutexGuard,
    atomic::{AtomicBool, Ordering},
};

pub(super) trait SyncFile: Send + Sync {
    fn sync(&self, file: &File) -> std::io::Result<()>;
}
struct NativeSync;
impl SyncFile for NativeSync {
    fn sync(&self, file: &File) -> std::io::Result<()> {
        file.sync_all()
    }
}

#[derive(Default)]
pub(super) struct Journal {
    file: Option<File>,
    bytes: u64,
    generation: u64,
    appended: u64,
    durable: u64,
    failed: Arc<AtomicBool>,
    pub(super) stop: bool,
    pub(super) syncs: u64,
}
pub(super) struct SharedJournal {
    state: Mutex<Journal>,
    sync_gate: Mutex<()>,
    syncer: Arc<dyn SyncFile>,
}
struct Ticket {
    file: File,
    generation: u64,
    through: u64,
    failed: Arc<AtomicBool>,
}
impl SharedJournal {
    pub(super) fn new() -> Self {
        Self::with_syncer(Arc::new(NativeSync))
    }
    fn with_syncer(syncer: Arc<dyn SyncFile>) -> Self {
        Self { state: Mutex::new(Journal::default()), sync_gate: Mutex::new(()), syncer }
    }
    pub(super) fn lock(&self) -> std::sync::LockResult<MutexGuard<'_, Journal>> {
        self.state.lock()
    }
    fn state(&self) -> Result<MutexGuard<'_, Journal>, Error> {
        self.lock().map_err(|_| io_error("recovery worker lock is unavailable"))
    }
    pub(super) fn fail(&self) -> Result<(), Error> {
        self.state()?.failed.store(true, Ordering::Release);
        Ok(())
    }
    pub(super) fn checkpoint(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        let mut state = self.state()?;
        let generation = state.generation.checked_add(1).ok_or_else(|| io_error("recovery generation limit reached"))?;
        let failed = Arc::new(AtomicBool::new(false));
        craft_atomic::WriteOptions::default()
            .with_new_file_permissions(craft_atomic::NewFilePermissions::Private)
            .atomic_write(path, bytes)
            .map_err(|_| io_error("cannot write recovery checkpoint; check permissions and free space"))?;
        // The old file/tickets remain owned by their generation until its sync finishes.
        state.file = None;
        state.bytes = bytes.len() as u64;
        state.generation = generation;
        state.appended = 0;
        state.durable = 0;
        state.failed = failed;
        Ok(())
    }
    pub(super) fn append(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        let mut state = self.state()?;
        if state.failed.load(Ordering::Acquire) {
            return Err(io_error("recovery requires a new checkpoint before editing"));
        }
        let sequence = state.appended.checked_add(1).ok_or_else(|| io_error("recovery append sequence limit reached"))?;
        if state.bytes.saturating_add(bytes.len() as u64) > MAX_JOURNAL_BYTES as u64 {
            return Err(invalid("recovery journal is full; save a checkpoint"));
        }
        if state.file.is_none() {
            let file = OpenOptions::new().append(true).open(path).map_err(|_| io_error("cannot open recovery journal; save a new checkpoint"))?;
            state.bytes = file.metadata().map_err(|_| io_error("cannot inspect recovery journal"))?.len();
            state.file = Some(file);
            if state.bytes.saturating_add(bytes.len() as u64) > MAX_JOURNAL_BYTES as u64 {
                return Err(invalid("recovery journal is full; save a checkpoint"));
            }
        }
        let result = state.file.as_mut().ok_or_else(|| io_error("recovery handle is unavailable"))?.write_all(bytes);
        if result.is_err() {
            state.failed.store(true, Ordering::Release);
            return Err(io_error("cannot append recovery journal; save a checkpoint before editing"));
        }
        state.bytes = state.bytes.saturating_add(bytes.len() as u64);
        state.appended = sequence;
        // A sync failure can become known during the write, without waiting for this lock.
        if state.failed.load(Ordering::Acquire) {
            return Err(io_error("recovery sync failed during append; save a new checkpoint"));
        }
        Ok(())
    }
    pub(super) fn flush(&self) -> Result<(), Error> {
        let (generation, through) = {
            let state = self.state()?;
            (state.generation, state.appended)
        };
        let _gate = self.sync_gate.lock().map_err(|_| io_error("recovery sync lock is unavailable"))?;
        let ticket = {
            let state = self.state()?;
            // A published replacement checkpoint is already durable and supersedes this boundary.
            if state.generation != generation {
                return Ok(());
            }
            if state.failed.load(Ordering::Acquire) {
                return Err(io_error("recovery requires a new checkpoint before editing"));
            }
            if state.durable >= through {
                return Ok(());
            }
            let file = match state.file.as_ref().ok_or_else(|| io_error("recovery handle is unavailable"))?.try_clone() {
                Ok(file) => file,
                Err(_) => {
                    state.failed.store(true, Ordering::Release);
                    return Err(io_error("cannot clone recovery sync handle; save a new checkpoint"));
                }
            };
            Ticket { file, generation, through: state.appended, failed: state.failed.clone() }
        };
        let result = self.syncer.sync(&ticket.file);
        if result.is_err() {
            ticket.failed.store(true, Ordering::Release);
        }
        let mut state = self.state()?;
        if state.generation != ticket.generation {
            return Ok(());
        }
        if result.is_err() {
            return Err(io_error("cannot sync recovery journal; save a new checkpoint"));
        }
        state.durable = state.durable.max(ticket.through);
        state.syncs = state.syncs.saturating_add(1);
        if state.failed.load(Ordering::Acquire) {
            return Err(io_error("recovery requires a new checkpoint before editing"));
        }
        Ok(())
    }
    pub(super) fn discard(&self, path: &Path) -> Result<(), Error> {
        self.flush()?;
        let mut state = self.state()?;
        let generation = state.generation.checked_add(1).ok_or_else(|| io_error("recovery generation limit reached"))?;
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(io_error("cannot discard recovery file")),
        }
        state.file = None;
        state.bytes = 0;
        state.generation = generation;
        state.appended = 0;
        state.durable = 0;
        state.failed = Arc::new(AtomicBool::new(false));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, Sender};

    struct ControlledSync {
        entered: Sender<()>,
        release: Mutex<Receiver<bool>>,
    }
    impl SyncFile for ControlledSync {
        fn sync(&self, _: &File) -> std::io::Result<()> {
            self.entered.send(()).map_err(|_| std::io::Error::other("test receiver closed"))?;
            if self.release.lock().map_err(|_| std::io::Error::other("test lock poisoned"))?.recv().map_err(|_| std::io::Error::other("test sender closed"))? {
                Ok(())
            } else {
                Err(std::io::Error::other("injected sync failure"))
            }
        }
    }
    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
        journal: Arc<SharedJournal>,
        entered: Receiver<()>,
        release: Sender<bool>,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("recovery");
            let (entered_tx, entered) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let journal = Arc::new(SharedJournal::with_syncer(Arc::new(ControlledSync { entered: entered_tx, release: Mutex::new(release_rx) })));
            journal.checkpoint(&path, b"checkpoint").unwrap();
            Self { _dir: dir, path, journal, entered, release }
        }
        fn syncing(&self) -> JoinHandle<Result<(), Error>> {
            let journal = self.journal.clone();
            let worker = thread::spawn(move || journal.flush());
            self.entered.recv_timeout(Duration::from_secs(5)).unwrap();
            worker
        }
    }
    #[test]
    fn append_progresses_while_sync_is_blocked_and_new_append_remains_dirty() {
        let f = Fixture::new();
        f.journal.append(&f.path, b"first").unwrap();
        let sync = f.syncing();
        let journal = f.journal.clone();
        let path = f.path.clone();
        let (sent, received) = mpsc::channel();
        let append = thread::spawn(move || sent.send(journal.append(&path, b"second")).unwrap());
        let progressed = received.recv_timeout(Duration::from_secs(5));
        // Always release/join before assertions, so a regression cannot hang cleanup.
        f.release.send(true).unwrap();
        sync.join().unwrap().unwrap();
        append.join().unwrap();
        assert!(progressed.unwrap().is_ok(), "append must complete without waiting for sync");
        {
            let state = f.journal.lock().unwrap();
            assert_eq!((state.durable, state.appended), (1, 2));
        }
        let sync = f.syncing();
        f.release.send(true).unwrap();
        sync.join().unwrap().unwrap();
        let state = f.journal.lock().unwrap();
        assert_eq!((state.durable, state.appended), (2, 2));
    }
    #[test]
    fn explicit_flush_waits_for_its_boundary_and_does_not_clear_later_appends() {
        let f = Fixture::new();
        f.journal.append(&f.path, b"first").unwrap();
        let worker = f.syncing();
        f.journal.append(&f.path, b"second").unwrap();
        let journal = f.journal.clone();
        let (sent, received) = mpsc::channel();
        let flush = thread::spawn(move || sent.send(journal.flush()).unwrap());
        f.release.send(true).unwrap();
        worker.join().unwrap().unwrap();
        f.entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(received.try_recv(), Err(mpsc::TryRecvError::Empty)));
        f.journal.append(&f.path, b"third").unwrap();
        f.release.send(true).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        flush.join().unwrap();
        let state = f.journal.lock().unwrap();
        assert_eq!((state.durable, state.appended), (2, 3));
    }
    #[test]
    fn stale_success_or_failure_cannot_clear_or_poison_replacement_generation() {
        for success in [true, false] {
            let f = Fixture::new();
            f.journal.append(&f.path, b"old append").unwrap();
            let old = f.syncing();
            f.journal.checkpoint(&f.path, b"new checkpoint").unwrap();
            f.journal.append(&f.path, b"new append").unwrap();
            f.release.send(success).unwrap();
            old.join().unwrap().unwrap();
            {
                let state = f.journal.lock().unwrap();
                assert_eq!((state.durable, state.appended), (0, 1));
                assert!(!state.failed.load(Ordering::Acquire));
            }
            assert_eq!(std::fs::read(&f.path).unwrap(), b"new checkpointnew append");
            let new = f.syncing();
            f.release.send(true).unwrap();
            new.join().unwrap().unwrap();
            assert_eq!(f.journal.lock().unwrap().durable, 1);
        }
    }
    #[test]
    fn current_failure_latches_until_checkpoint_and_close_waits_for_sync() {
        let f = Fixture::new();
        f.journal.append(&f.path, b"append").unwrap();
        let sync = f.syncing();
        f.release.send(false).unwrap();
        assert!(sync.join().unwrap().is_err());
        assert!(f.journal.append(&f.path, b"refused").is_err());
        assert!(f.journal.flush().is_err());
        f.journal.checkpoint(&f.path, b"repair").unwrap();
        f.journal.append(&f.path, b"new").unwrap();
        let journal = f.journal.clone();
        let path = f.path.clone();
        let (sent, received) = mpsc::channel();
        let close = thread::spawn(move || sent.send(journal.discard(&path)).unwrap());
        f.entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(f.path.exists());
        assert!(matches!(received.try_recv(), Err(mpsc::TryRecvError::Empty)));
        f.release.send(true).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        close.join().unwrap();
        assert!(!f.path.exists());
    }
    #[test]
    fn counter_overflow_fails_before_checkpoint_or_append_publication() {
        let f = Fixture::new();
        f.journal.lock().unwrap().generation = u64::MAX;
        assert!(f.journal.checkpoint(&f.path, b"must not publish").is_err());
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpoint");
        f.journal.lock().unwrap().appended = u64::MAX;
        assert!(f.journal.append(&f.path, b"must not append").is_err());
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpoint");
    }

    #[test]
    fn failed_checkpoint_keeps_original_generation_handle_and_dirty_boundary() {
        let f = Fixture::new();
        f.journal.append(&f.path, b"retained append").unwrap();
        let before = {
            let state = f.journal.lock().unwrap();
            (state.generation, state.appended, state.durable)
        };
        let directory = f._dir.path().join("not-a-file");
        std::fs::create_dir(&directory).unwrap();
        assert!(f.journal.checkpoint(&directory, b"must not publish").is_err());
        {
            let state = f.journal.lock().unwrap();
            assert_eq!((state.generation, state.appended, state.durable), before);
            assert!(!state.failed.load(Ordering::Acquire));
        }
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpointretained append");
        let sync = f.syncing();
        f.release.send(true).unwrap();
        sync.join().unwrap().unwrap();
        assert_eq!(f.journal.lock().unwrap().durable, 1);
    }
}
