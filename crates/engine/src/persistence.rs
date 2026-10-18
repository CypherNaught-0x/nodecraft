use crate::{file_cmds::FileStatus, session::guard};
use nodecraft_format::{AssetStore, ProjectAccess, ProjectStorage, Snapshot, StorageLocation, journal};
use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::collections::BTreeSet;

pub(crate) struct Persistence {
    storage: Box<dyn ProjectStorage>,
    location: Option<StorageLocation>,
    saved_state: Option<u64>,
    epoch: u64,
    checkpoint: Blake3,
    assets: BTreeSet<Blake3>,
    sequence: u64,
    journal_bytes: usize,
    recovery_required: bool,
    closed: bool,
    read_only: bool,
}

impl Persistence {
    pub fn new(storage: Box<dyn ProjectStorage>, snapshot: &Snapshot) -> Result<Self, Error> {
        let checkpoint = journal::checkpoint(snapshot)?;
        let mut state = Self {
            storage,
            location: None,
            saved_state: None,
            epoch: snapshot.epoch,
            checkpoint: checkpoint.identity,
            assets: checkpoint.assets.clone(),
            sequence: 0,
            journal_bytes: 0,
            recovery_required: true,
            closed: false,
            read_only: false,
        };
        guard(|| state.storage.checkpoint(&checkpoint.bytes))?;
        state.adopt(checkpoint);
        Ok(state)
    }
    fn adopt(&mut self, checkpoint: journal::Checkpoint) {
        self.checkpoint = checkpoint.identity;
        self.assets = checkpoint.assets;
        self.sequence = 0;
        self.journal_bytes = checkpoint.bytes.len();
        self.recovery_required = false;
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
        let (frame, additional) = admission.into_replay().into_frame(self.checkpoint, sequence, &self.assets)?;
        let bytes = self.journal_bytes.saturating_add(frame.len());
        if bytes > journal::MAX_JOURNAL_BYTES {
            return Err(Error::new(ErrorKind::Conflict, "recovery journal is full; save before editing"));
        }
        // An append failure is ambiguous: preserve the live document and latch before the call.
        self.recovery_required = true;
        guard(|| self.storage.append(&frame))?;
        self.recovery_required = false;
        self.assets.extend(additional);
        self.sequence = sequence;
        self.journal_bytes = bytes;
        Ok(())
    }
    pub fn save(&mut self, document: &Project, assets: &AssetStore, state: u64, location: Option<StorageLocation>) -> Result<bool, Error> {
        if self.read_only && location.as_ref().is_none_or(|location| Some(location) == self.location.as_ref()) {
            return Err(Error::new(ErrorKind::Conflict, "source is read-only; use Save As to a writable destination"));
        }
        let location = location.or_else(|| self.location.clone()).ok_or_else(|| Error::new(ErrorKind::Conflict, "save_as needs an explicit location"))?;
        let epoch = self.epoch.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "checkpoint epoch overflow"))?;
        let snapshot = Snapshot { project: document.clone(), assets: assets.clone(), epoch };
        let checkpoint = journal::checkpoint(&snapshot)?;
        // Saving the complete live snapshot can repair an ambiguous/failed recovery stream.
        // Attempt its durability boundary, then replace it with the new durable checkpoint.
        let _ = guard(|| self.storage.flush());
        let access = guard(|| self.storage.save_with_access(&location, checkpoint.archive()?))?;
        // The destination is published. A subsequent recovery failure must not turn a successful
        // save into a misleading error or leave the location/savepoint pointing at the old file.
        self.location = Some(location);
        self.read_only = access == ProjectAccess::ReadOnly;
        self.saved_state = Some(state);
        self.epoch = epoch;
        self.recovery_required = true;
        if guard(|| self.storage.checkpoint(&checkpoint.bytes)).is_ok() {
            self.adopt(checkpoint);
        }
        Ok(!self.recovery_required)
    }
    pub fn open(&mut self, location: StorageLocation, state: u64) -> Result<Snapshot, Error> {
        let prepared = (|| {
            let loaded = guard(|| self.storage.prepare_open(&location))?;
            let (snapshot, checkpoint) = journal::checkpoint_from_archive(loaded.bytes)?;
            guard(|| self.storage.checkpoint(&checkpoint.bytes))?;
            Ok::<_, Error>((snapshot, checkpoint, loaded.access))
        })();
        let (snapshot, checkpoint, access) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                guard(|| {
                    self.storage.cancel_open();
                    Ok(())
                })?;
                return Err(error);
            }
        };
        guard(|| {
            self.storage.commit_open();
            Ok(())
        })?;
        self.adopt(checkpoint);
        self.location = Some(location);
        self.read_only = access == ProjectAccess::ReadOnly;
        self.saved_state = Some(state);
        self.epoch = snapshot.epoch;
        Ok(snapshot)
    }
    /// Adopts `snapshot` (work recovered from an earlier session's journal) as the session's new
    /// checkpoint, at `location` (the file the earlier session had, if any) and unsaved.
    pub fn adopt_recovered(&mut self, snapshot: &Snapshot, location: Option<StorageLocation>) -> Result<(), Error> {
        let checkpoint = journal::checkpoint(snapshot)?;
        guard(|| self.storage.checkpoint(&checkpoint.bytes))?;
        self.adopt(checkpoint);
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
        guard(|| self.storage.flush())?;
        guard(|| self.storage.discard())?;
        self.closed = true;
        self.recovery_required = true;
        Ok(())
    }
}
