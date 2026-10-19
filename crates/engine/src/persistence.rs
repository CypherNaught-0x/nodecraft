use crate::{file_cmds::FileStatus, save_runner::SaveRunner, session::guard};
use nodecraft_format::{
    ArchivePlan, AssetStore, CheckpointPlan, ProjectAccess, ProjectStorage, RecordedAssets, Snapshot, StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::collections::BTreeSet;
use std::sync::mpsc;

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
/// A background save's file work: the same steps, in the same order, as an inline save.
fn write(
    storage: &mut dyn ProjectStorage,
    snapshot: &Snapshot,
    capability: StorageCapability,
    location: &StorageLocation,
    metrics: &StreamMetrics,
) -> Result<Published, Error> {
    let cancel = TransferCancel::default();
    let archive = ArchivePlan::prepare(snapshot, capability, &cancel, metrics)?;
    let checkpoint = CheckpointPlan::new(archive)?;
    let adopted = CheckpointState::prepare(&checkpoint)?;
    let _ = guard(|| storage.flush());
    let access = guard(|| storage.save_plan(location, checkpoint.archive(), &cancel, metrics))?;
    // Save succeeded. A failed recovery replacement must not claim it was unpublished.
    let adopted = guard(|| storage.checkpoint_plan(&checkpoint, &cancel, metrics)).is_ok().then_some(adopted);
    Ok(Published { access, adopted })
}
struct Done {
    storage: Box<dyn ProjectStorage + Send>,
    outcome: Result<Published, Error>,
}
struct Pending {
    done: mpsc::Receiver<Done>,
    location: StorageLocation,
    epoch: u64,
    state: u64,
}
/// How [`Persistence::save`] went: finished (`true` when recovery is ready), or still writing.
pub(crate) enum Saving {
    Done(bool),
    Started,
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
            saving: self.pending.is_some(),
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
    ) -> Result<Saving, Error> {
        if self.pending.is_some() {
            return Err(Error::new(ErrorKind::Busy, "a save is already in progress"));
        }
        if self.read_only && location.as_ref().is_none_or(|location| Some(location) == self.location.as_ref()) {
            return Err(Error::new(ErrorKind::Conflict, "source is read-only; use Save As to a writable destination"));
        }
        let location = location.or_else(|| self.location.clone()).ok_or_else(|| Error::new(ErrorKind::Conflict, "save_as needs an explicit location"))?;
        let epoch = self.epoch.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "checkpoint epoch overflow"))?;
        // Spill history-only roots before replacing any base or output destination.
        assets.verify_retained(retained, &TransferCancel::default(), &self.metrics)?;
        let snapshot = Snapshot { project: document.clone(), assets: assets.clone(), epoch };
        // Browser asset sources aren't `Send`; the web host has no runner and saves inline.
        #[cfg(target_arch = "wasm32")]
        let _ = runner;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(runner) = runner {
            match std::mem::replace(&mut self.store, Store::Lent) {
                Store::Sendable(mut storage) => {
                    let (sender, done) = mpsc::sync_channel(1);
                    let (capability, metrics, target) = (self.capability, self.metrics.clone(), location.clone());
                    let job = crate::save_runner::SaveJob::new(move || {
                        let outcome = guard(|| write(storage.as_mut(), &snapshot, capability, &target, &metrics));
                        // A closed receiver means the session is gone; the storage drops here.
                        let _ = sender.send(Done { storage, outcome });
                    });
                    let refused = runner.spawn(job).err();
                    self.pending = Some(Pending { done, location, epoch, state });
                    let Some(job) = refused else { return Ok(Saving::Started) };
                    job.run();
                    return self.settle(true).unwrap_or_else(|| Err(Error::new(ErrorKind::Internal, "the save did not finish"))).map(Saving::Done);
                }
                other => self.store = other,
            }
        }
        let published = write(self.store.get()?, &snapshot, self.capability, &location, &self.metrics)?;
        Ok(Saving::Done(self.publish(location, epoch, state, published)))
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
    /// Finishes a background save: `None` while none is pending or (without `wait`) it is still
    /// writing, else its outcome (`true` when recovery is ready).
    pub fn settle(&mut self, wait: bool) -> Option<Result<bool, Error>> {
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
            return Some(Err(Error::new(ErrorKind::Internal, "the save stopped before it finished; restart NodeCraft to recover your work")));
        };
        self.store = Store::Sendable(done.storage);
        Some(done.outcome.map(|published| self.publish(pending.location, pending.epoch, pending.state, published)))
    }
    pub fn open(&mut self, location: StorageLocation, state: u64) -> Result<Snapshot, Error> {
        let cancel = TransferCancel::default();
        let prepared = (|| {
            let prepared = guard(|| self.store.get()?.prepare_open_snapshot(&location, &cancel, &self.metrics))?;
            let (snapshot, checkpoint, access) = prepared.into_parts();
            let adopted = CheckpointState::prepare(&checkpoint)?;
            guard(|| self.store.get()?.checkpoint_plan(&checkpoint, &cancel, &self.metrics))?;
            Ok::<_, Error>((snapshot, adopted, access))
        })();
        let (snapshot, adopted, access) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                guard(|| {
                    self.store.get()?.cancel_open();
                    Ok(())
                })?;
                return Err(error);
            }
        };
        // This hook is infallible by protocol; all next-state ownership is already ready.
        if let Err(error) = guard(|| {
            self.store.get()?.commit_open();
            Ok(())
        }) {
            // A host violating its infallible hook cannot authorize edits against the
            // old live document after its replacement checkpoint was published.
            self.recovery_required = true;
            return Err(error);
        }
        self.adopt(adopted);
        self.location = Some(location);
        self.read_only = access == ProjectAccess::ReadOnly;
        self.saved_state = Some(state);
        self.epoch = snapshot.epoch;
        Ok(snapshot)
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
