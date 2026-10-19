//! Actual recovery handles, quota generations and single-use publication ownership.
//! No owner/generation points back to an asset workspace or session context.
use super::*;
use crate::{
    RecordedAssets,
    disk_quota::{DiskPool, DiskQuota, DiskReservation},
    native_atomic::AtomicFile,
};
use std::{
    io::{Seek, SeekFrom},
    sync::{
        MutexGuard,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
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
/// The held cooperative OS lock is authority. same-file equality is only a fallible
/// mismatch diagnostic; protected owned paths and verified bytes remain required.
struct RecoveryLock {
    file: same_file::Handle,
    path: PathBuf,
}
impl RecoveryLock {
    fn check(&self) -> Result<(), Error> {
        let current = same_file::Handle::from_path(&self.path).map_err(|_| io_error("recovery lock path is unavailable"))?;
        if current != self.file {
            return Err(io_error("recovery lock path changed; preserve recovery and reopen its owner"));
        }
        Ok(())
    }
}
/// File ownership is first, so its final close precedes byte/backing/lock release.
/// Tickets and prepared plans pin this exact allocation, including after rename.
struct Generation {
    file: Arc<AtomicFile>,
    charge: Mutex<DiskReservation>,
    failed: AtomicBool,
    retired: AtomicBool,
    _lock: Arc<RecoveryLock>,
}
impl Generation {
    fn pending(quota: &DiskQuota, lock: &Arc<RecoveryLock>, bytes: u64) -> Result<Arc<Self>, Error> {
        let charge = quota.reserve(DiskPool::Journals, bytes)?;
        let file = AtomicFile::prepare(quota)?;
        Ok(Arc::new(Self { file, charge: Mutex::new(charge), failed: AtomicBool::new(false), retired: AtomicBool::new(false), _lock: lock.clone() }))
    }
    fn existing(quota: &DiskQuota, lock: &Arc<RecoveryLock>, file: File, bytes: u64) -> Result<Arc<Self>, Error> {
        let charge = quota.reserve(DiskPool::Journals, bytes)?;
        let file = AtomicFile::from_existing(quota, file)?;
        Ok(Arc::new(Self { file, charge: Mutex::new(charge), failed: AtomicBool::new(true), retired: AtomicBool::new(false), _lock: lock.clone() }))
    }
    fn grow(&self, bytes: u64) -> Result<(), Error> {
        let mut charge = self.charge.lock().map_err(|_| io_error("recovery quota ownership is unavailable"))?;
        if bytes > charge.byte_len() {
            charge.resize(bytes)?;
        }
        Ok(())
    }
    fn check_path(&self, path: &Path, bytes: u64) -> Result<(), Error> {
        self.file.with_file(|file| {
            let meta = file.metadata().map_err(|_| io_error("cannot inspect retained recovery file"))?;
            let high_water = self.charge.lock().map_err(|_| io_error("recovery quota ownership is unavailable"))?.byte_len();
            let extent_matches = if self.failed.load(Ordering::Acquire) { meta.len() >= bytes && meta.len() <= high_water } else { meta.len() == bytes };
            if !meta.is_file() || !extent_matches {
                return Err(io_error("recovery extent changed; preserve evidence and checkpoint"));
            }
            let held = same_file::Handle::from_file(file.try_clone().map_err(|_| io_error("cannot inspect recovery association"))?)
                .map_err(|_| io_error("cannot inspect recovery association"))?;
            let current = same_file::Handle::from_path(path).map_err(|_| io_error("published recovery path is unavailable"))?;
            if held != current {
                return Err(io_error("recovery path changed; preserve evidence and reopen its owner"));
            }
            Ok(())
        })
    }
}
#[derive(Clone)]
pub(crate) enum Wire {
    Legacy,
    Streaming { identity: nodecraft_types::Blake3, checkpoint_bytes: u64, recorded: Arc<RecordedAssets> },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Active,
    Closed,
    Claiming,
    Discarded,
}
pub(super) struct Journal {
    current: Option<Arc<Generation>>,
    bytes: u64,
    generation: u64,
    appended: u64,
    durable: u64,
    wire: Wire,
    admitted: bool,
    phase: Phase,
    pub(super) stop: bool,
    pub(super) syncs: u64,
}
/// Workspace -> SharedJournal -> Generation -> quota/lock. No back edge.
pub(crate) struct SharedJournal {
    state: Mutex<Journal>,
    operation: Mutex<()>,
    sync_gate: Mutex<()>,
    syncer: Arc<dyn SyncFile>,
    quota: DiskQuota,
    recovery: PathBuf,
    lock: Arc<RecoveryLock>,
}
const PENDING: u8 = 0;
const IN_FLIGHT: u8 = 1;
const PUBLISHED: u8 = 2;
const FAILED: u8 = 3;
pub(crate) struct Publication {
    owner: Arc<SharedJournal>,
    generation: Arc<Generation>,
    phase: AtomicU8,
    wire: Wire,
    bytes: u64,
}
struct PublicationClaim<'a> {
    publication: &'a Publication,
    rollback: u8,
}
impl Drop for PublicationClaim<'_> {
    fn drop(&mut self) {
        self.publication.phase.store(self.rollback, Ordering::Release);
    }
}
struct Ticket {
    // The cloned File closes before dropping the generation that pays for it.
    file: File,
    generation: Arc<Generation>,
    ordinal: u64,
    through: u64,
}
pub(super) struct ReopenClaim<'a> {
    owner: &'a SharedJournal,
    _operation: MutexGuard<'a, ()>,
    committed: bool,
}
impl ReopenClaim<'_> {
    pub(super) fn commit(mut self) {
        let mut state = self.owner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.phase = Phase::Active;
        state.stop = false;
        self.committed = true;
    }
}
impl Drop for ReopenClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.owner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).phase = Phase::Closed;
        }
    }
}
impl SharedJournal {
    pub(super) fn new(recovery: PathBuf, lock_path: PathBuf, lock_file: File, quota: DiskQuota) -> Result<Self, Error> {
        Self::with_syncer(recovery, lock_path, lock_file, quota, Arc::new(NativeSync))
    }
    fn with_syncer(recovery: PathBuf, lock_path: PathBuf, lock_file: File, quota: DiskQuota, syncer: Arc<dyn SyncFile>) -> Result<Self, Error> {
        let lock = Arc::new(RecoveryLock {
            file: same_file::Handle::from_file(lock_file).map_err(|_| io_error("cannot retain recovery lock identity"))?,
            path: lock_path,
        });
        lock.check()?;
        let current = match OpenOptions::new().read(true).write(true).open(&recovery) {
            Ok(file) => {
                let meta = file.metadata().map_err(|_| io_error("cannot inspect recovery file"))?;
                if !meta.is_file() || meta.len() > crate::frame_plan::MAX_STREAM_JOURNAL_BYTES {
                    return Err(invalid("recovery exceeds native journal limit"));
                }
                Some((Generation::existing(&quota, &lock, file, meta.len())?, meta.len()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(io_error("cannot retain recovery file; check permissions")),
        };
        let (current, bytes) = current.map_or((None, 0), |(generation, bytes)| (Some(generation), bytes));
        Ok(Self {
            state: Mutex::new(Journal {
                current,
                bytes,
                generation: 0,
                appended: 0,
                durable: 0,
                wire: Wire::Legacy,
                admitted: false,
                phase: Phase::Active,
                stop: false,
                syncs: 0,
            }),
            operation: Mutex::new(()),
            sync_gate: Mutex::new(()),
            syncer,
            quota,
            recovery,
            lock,
        })
    }
    pub(super) fn paths(&self) -> (&Path, &Path) {
        (&self.recovery, &self.lock.path)
    }
    pub(super) fn lock(&self) -> std::sync::LockResult<MutexGuard<'_, Journal>> {
        self.state.lock()
    }
    fn state(&self) -> Result<MutexGuard<'_, Journal>, Error> {
        self.lock().map_err(|_| io_error("recovery state is unavailable"))
    }
    fn operation(&self) -> Result<MutexGuard<'_, ()>, Error> {
        self.operation.try_lock().map_err(|_| Error::new(ErrorKind::Busy, "recovery publication or adoption is in progress; retry"))
    }
    fn active(state: &Journal) -> Result<(), Error> {
        if state.phase != Phase::Active {
            return Err(Error::new(ErrorKind::Busy, "recovery owner is not active"));
        }
        Ok(())
    }
    fn check_association(&self) -> Result<(), Error> {
        self.lock.check()?;
        let (current, bytes) = {
            let state = self.state()?;
            (state.current.clone(), state.bytes)
        };
        if let Some(current) = current {
            current.check_path(&self.recovery, bytes)?;
        } else if self.recovery.try_exists().map_err(|_| io_error("cannot inspect recovery location"))? {
            return Err(io_error("unadmitted recovery appeared at the owned location"));
        }
        Ok(())
    }
    pub(crate) fn prepare(self: &Arc<Self>, bytes: u64, wire: Wire) -> Result<Arc<Publication>, Error> {
        if bytes > crate::frame_plan::MAX_STREAM_JOURNAL_BYTES {
            return Err(invalid("checkpoint exceeds journal limit"));
        }
        {
            let state = self.state()?;
            if state.phase != Phase::Active && state.phase != Phase::Discarded {
                return Err(Error::new(ErrorKind::Busy, "recovery owner is not active"));
            }
        }
        let generation = Generation::pending(&self.quota, &self.lock, bytes)?;
        Ok(Arc::new(Publication { owner: self.clone(), generation, phase: AtomicU8::new(PENDING), wire, bytes }))
    }
    pub(super) fn fail(&self) -> Result<(), Error> {
        if let Some(current) = &self.state()?.current {
            current.failed.store(true, Ordering::Release);
        }
        Ok(())
    }
    pub(super) fn begin_reopen(&self) -> Result<ReopenClaim<'_>, Error> {
        let operation = self.operation()?;
        {
            let mut state = self.state()?;
            if state.phase != Phase::Closed {
                return Err(Error::new(ErrorKind::Busy, "recovery owner is already active or cannot be reopened"));
            }
            state.phase = Phase::Claiming;
        }
        let claim = ReopenClaim { owner: self, _operation: operation, committed: false };
        self.check_association()?;
        Ok(claim)
    }
    pub(super) fn close(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let discarded = state.phase == Phase::Discarded;
        if !discarded {
            state.phase = Phase::Closed;
        }
        discarded
    }
    pub(super) fn checkpoint(self: &Arc<Self>, workspace: &crate::NativeAssetWorkspace, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid("legacy recovery exceeds size limit"));
        }
        let plan = self.prepare(bytes.len() as u64, Wire::Legacy)?;
        self.publish(workspace, &plan, |file| file.write_all(bytes).map_err(|_| io_error("cannot write recovery checkpoint")))
    }
    pub(super) fn checkpoint_plan(
        self: &Arc<Self>,
        workspace: &crate::NativeAssetWorkspace,
        plan: &CheckpointPlan,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<(), Error> {
        let publication = plan.native_publication()?;
        self.publish(workspace, publication, |file| plan.write_to(file, cancel, metrics))
    }
    fn publish(
        self: &Arc<Self>,
        workspace: &crate::NativeAssetWorkspace,
        publication: &Publication,
        write: impl FnOnce(&mut dyn Write) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if !Arc::ptr_eq(self, &publication.owner) || !Arc::ptr_eq(self, &workspace.journal()?) {
            return Err(io_error("checkpoint plan belongs to another recovery owner"));
        }
        let _operation = self.operation()?;
        let stage = self.quota.stage_operation()?;
        {
            let state = self.state()?;
            if state.phase != Phase::Active && state.phase != Phase::Discarded {
                return Err(Error::new(ErrorKind::Busy, "recovery owner is not active"));
            }
        }
        self.check_association()?;
        let ordinal = self.state()?.generation.checked_add(1).ok_or_else(|| io_error("recovery generation limit reached"))?;
        publication
            .phase
            .compare_exchange(PENDING, IN_FLIGHT, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io_error("checkpoint publication plan is already used, failed or in flight"))?;
        let mut claim = PublicationClaim { publication, rollback: FAILED };
        match workspace.publish_file(&stage, &self.recovery, publication.bytes, true, &publication.generation.file, write) {
            Ok(()) => {
                // Every field and handle already exists. Poison cannot turn an actual
                // rename into an error; later fallible operations still refuse poison.
                let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let old = state.current.replace(publication.generation.clone());
                state.bytes = publication.bytes;
                state.generation = ordinal;
                state.appended = 0;
                state.durable = 0;
                state.wire = publication.wire.clone();
                state.admitted = true;
                state.phase = Phase::Active;
                drop(state);
                if let Some(old) = old {
                    old.retired.store(true, Ordering::Release);
                }
                claim.rollback = PUBLISHED;
                Ok(())
            }
            Err(failure) => {
                claim.rollback = if failure.cleanup_complete { PENDING } else { FAILED };
                Err(failure.error)
            }
        }
    }
    pub(super) fn append(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        if path != self.recovery {
            return Err(io_error("append location belongs to another recovery owner"));
        }
        self.append_with(bytes.len() as u64, None, |file| file.write_all(bytes).map_err(|_| io_error("cannot append legacy recovery frame")))
    }
    pub(super) fn append_plan(&self, plan: &crate::FramePlan, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        self.append_with(plan.byte_len(), Some(plan), |file| plan.write_to(file, cancel, metrics))
    }
    fn append_with(&self, bytes: u64, plan: Option<&crate::FramePlan>, write: impl FnOnce(&mut dyn Write) -> Result<(), Error>) -> Result<(), Error> {
        let _operation = self.operation()?;
        self.check_association()?;
        let (current, sequence, before, next_recorded) = {
            let state = self.state()?;
            Self::active(&state)?;
            let current = state.current.clone().ok_or_else(|| io_error("recovery requires a checkpoint before editing"))?;
            if current.failed.load(Ordering::Acquire) {
                return Err(io_error("recovery requires a new checkpoint before editing"));
            }
            let sequence = state.appended.checked_add(1).ok_or_else(|| io_error("recovery append sequence limit reached"))?;
            let end = state.bytes.checked_add(bytes).ok_or_else(|| invalid("recovery extent overflow"))?;
            let next_recorded = match (&state.wire, plan) {
                (Wire::Legacy, None) => {
                    if end > MAX_JOURNAL_BYTES as u64 {
                        return Err(invalid("recovery journal is full; save a checkpoint"));
                    }
                    None
                }
                (Wire::Streaming { identity, checkpoint_bytes, recorded }, Some(plan)) => {
                    if end > crate::frame_plan::MAX_STREAM_JOURNAL_BYTES
                        || end.checked_sub(*checkpoint_bytes).is_none_or(|tail| tail > crate::frame_plan::MAX_STREAM_TAIL_BYTES)
                    {
                        return Err(invalid("recovery tail is full; Save before editing"));
                    }
                    Some(plan.admit_journal(*identity, sequence, recorded)?)
                }
                _ => return Err(invalid("recovery frame capability does not match its checkpoint")),
            };
            (current, sequence, state.bytes, next_recorded)
        };
        let end = before.checked_add(bytes).ok_or_else(|| invalid("recovery extent overflow"))?;
        current.grow(end)?; // High water remains charged even after a partial/failed write.
        let written = current.file.with_file(|file| {
            file.seek(SeekFrom::End(0)).map_err(|_| io_error("cannot seek recovery append boundary"))?;
            let mut bounded = AppendWriter { file, left: bytes };
            write(&mut bounded)?;
            if bounded.left != 0 {
                return Err(io_error("recovery frame did not fill its admitted extent"));
            }
            Ok(())
        });
        if let Err(error) = written {
            current.failed.store(true, Ordering::Release);
            return Err(error);
        }
        let mut state = self.state().inspect_err(|_| current.failed.store(true, Ordering::Release))?;
        state.bytes = end;
        state.appended = sequence;
        if let (Some(recorded), Wire::Streaming { recorded: old, .. }) = (next_recorded, &mut state.wire) {
            *old = recorded;
        }
        if current.failed.load(Ordering::Acquire) {
            return Err(io_error("recovery sync failed during append; save a new checkpoint"));
        }
        Ok(())
    }
    pub(super) fn flush(&self) -> Result<(), Error> {
        let (ordinal, through) = {
            let state = self.state()?;
            Self::active(&state)?;
            (state.generation, state.appended)
        };
        let _gate = self.sync_gate.lock().map_err(|_| io_error("recovery sync lock is unavailable"))?;
        // Capturing the generation Arc while Active is the pin. File cloning may
        // wait for a writer's cursor, so it must occur outside the state mutex too.
        let (current, through) = {
            let state = self.state()?;
            if state.generation != ordinal || state.phase != Phase::Active {
                return Ok(());
            }
            let current = state.current.as_ref();
            if current.is_some_and(|generation| generation.failed.load(Ordering::Acquire)) {
                return Err(io_error("recovery requires a new checkpoint before editing"));
            }
            if state.durable >= through {
                return Ok(());
            }
            let current = current.ok_or_else(|| io_error("recovery handle is unavailable"))?;
            (current.clone(), state.appended)
        };
        let file = current.file.clone_file().inspect_err(|_| current.failed.store(true, Ordering::Release))?;
        let ticket = Ticket { file, generation: current, ordinal, through };
        let result = self.syncer.sync(&ticket.file);
        if result.is_err() {
            ticket.generation.failed.store(true, Ordering::Release);
        }
        let mut state = self.state()?;
        if state.generation != ticket.ordinal || !state.current.as_ref().is_some_and(|current| Arc::ptr_eq(current, &ticket.generation)) {
            return Ok(());
        }
        if result.is_err() {
            return Err(io_error("cannot sync recovery journal; save a new checkpoint"));
        }
        state.durable = state.durable.max(ticket.through);
        state.syncs = state.syncs.saturating_add(1);
        if ticket.generation.failed.load(Ordering::Acquire) {
            return Err(io_error("recovery requires a new checkpoint before editing"));
        }
        Ok(())
    }
    pub(super) fn recover(
        &self,
        context: AssetContext,
        workspace: &crate::NativeAssetWorkspace,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<crate::journal::Recovered, Error> {
        let _operation = self.operation()?;
        self.check_association().inspect_err(|_| {
            let _ = self.fail();
        })?;
        let current = {
            let state = self.state()?;
            Self::active(&state)?;
            state.current.clone()
        }
        .ok_or_else(|| io_error("recovery file does not exist"))?;
        let parsed = current.file.with_file(|file| {
            let length = file.metadata().map_err(|_| io_error("cannot inspect retained recovery source"))?.len();
            let recovered = super::recovery::recover(file, length, context, workspace, cancel, metrics)?;
            Ok((recovered, length))
        });
        let (parsed, bytes) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                if matches!(error.kind, ErrorKind::InvalidInput | ErrorKind::Unsupported) {
                    current.failed.store(true, Ordering::Release);
                }
                return Err(error);
            }
        };
        let mut state = self.state()?;
        if parsed.recovered.damage.is_some() {
            current.failed.store(true, Ordering::Release);
        } else if !state.admitted {
            current.failed.store(false, Ordering::Release);
        }
        state.bytes = bytes;
        state.wire = parsed.wire;
        state.appended = parsed.recovered.applied;
        state.durable = 0;
        state.admitted = true;
        Ok(parsed.recovered)
    }
    pub(super) fn discard(&self, path: &Path) -> Result<(), Error> {
        self.discard_with(path, |path| std::fs::remove_file(path))
    }
    fn discard_with(&self, path: &Path, remove: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), Error> {
        if path != self.recovery {
            return Err(io_error("discard location belongs to another recovery owner"));
        }
        let _operation = self.operation()?;
        self.flush()?;
        self.check_association()?;
        let ordinal = self.state()?.generation.checked_add(1).ok_or_else(|| io_error("recovery generation limit reached"))?;
        match remove(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(io_error("cannot discard recovery file; retained bytes remain owned")),
        }
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let old = state.current.take();
        state.bytes = 0;
        state.generation = ordinal;
        state.appended = 0;
        state.durable = 0;
        state.phase = Phase::Discarded;
        drop(state);
        if let Some(old) = old {
            old.retired.store(true, Ordering::Release);
        }
        Ok(())
    }
}
struct AppendWriter<'a> {
    file: &'a mut File,
    left: u64,
}
impl Write for AppendWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.left {
            return Err(std::io::Error::other("recovery output exceeds its admitted extent"));
        }
        let n = self.file.write(bytes)?;
        self.left = self.left.checked_sub(n as u64).ok_or_else(|| std::io::Error::other("recovery extent overflow"))?;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
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
        workspace: crate::NativeAssetWorkspace,
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
            let lock_path = dir.path().join("recovery.lock");
            let lock_file = lock_file(&lock_path, "test lock").unwrap();
            let workspace = crate::NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
            let journal = Arc::new(
                SharedJournal::with_syncer(
                    path.clone(),
                    lock_path,
                    lock_file,
                    workspace.quota().clone(),
                    Arc::new(ControlledSync { entered: entered_tx, release: Mutex::new(release_rx) }),
                )
                .unwrap(),
            );
            workspace.install_journal(&journal).unwrap();
            journal.checkpoint(&workspace, b"checkpoint").unwrap();
            Self { workspace, _dir: dir, path, journal, entered, release }
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
            f.journal.checkpoint(&f.workspace, b"new checkpoint").unwrap();
            f.journal.append(&f.path, b"new append").unwrap();
            f.release.send(success).unwrap();
            old.join().unwrap().unwrap();
            {
                let state = f.journal.lock().unwrap();
                assert_eq!((state.durable, state.appended), (0, 1));
                assert!(!state.current.as_ref().unwrap().failed.load(Ordering::Acquire));
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
        f.journal.checkpoint(&f.workspace, b"repair").unwrap();
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
        assert!(f.journal.checkpoint(&f.workspace, b"must not publish").is_err());
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
        let pending = f.journal.prepare(16, Wire::Legacy).unwrap();
        assert!(
            f.journal
                .publish(&f.workspace, &pending, |file| {
                    file.write_all(b"partial").unwrap();
                    Err(io_error("injected prepublication write failure"))
                })
                .is_err()
        );
        {
            let state = f.journal.lock().unwrap();
            assert_eq!((state.generation, state.appended, state.durable), before);
            assert!(!state.current.as_ref().unwrap().failed.load(Ordering::Acquire));
        }
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpointretained append");
        let sync = f.syncing();
        f.release.send(true).unwrap();
        sync.join().unwrap().unwrap();
        assert_eq!(f.journal.lock().unwrap().durable, 1);
    }

    #[test]
    fn publication_is_owner_bound_single_use_and_clean_failure_alone_is_retryable() {
        let f = Fixture::new();
        let other = Fixture::new();
        let plan = f.journal.prepare(3, Wire::Legacy).unwrap();
        let clone = plan.clone();
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 13);
        assert!(other.journal.publish(&other.workspace, &plan, |_| panic!("foreign plan must refuse before writing")).is_err());
        assert_eq!(plan.phase.load(Ordering::Acquire), PENDING);
        assert!(
            f.journal
                .publish(&f.workspace, &plan, |file| {
                    file.write_all(b"n").unwrap();
                    Err(io_error("injected preparation failure"))
                })
                .is_err()
        );
        assert_eq!(plan.phase.load(Ordering::Acquire), PENDING);
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpoint");
        assert_eq!(f.workspace.quota().usage().unwrap().staging, 0);
        f.journal.publish(&f.workspace, &clone, |file| file.write_all(b"new").map_err(|_| io_error("test write"))).unwrap();
        assert_eq!(plan.phase.load(Ordering::Acquire), PUBLISHED);
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 3);
        assert!(f.journal.publish(&f.workspace, &plan, |_| panic!("published claim cannot write twice")).is_err());
        assert_eq!(std::fs::read(&f.path).unwrap(), b"new");
    }
    #[test]
    fn concurrent_claim_refuses_without_waiting_or_publishing_a_second_generation() {
        let f = Fixture::new();
        let plan = f.journal.prepare(3, Wire::Legacy).unwrap();
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let owner = f.journal.clone();
        let workspace = f.workspace.clone();
        let first = plan.clone();
        let worker = thread::spawn(move || {
            owner.publish(&workspace, &first, |file| {
                entered_tx.send(()).map_err(|_| io_error("test receiver closed"))?;
                release_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| io_error("test release timed out"))?;
                file.write_all(b"new").map_err(|_| io_error("test write"))
            })
        });
        let entered_ok = entered.recv_timeout(Duration::from_secs(5));
        let second = f.journal.publish(&f.workspace, &plan, |_| panic!("second claim must not enter writer"));
        let before = std::fs::read(&f.path);
        let _ = release.send(());
        let result = worker.join();
        assert!(entered_ok.is_ok());
        assert!(second.is_err());
        assert_eq!(before.unwrap(), b"checkpoint");
        result.unwrap().unwrap();
        assert_eq!(plan.phase.load(Ordering::Acquire), PUBLISHED);
        assert_eq!(std::fs::read(&f.path).unwrap(), b"new");
    }
    #[test]
    fn failed_temp_delete_permanently_fails_claim_and_keeps_exact_debt_charge() {
        let f = Fixture::new();
        let plan = f.journal.prepare(3, Wire::Legacy).unwrap();
        f.workspace.fail_removals(1);
        assert!(
            f.journal
                .publish(&f.workspace, &plan, |file| {
                    file.write_all(b"n").unwrap();
                    Err(io_error("injected write failure"))
                })
                .is_err()
        );
        assert_eq!(plan.phase.load(Ordering::Acquire), FAILED);
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 13);
        assert_eq!(f.workspace.quota().cleanup_retained_bytes().unwrap(), 3);
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpoint");
        assert!(NativeStorage::new(f.path.clone()).is_err(), "the actual recovery lock remains held");
        f.workspace.cleanup().unwrap();
        assert_eq!(f.workspace.quota().usage().unwrap().staging, 0);
        assert!(f.journal.publish(&f.workspace, &plan, |_| panic!("failed cleanup claim must not revive")).is_err());
        drop(plan);
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 10);
    }
    #[test]
    fn partial_append_retains_high_water_and_requires_replacement_before_retry() {
        let f = Fixture::new();
        assert!(
            f.journal
                .append_with(5, None, |file| {
                    file.write_all(b"par").unwrap();
                    Err(io_error("injected partial append"))
                })
                .is_err()
        );
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpointpar");
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 15);
        assert_eq!(f.journal.lock().unwrap().appended, 0);
        assert!(f.journal.append(&f.path, b"no").is_err());
        f.journal.checkpoint(&f.workspace, b"repair").unwrap();
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 6);
        assert_eq!(std::fs::read(&f.path).unwrap(), b"repair");
    }
    #[test]
    fn retired_sync_pin_retains_old_bytes_and_backing_until_last_file_close() {
        let f = Fixture::new();
        f.journal.append(&f.path, b"old").unwrap();
        let old_bytes = f.workspace.quota().usage().unwrap().journals;
        let sync = f.syncing();
        let prepared = f.journal.prepare(3, Wire::Legacy).unwrap();
        f.journal.publish(&f.workspace, &prepared, |file| file.write_all(b"new").map_err(|_| io_error("test write"))).unwrap();
        let while_pinned = f.workspace.quota().usage().unwrap().journals;
        let while_backings = f.workspace.quota().backing_count().unwrap();
        f.release.send(false).unwrap();
        let result = sync.join();
        assert_eq!(while_pinned, old_bytes + 3);
        assert_eq!(while_backings, 2);
        result.unwrap().unwrap();
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 3);
        assert_eq!(f.workspace.quota().backing_count().unwrap(), 1);
        assert!(!f.journal.lock().unwrap().current.as_ref().unwrap().failed.load(Ordering::Acquire));
    }

    #[test]
    fn failed_discard_retains_published_generation_bytes_lock_and_reopen_owner() {
        let f = Fixture::new();
        let before = f.workspace.quota().usage().unwrap();
        assert!(f.journal.discard_with(&f.path, |_| Err(std::io::Error::other("injected discard failure"))).is_err());
        assert_eq!(f.workspace.quota().usage().unwrap(), before);
        assert_eq!(std::fs::read(&f.path).unwrap(), b"checkpoint");
        assert!(NativeStorage::new(f.path.clone()).is_err());
        assert!(!f.journal.close());
        let reopen = f.journal.begin_reopen().unwrap();
        assert_eq!(f.workspace.quota().usage().unwrap(), before);
        reopen.commit();
        f.journal.discard(&f.path).unwrap();
        assert!(!f.path.exists());
        assert_eq!(f.workspace.quota().usage().unwrap().journals, 0);
    }
}
