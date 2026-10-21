//! Opening a `.ncraft` project headlessly.
//!
//! The engine opens projects through storage with a recovery journal, as the app does. The CLI's
//! journal lives in a private folder of its own under the system temp folder, made fresh for each
//! command and removed when it ends, so the CLI never shares (or locks) the app's recovery files
//! and never writes next to the project.
use std::path::{Path, PathBuf};

use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, NativeStorage, Snapshot};
use nodecraft_graph::Project;
use nodecraft_types::{Error, ErrorKind, ProjectId};
use serde_json::json;

/// A private temp folder, removed (with everything in it) when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(new_id: &mut dyn FnMut() -> [u8; 16]) -> Result<Self, Error> {
        let suffix: String = new_id().iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("nodecraft-cli-{}-{suffix}", std::process::id()));
        // `create_dir`, not `create_dir_all`: a folder that is already there isn't ours.
        std::fs::create_dir(&dir).map_err(|e| Error::new(ErrorKind::Internal, format!("can't make a scratch folder: {e}")))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session on an opened project. The session goes before its scratch folder.
pub struct Opened {
    pub session: Session,
    _scratch: Scratch,
}

/// Opens the project at `path` read-only: nothing is saved back to it.
pub fn open(path: &Path, new_id: &mut dyn FnMut() -> [u8; 16]) -> Result<Opened, Error> {
    // Checked first, so a typo reads as one rather than as a storage error.
    if !path.is_file() {
        return Err(Error::new(ErrorKind::NotFound, format!("no project file at {}", path.display())));
    }
    let location = path.to_str().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the project path must be valid UTF-8"))?;
    let scratch = Scratch::new(new_id)?;
    let storage = NativeStorage::new(scratch.0.join("recovery"))?;
    let blank = Project::new(ProjectId::from_bytes(new_id()), "Untitled")?;
    let mut session = Session::with_sendable_storage(Snapshot { project: blank, assets: AssetStore::default(), epoch: 0 }, Box::new(storage))?;
    session.dispatch("file.open", &json!({"location": location, "discard_dirty": true}))?;
    Ok(Opened { session, _scratch: scratch })
}
