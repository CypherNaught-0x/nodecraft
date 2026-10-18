use crate::{file_cmds::FileStatus, session::guard};
use nodecraft_format::{
    ArchivePlan, AssetStore, CheckpointPlan, ProjectAccess, ProjectStorage, RecordedAssets, Snapshot, StorageCapability, StorageLocation,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::collections::BTreeSet;

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
pub(crate) struct Persistence {
    storage: Box<dyn ProjectStorage>,
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
    pub fn new(storage: Box<dyn ProjectStorage>, mut snapshot: Snapshot) -> Result<(Self, Snapshot), Error> {
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
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
            storage,
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
        guard(|| state.storage.checkpoint_plan(&checkpoint, &cancel, &state.metrics))?;
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
        guard(|| self.storage.append_plan(&frame, &cancel, &self.metrics))?;
        self.assets = next;
        self.sequence = sequence;
        self.journal_bytes = bytes;
        self.recovery_required = false;
        Ok(())
    }
    pub fn save(
        &mut self,
        document: &Project,
        assets: &AssetStore,
        retained: &BTreeSet<Blake3>,
        state: u64,
        location: Option<StorageLocation>,
    ) -> Result<bool, Error> {
        if self.read_only && location.as_ref().is_none_or(|location| Some(location) == self.location.as_ref()) {
            return Err(Error::new(ErrorKind::Conflict, "source is read-only; use Save As to a writable destination"));
        }
        let location = location.or_else(|| self.location.clone()).ok_or_else(|| Error::new(ErrorKind::Conflict, "save_as needs an explicit location"))?;
        let epoch = self.epoch.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "checkpoint epoch overflow"))?;
        let cancel = TransferCancel::default();
        // Spill history-only roots before replacing any base or output destination.
        assets.verify_retained(retained, &cancel, &self.metrics)?;
        let snapshot = Snapshot { project: document.clone(), assets: assets.clone(), epoch };
        let archive = ArchivePlan::prepare(&snapshot, self.capability, &cancel, &self.metrics)?;
        let checkpoint = CheckpointPlan::new(archive)?;
        let adopted = CheckpointState::prepare(&checkpoint)?;
        let _ = guard(|| self.storage.flush());
        let access = guard(|| self.storage.save_plan(&location, checkpoint.archive(), &cancel, &self.metrics))?;
        // Save succeeded. A failed recovery replacement must not claim it was unpublished.
        self.location = Some(location);
        self.read_only = access == ProjectAccess::ReadOnly;
        self.saved_state = Some(state);
        self.epoch = epoch;
        self.recovery_required = true;
        if guard(|| self.storage.checkpoint_plan(&checkpoint, &cancel, &self.metrics)).is_ok() {
            self.adopt(adopted);
        }
        Ok(!self.recovery_required)
    }
    pub fn open(&mut self, location: StorageLocation, state: u64) -> Result<Snapshot, Error> {
        let cancel = TransferCancel::default();
        let prepared = (|| {
            let prepared = guard(|| self.storage.prepare_open_snapshot(&location, &cancel, &self.metrics))?;
            let (snapshot, checkpoint, access) = prepared.into_parts();
            let adopted = CheckpointState::prepare(&checkpoint)?;
            guard(|| self.storage.checkpoint_plan(&checkpoint, &cancel, &self.metrics))?;
            Ok::<_, Error>((snapshot, adopted, access))
        })();
        let (snapshot, adopted, access) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                guard(|| {
                    self.storage.cancel_open();
                    Ok(())
                })?;
                return Err(error);
            }
        };
        // This hook is infallible by protocol; all next-state ownership is already ready.
        if let Err(error) = guard(|| {
            self.storage.commit_open();
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
    pub fn close(&mut self, state: u64, discard_dirty: bool) -> Result<(), Error> {
        if self.saved_state != Some(state) && !discard_dirty {
            return Err(Error::new(ErrorKind::Conflict, "save changes or explicitly discard them before closing recovery"));
        }
        self.recovery_required = true;
        guard(|| self.storage.flush())?;
        guard(|| self.storage.discard())?;
        self.closed = true;
        self.recovery_required = true;
        Ok(())
    }
}
