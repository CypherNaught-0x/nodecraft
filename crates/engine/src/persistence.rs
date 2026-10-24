use crate::{
    file_cmds::{FileOperation, FileOperationKind, FilePhase, FileStatus},
    save_runner::SaveRunner,
    session::guard,
};
use nodecraft_format::{
    ArchivePlan, AssetStore, CheckpointPlan, ProjectAccess, ProjectStorage, RecordedAssets, Snapshot, StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
    mpsc,
};

struct CheckpointState {
    identity: Blake3,
    assets: RecordedAssets,
    bytes: u64,
}
impl CheckpointState {
    fn prepare(plan: &CheckpointPlan) -> Result<Self, Error> {
        Ok(Self { identity: plan.identity(), assets: RecordedAssets::new(plan.asset_lengths())?, bytes: plan.byte_len() })
    }
}
/// The storage, or where it went: a `Send` storage can be lent to a background save.
pub(crate) enum Store {
    Local(Box<dyn ProjectStorage>),
    Sendable(Box<dyn ProjectStorage + Send>),
    /// Lent to a save that hasn't handed it back. Settled before any other storage call, so it
    /// stays only if the save's worker dropped it without running.
    Lent,
}
impl Store {
    fn get(&mut self) -> Result<&mut dyn ProjectStorage, Error> {
        match self {
            Self::Local(storage) => Ok(storage.as_mut()),
            Self::Sendable(storage) => Ok(storage.as_mut()),
            Self::Lent => Err(Error::new(ErrorKind::Internal, "project storage was lost when a save stopped; restart NodeCraft to recover your work")),
        }
    }
}
/// What a save published: the destination's access, and the new checkpoint unless writing it
/// failed after the destination was already replaced.
struct Published {
    access: ProjectAccess,
    adopted: Option<CheckpointState>,
}
/// Which [`FilePhase`] a file operation is in, shared with the thread doing it.
#[derive(Clone)]
struct Progress(Arc<AtomicU8>);
impl Progress {
    fn new(phase: FilePhase) -> Self {
        Self(Arc::new(AtomicU8::new(phase as u8)))
    }
    fn set(&self, phase: FilePhase) {
        self.0.store(phase as u8, Ordering::Release);
    }
    fn get(&self) -> FilePhase {
        [FilePhase::Preparing, FilePhase::Reading, FilePhase::Writing, FilePhase::Checkpointing]
            .into_iter()
            .find(|phase| *phase as u8 == self.0.load(Ordering::Acquire))
            .unwrap_or(FilePhase::Checkpointing)
    }
}
/// What a file operation's thread reports to; inline operations get a fresh one.
#[derive(Clone)]
struct Control {
    cancel: TransferCancel,
    progress: Progress,
}
impl Control {
    fn new(phase: FilePhase) -> Self {
        Self { cancel: TransferCancel::default(), progress: Progress::new(phase) }
    }
    fn cancelled(&self) -> bool {
        self.cancel.check().is_err()
    }
}
/// A save's file work, inline or on the runner. Cancelling stops it only until the
/// destination is published: the checkpoint after that gets a token of its own.
fn write(
    storage: &mut dyn ProjectStorage,
    snapshot: &Snapshot,
    capability: StorageCapability,
    location: &StorageLocation,
    control: &Control,
    metrics: &StreamMetrics,
) -> Result<Published, Error> {
    let cancel = &control.cancel;
    let archive = ArchivePlan::prepare(snapshot, capability, cancel, metrics)?;
    let checkpoint = CheckpointPlan::new(archive)?;
    let adopted = CheckpointState::prepare(&checkpoint)?;
    control.progress.set(FilePhase::Writing);
    let _ = guard(|| storage.flush());
    cancel.check()?;
    let access = guard(|| storage.save_plan(location, checkpoint.archive(), cancel, metrics))?;
    control.progress.set(FilePhase::Checkpointing);
    // Save succeeded. A failed recovery replacement must not claim it was unpublished.
    let adopted = guard(|| storage.checkpoint_plan(&checkpoint, &TransferCancel::default(), metrics)).is_ok().then_some(adopted);
    Ok(Published { access, adopted })
}
/// What an open read, ready to adopt.
pub(crate) struct Opened {
    snapshot: Snapshot,
    adopted: CheckpointState,
    access: ProjectAccess,
}
/// An open that failed, and whether it failed after its replacement checkpoint was published
/// (a host breaking its infallible `commit_open`), so the old document can't journal on.
struct OpenFailure {
    error: Error,
    latch: bool,
}
/// An open's file work, inline or on the runner: freeze and decode the file, then publish its
/// recovery checkpoint, which is the point of no return. Cancelling stops it before that.
fn read(storage: &mut dyn ProjectStorage, location: &StorageLocation, control: &Control, metrics: &StreamMetrics) -> Result<Opened, OpenFailure> {
    let cancel = &control.cancel;
    let prepared = (|| {
        let prepared = guard(|| storage.prepare_open_snapshot(location, cancel, metrics))?;
        let (snapshot, checkpoint, access) = prepared.into_parts();
        let adopted = CheckpointState::prepare(&checkpoint)?;
        control.progress.set(FilePhase::Checkpointing);
        cancel.check()?;
        guard(|| storage.checkpoint_plan(&checkpoint, cancel, metrics))?;
        Ok::<_, Error>(Opened { snapshot, adopted, access })
    })();
    let opened = match prepared {
        Ok(opened) => opened,
        Err(error) => {
            guard(|| {
                storage.cancel_open();
                Ok(())
            })
            .map_err(|error| OpenFailure { error, latch: false })?;
            return Err(OpenFailure { error, latch: false });
        }
    };
    // This hook is infallible by protocol; all next-state ownership is already ready.
    guard(|| {
        storage.commit_open();
        Ok(())
    })
    .map_err(|error| OpenFailure { error, latch: true })?;
    Ok(opened)
}
enum Outcome {
    Saved { location: StorageLocation, epoch: u64, state: u64, result: Result<Published, Error> },
    Opened { location: StorageLocation, state: u64, result: Result<Opened, OpenFailure> },
}
/// A file operation's work: `Send` where it can run on the runner, and on the web (whose asset
/// sources aren't `Send`) anything, since it always runs inline there.
#[cfg(not(target_arch = "wasm32"))]
trait Work: FnOnce(&mut dyn ProjectStorage) -> Outcome + Send + 'static {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: FnOnce(&mut dyn ProjectStorage) -> Outcome + Send + 'static> Work for T {}
#[cfg(target_arch = "wasm32")]
trait Work: FnOnce(&mut dyn ProjectStorage) -> Outcome {}
#[cfg(target_arch = "wasm32")]
impl<T: FnOnce(&mut dyn ProjectStorage) -> Outcome> Work for T {}
struct Done {
    storage: Box<dyn ProjectStorage + Send>,
    outcome: Outcome,
}
struct Pending {
    done: mpsc::Receiver<Done>,
    kind: FileOperationKind,
    control: Control,
}
/// How [`Persistence::save`] or [`Persistence::open`] went: finished, or still running on the
/// runner, for [`Persistence::settle`] to finish.
pub(crate) enum Started<T> {
    Done(T),
    Started,
}
/// What [`Persistence::settle`] finished: a save (`true` when recovery is ready) or an open.
pub(crate) enum Settled {
    Saved(bool),
    Opened(Snapshot),
}
pub(crate) struct Persistence {
    store: Store,
    pending: Option<Pending>,
    capability: StorageCapability,
    location: Option<StorageLocation>,
    saved_state: Option<u64>,
    epoch: u64,
    checkpoint: Blake3,
    assets: RecordedAssets,
    sequence: u64,
    journal_bytes: u64,
    checkpoint_bytes: u64,
    recovery_required: bool,
    closed: bool,
    read_only: bool,
    metrics: StreamMetrics,
}
impl Persistence {
    pub fn new(mut store: Store, mut snapshot: Snapshot) -> Result<(Self, Snapshot), Error> {
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
        let storage = store.get()?;
        let capability = guard(|| Ok(storage.capability()))?;
        if capability == StorageCapability::StreamingV2 {
            let context = guard(|| Ok(storage.asset_context()))?
                .ok_or_else(|| Error::new(ErrorKind::Unsupported, "streaming storage requires its authentic asset context"))?;
            snapshot.assets = snapshot.assets.in_context(context, &cancel, &metrics)?;
        }
        let archive = ArchivePlan::prepare(&snapshot, capability, &cancel, &metrics)?;
        let checkpoint = CheckpointPlan::new(archive)?;
        let adopted = CheckpointState::prepare(&checkpoint)?;
        let mut state = Self {
            store,
            pending: None,
            capability,
            location: None,
            saved_state: None,
            epoch: snapshot.epoch,
            checkpoint: adopted.identity,
            assets: RecordedAssets::default(),
            sequence: 0,
            journal_bytes: 0,
            checkpoint_bytes: 0,
            recovery_required: true,
            closed: false,
            read_only: false,
            metrics,
        };
        guard(|| state.store.get()?.checkpoint_plan(&checkpoint, &cancel, &state.metrics))?;
        state.adopt(adopted);
        Ok((state, snapshot))
    }
    fn adopt(&mut self, checkpoint: CheckpointState) {
        self.checkpoint = checkpoint.identity;
        self.assets = checkpoint.assets;
        self.sequence = 0;
        self.journal_bytes = checkpoint.bytes;
        self.checkpoint_bytes = checkpoint.bytes;
        self.recovery_required = false;
    }
    pub(crate) fn capability(&self) -> StorageCapability {
        self.capability
    }
    pub(crate) fn metrics(&self) -> StreamMetrics {
        self.metrics.clone()
    }
    pub fn status(&self, state: u64) -> FileStatus {
        FileStatus {
            available: !self.closed,
            location: self.location.clone(),
            dirty: self.saved_state != Some(state),
            recovery_required: self.recovery_required,
            read_only: self.read_only,
            saving: self.pending.as_ref().is_some_and(|pending| pending.kind == FileOperationKind::Save),
            operation: self.pending.as_ref().map(|pending| FileOperation {
                kind: pending.kind,
                phase: pending.control.progress.get(),
                cancel_requested: pending.control.cancelled(),
            }),
        }
    }
    pub fn append(&mut self, admission: crate::session::PersistentAdmission) -> Result<(), Error> {
        if self.closed {
            return Err(Error::new(ErrorKind::Conflict, "session recovery was closed"));
        }
        if self.recovery_required {
            return Err(Error::new(ErrorKind::Conflict, "recovery journal needs a checkpoint; save before editing"));
        }
        let sequence = self.sequence.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "recovery sequence overflow"))?;
        let cancel = TransferCancel::default();
        let (frame, next) = admission.into_replay().into_plan(self.checkpoint, sequence, &self.assets, self.capability, &cancel, &self.metrics)?;
        let limit = match self.capability {
            StorageCapability::SmallV1 => nodecraft_format::journal::MAX_JOURNAL_BYTES as u64,
            StorageCapability::StreamingV2 => nodecraft_format::MAX_STREAM_JOURNAL_BYTES,
        };
        let bytes = self
            .journal_bytes
            .checked_add(frame.byte_len())
            .filter(|n| *n <= limit)
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "recovery journal is full; save before editing"))?;
        if self.capability == StorageCapability::StreamingV2
            && bytes.checked_sub(self.checkpoint_bytes).is_none_or(|tail| tail > nodecraft_format::MAX_STREAM_TAIL_BYTES)
        {
            return Err(Error::new(ErrorKind::Conflict, "recovery tail is full; save before editing"));
        }
        // Planning is reversible; the host call can partially write and must latch first.
        self.recovery_required = true;
        guard(|| self.store.get()?.append_plan(&frame, &cancel, &self.metrics))?;
        self.assets = next;
        self.sequence = sequence;
        self.journal_bytes = bytes;
        self.recovery_required = false;
        Ok(())
    }
    /// Saves at `state`. With a `runner` and a `Send` storage, the file work runs there and this
    /// returns [`Saving::Started`]; [`Self::settle`] finishes it. Otherwise it runs inline.
    pub fn save(
        &mut self,
        document: &Project,
        assets: &AssetStore,
        retained: &BTreeSet<Blake3>,
        state: u64,
        location: Option<StorageLocation>,
        runner: Option<&dyn SaveRunner>,
    ) -> Result<Started<bool>, Error> {
        self.idle()?;
        if self.read_only && location.as_ref().is_none_or(|location| Some(location) == self.location.as_ref()) {
            return Err(Error::new(ErrorKind::Conflict, "source is read-only; use Save As to a writable destination"));
        }
        let location = location.or_else(|| self.location.clone()).ok_or_else(|| Error::new(ErrorKind::Conflict, "save_as needs an explicit location"))?;
        let epoch = self.epoch.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "checkpoint epoch overflow"))?;
        // Spill history-only roots before replacing any base or output destination.
        assets.verify_retained(retained, &TransferCancel::default(), &self.metrics)?;
        let snapshot = Snapshot { project: document.clone(), assets: assets.clone(), epoch };
        let control = Control::new(FilePhase::Preparing);
        let (capability, metrics) = (self.capability, self.metrics.clone());
        let work = {
            let control = control.clone();
            move |storage: &mut dyn ProjectStorage| {
                let result = guard(|| write(storage, &snapshot, capability, &location, &control, &metrics));
                Outcome::Saved { location, epoch, state, result }
            }
        };
        match self.run(FileOperationKind::Save, control, runner, work)? {
            Started::Started => Ok(Started::Started),
            Started::Done(Settled::Saved(ready)) => Ok(Started::Done(ready)),
            Started::Done(Settled::Opened(_)) => Err(Error::new(ErrorKind::Internal, "a save finished as an open")),
        }
    }
    fn idle(&self) -> Result<(), Error> {
        match self.pending.as_ref().map(|pending| pending.kind) {
            None => Ok(()),
            Some(FileOperationKind::Save) => Err(Error::new(ErrorKind::Busy, "a save is already in progress")),
            Some(FileOperationKind::Open) => Err(Error::new(ErrorKind::Busy, "a project is still opening")),
        }
    }
    /// Runs `work` on `runner` when there is one and the storage is `Send`, else inline. Either
    /// way its outcome goes through [`Self::finish`], so both paths publish the same state.
    fn run(&mut self, kind: FileOperationKind, control: Control, runner: Option<&dyn SaveRunner>, work: impl Work) -> Result<Started<Settled>, Error> {
        // Browser asset sources aren't `Send`; the web host has no runner and works inline.
        #[cfg(target_arch = "wasm32")]
        let _ = runner;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(runner) = runner {
            match std::mem::replace(&mut self.store, Store::Lent) {
                Store::Sendable(mut storage) => {
                    let (sender, done) = mpsc::sync_channel(1);
                    let job = crate::save_runner::SaveJob::new(move || {
                        let outcome = work(storage.as_mut());
                        // A closed receiver means the session is gone; the storage drops here.
                        let _ = sender.send(Done { storage, outcome });
                    });
                    let refused = runner.spawn(job).err();
                    self.pending = Some(Pending { done, kind, control });
                    let Some(job) = refused else { return Ok(Started::Started) };
                    job.run();
                    return self.settle(true).unwrap_or_else(|| Err(Error::new(ErrorKind::Internal, "the file operation did not finish"))).map(Started::Done);
                }
                other => self.store = other,
            }
        }
        let outcome = work(self.store.get()?);
        self.finish(outcome, &control).map(Started::Done)
    }
    /// Asks the pending operation to stop; `false` when there is none.
    pub fn cancel(&self) -> bool {
        self.pending.as_ref().map(|pending| pending.control.cancel.cancel()).is_some()
    }
    fn publish(&mut self, location: StorageLocation, epoch: u64, state: u64, published: Published) -> bool {
        self.location = Some(location);
        self.read_only = published.access == ProjectAccess::ReadOnly;
        self.saved_state = Some(state);
        self.epoch = epoch;
        self.recovery_required = true;
        if let Some(adopted) = published.adopted {
            self.adopt(adopted);
        }
        !self.recovery_required
    }
    /// Finishes a background save or open: `None` while none is pending or (without `wait`) it
    /// is still running, else its outcome.
    pub fn settle(&mut self, wait: bool) -> Option<Result<Settled, Error>> {
        let pending = self.pending.as_ref()?;
        let done = if wait {
            pending.done.recv().ok()
        } else {
            match pending.done.try_recv() {
                Ok(done) => Some(done),
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let pending = self.pending.take()?;
        let Some(done) = done else {
            // The runner dropped the job unrun, and the storage with it: nothing more can be
            // journalled. The journal on disk still holds every edit up to this save.
            self.recovery_required = true;
            let what = match pending.kind {
                FileOperationKind::Save => "the save",
                FileOperationKind::Open => "opening the project",
            };
            return Some(Err(Error::new(ErrorKind::Internal, format!("{what} stopped before it finished; restart NodeCraft to recover your work"))));
        };
        self.store = Store::Sendable(done.storage);
        Some(self.finish(done.outcome, &pending.control))
    }
    /// Publishes a finished operation. One stopped by `file.cancel` reports the cancellation:
    /// nothing was published, so what was there before still is. Any other failure keeps its
    /// own error, even when a cancel was asked for after it.
    fn finish(&mut self, outcome: Outcome, control: &Control) -> Result<Settled, Error> {
        let cancelled = |error: &Error| control.cancelled() && TransferCancel::is_cancellation(error);
        match outcome {
            Outcome::Saved { location, epoch, state, result } => match result {
                Ok(published) => Ok(Settled::Saved(self.publish(location, epoch, state, published))),
                Err(error) if cancelled(&error) => Err(Error::new(ErrorKind::Conflict, "the save was cancelled; the file is unchanged")),
                Err(error) => Err(error),
            },
            Outcome::Opened { location, state, result } => match result {
                Ok(opened) => Ok(Settled::Opened(self.opened(location, state, opened))),
                Err(failure) => {
                    if failure.latch {
                        // The replacement checkpoint is published: the old live document can
                        // no longer journal against it.
                        self.recovery_required = true;
                        return Err(failure.error);
                    }
                    if cancelled(&failure.error) {
                        return Err(Error::new(ErrorKind::Conflict, "opening was cancelled; the current project is unchanged"));
                    }
                    Err(failure.error)
                }
            },
        }
    }
    fn opened(&mut self, location: StorageLocation, state: u64, opened: Opened) -> Snapshot {
        self.adopt(opened.adopted);
        self.location = Some(location);
        self.read_only = opened.access == ProjectAccess::ReadOnly;
        self.saved_state = Some(state);
        self.epoch = opened.snapshot.epoch;
        opened.snapshot
    }
    /// Opens `location` as the document at `state`. With a `runner` and a `Send` storage, the
    /// file work runs there and this returns [`Started::Started`]; [`Self::settle`] finishes
    /// it. Otherwise it runs inline.
    pub fn open(&mut self, location: StorageLocation, state: u64, runner: Option<&dyn SaveRunner>) -> Result<Started<Snapshot>, Error> {
        self.idle()?;
        let control = Control::new(FilePhase::Reading);
        let metrics = self.metrics.clone();
        let work = {
            let control = control.clone();
            move |storage: &mut dyn ProjectStorage| {
                let result = read(storage, &location, &control, &metrics);
                Outcome::Opened { location, state, result }
            }
        };
        match self.run(FileOperationKind::Open, control, runner, work)? {
            Started::Started => Ok(Started::Started),
            Started::Done(Settled::Opened(snapshot)) => Ok(Started::Done(snapshot)),
            Started::Done(Settled::Saved(_)) => Err(Error::new(ErrorKind::Internal, "an open finished as a save")),
        }
    }
    /// Adopts `snapshot` (work recovered from an earlier session's journal) as the session's new
    /// checkpoint, at `location` (the file the earlier session had, if any) and unsaved.
    pub fn adopt_recovered(&mut self, snapshot: &Snapshot, location: Option<StorageLocation>) -> Result<(), Error> {
        let cancel = TransferCancel::default();
        let archive = ArchivePlan::prepare(snapshot, self.capability, &cancel, &self.metrics)?;
        let checkpoint = CheckpointPlan::new(archive)?;
        let adopted = CheckpointState::prepare(&checkpoint)?;
        guard(|| self.store.get()?.checkpoint_plan(&checkpoint, &cancel, &self.metrics))?;
        self.adopt(adopted);
        self.location = location;
        self.read_only = false;
        self.saved_state = None;
        self.epoch = snapshot.epoch;
        Ok(())
    }
    pub fn close(&mut self, state: u64, discard_dirty: bool) -> Result<(), Error> {
        if self.saved_state != Some(state) && !discard_dirty {
            return Err(Error::new(ErrorKind::Conflict, "save changes or explicitly discard them before closing recovery"));
        }
        self.recovery_required = true;
        guard(|| self.store.get()?.flush())?;
        guard(|| self.store.get()?.discard())?;
        self.closed = true;
        self.recovery_required = true;
        Ok(())
    }
}
