//! What the run host keeps across a restart (#508): the usage log, and the provider handle of
//! every paid job that was submitted and hasn't ended.
//!
//! The host saves the journal whenever either changes ([`crate::run_host::RunHost::open_journal`]),
//! so a crash loses at most what changed since the last pump. On the next launch the log comes
//! back with its open reservations, each submitted job is resumed (polled, never sent again) and
//! its charge settled when it ends, and an open reservation with no handle (its submit may or may
//! not have reached the provider) is charged as unresolved.
use crate::jobs::sent_key;
use nodecraft_exec::cost::UsageLog;
use nodecraft_exec::run::ResumeRecipe;
use nodecraft_nodes::GenerateOutput;
use nodecraft_providers::JobHandle;
use nodecraft_types::{CanvasId, Error, ErrorKind, Money, NodeId, ProjectId, ProviderTarget};
use serde::{Deserialize, Serialize};

/// The journal's format version.
pub const JOURNAL_VERSION: u32 = 1;
/// Largest journal file read back; a bigger one is refused rather than loaded, and never
/// written. Room for a usage log at its own bounds (listed text, pending and retired charges)
/// with every submitted job.
pub const MAX_JOURNAL_BYTES: usize = 256 * 1024 * 1024;
/// Most submitted jobs a journal holds; admission bounds what a host runs at once far below it.
pub const MAX_SUBMITTED: usize = 4096;
/// Launches a submitted job waits for its project to be open again before it is given up on and
/// its charge settled as unresolved.
pub const MAX_PARKED_LAUNCHES: u32 = 30;

/// See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub usage: UsageLog,
    #[serde(default)]
    pub submitted: Vec<Submitted>,
}

/// A paid job the provider accepted and that hasn't ended: enough to poll it again, settle its
/// charge, and record its result on its card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submitted {
    pub project: ProjectId,
    pub canvas: CanvasId,
    pub node: NodeId,
    pub target: ProviderTarget,
    /// The scheduler's idempotency key; the key sent is `sent_key(key, generation)`.
    pub key: String,
    pub generation: u32,
    pub handle: JobHandle,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Money>,
    /// Whether it is charged: a free job has no reservation to settle.
    #[serde(default)]
    pub priced: bool,
    /// Where its outputs go on the card.
    pub routes: Vec<GenerateOutput>,
    /// The generation to record; `None` for a job whose step needs other jobs too (fanned out or
    /// several jobs), which is followed to its end so its charge settles, but not recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ResumeRecipe>,
    /// Launches it has waited through without being followed (see [`MAX_PARKED_LAUNCHES`]).
    #[serde(default)]
    pub launches: u32,
}

impl Submitted {
    /// The key the provider got, which its charge is logged under.
    pub fn sent_key(&self) -> String {
        sent_key(&self.key, self.generation)
    }
}

impl Journal {
    /// Reads a saved journal, refusing one that is too big, of a newer version, or malformed.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, "the run journal is too large to read"));
        }
        let journal: Self = serde_json::from_slice(bytes).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the run journal is damaged: {e}")))?;
        if journal.version != JOURNAL_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, format!("the run journal has version {}, which this NodeCraft can't read", journal.version)));
        }
        if journal.submitted.len() > MAX_SUBMITTED {
            return Err(Error::new(ErrorKind::InvalidInput, "the run journal lists too many jobs"));
        }
        Ok(journal)
    }

    /// The journal as saved, refusing one [`Self::from_bytes`] would refuse.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        if self.submitted.len() > MAX_SUBMITTED {
            return Err(Error::new(ErrorKind::Busy, "too many jobs are waiting to be followed to save them all"));
        }
        let bytes = serde_json::to_vec(self).map_err(|e| Error::new(ErrorKind::Internal, format!("the run journal can't be written: {e}")))?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(Error::new(ErrorKind::Busy, "the run journal is too large to save"));
        }
        Ok(bytes)
    }
}

/// Where a host keeps its journal.
pub trait JournalStore: Send {
    /// The saved journal, or `None` when there is none yet.
    fn load(&self) -> Result<Option<Journal>, Error>;
    fn save(&self, journal: &Journal) -> Result<(), Error>;
    /// Moves a journal that couldn't be read out of the way, so the next save doesn't replace
    /// what someone may still recover from it.
    fn set_aside(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// A journal in memory: for tests, and hosts with nowhere to save (it doesn't survive the process).
#[derive(Clone, Default)]
pub struct MemoryJournal(std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>);

impl MemoryJournal {
    fn bytes(&self) -> std::sync::MutexGuard<'_, Option<Vec<u8>>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The journal as last saved.
    pub fn saved(&self) -> Option<Journal> {
        self.bytes().as_deref().and_then(|bytes| Journal::from_bytes(bytes).ok())
    }
}

impl JournalStore for MemoryJournal {
    fn load(&self) -> Result<Option<Journal>, Error> {
        self.bytes().as_deref().map(Journal::from_bytes).transpose()
    }
    fn save(&self, journal: &Journal) -> Result<(), Error> {
        *self.bytes() = Some(journal.to_bytes()?);
        Ok(())
    }
}

/// A journal in a file, replaced atomically on every save so a crash mid-write keeps the last one.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileJournal {
    path: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileJournal {
    /// A journal in `path`. One process at a time may use a path: the desktop keeps it beside
    /// its recovery journal, whose lock it holds.
    pub fn at(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl JournalStore for FileJournal {
    fn load(&self) -> Result<Option<Journal>, Error> {
        use std::io::Read;
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::new(ErrorKind::Internal, format!("the run journal can't be read: {e}"))),
        };
        let mut bytes = Vec::new();
        let cap = u64::try_from(MAX_JOURNAL_BYTES).unwrap_or(u64::MAX).saturating_add(1);
        file.take(cap).read_to_end(&mut bytes).map_err(|e| Error::new(ErrorKind::Internal, format!("the run journal can't be read: {e}")))?;
        Journal::from_bytes(&bytes).map(Some)
    }
    fn save(&self, journal: &Journal) -> Result<(), Error> {
        let bytes = journal.to_bytes()?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::new(ErrorKind::Internal, format!("the run journal's folder can't be made: {e}")))?;
        }
        craft_atomic::atomic_write(&self.path, &bytes).map_err(|e| Error::new(ErrorKind::Internal, format!("the run journal can't be saved: {e}")))
    }
    fn set_aside(&self) -> Result<(), Error> {
        let aside = self.path.with_extension("json.damaged");
        match std::fs::rename(&self.path, aside) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::new(ErrorKind::Internal, format!("the damaged run journal can't be moved aside: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_round_trips_and_hostile_files_are_refused() {
        let journal = Journal { version: JOURNAL_VERSION, ..Journal::default() };
        assert_eq!(Journal::from_bytes(&journal.to_bytes().unwrap()).unwrap(), journal);
        assert!(Journal::from_bytes(b"{").is_err());
        assert!(Journal::from_bytes(br#"{"version":2,"usage":{}}"#).is_err());
        assert!(Journal::from_bytes(br#"{"version":1,"usage":{},"extra":1}"#).is_err());
        assert!(Journal::from_bytes(&vec![b' '; MAX_JOURNAL_BYTES + 1]).is_err());
    }

    #[test]
    fn a_journal_that_couldnt_be_read_back_is_never_written() {
        let entry: Submitted = serde_json::from_value(serde_json::json!({
            "project": ProjectId::from_bytes([1; 16]), "canvas": CanvasId::from_bytes([2; 16]), "node": NodeId::from_bytes([3; 16]),
            "target": {"adapter": "mock"}, "key": "k", "generation": 0,
            "handle": {"remote_id": "r"}, "model": "m", "routes": []
        }))
        .unwrap();
        let journal = Journal { version: JOURNAL_VERSION, submitted: vec![entry; MAX_SUBMITTED + 1], ..Journal::default() };
        assert!(journal.to_bytes().is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_file_journal_is_none_until_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileJournal::at(dir.path().join("nested").join("runs.json"));
        assert_eq!(store.load().unwrap(), None);
        let journal = Journal { version: JOURNAL_VERSION, ..Journal::default() };
        store.save(&journal).unwrap();
        assert_eq!(store.load().unwrap(), Some(journal.clone()));
        std::fs::write(dir.path().join("nested").join("runs.json"), b"{").unwrap();
        assert!(store.load().is_err());
        store.set_aside().unwrap();
        assert_eq!(store.load().unwrap(), None, "a damaged journal is moved aside, not overwritten");
        assert!(dir.path().join("nested").join("runs.json.damaged").exists());
    }
}
