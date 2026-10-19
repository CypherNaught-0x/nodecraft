//! Project files in the desktop app (#406): the session is built over native storage, so
//! `file.open`, `file.save` and `file.save_as` work from the UI, the control channel and MCP
//! alike, and every edit is journalled for crash recovery (#408). Dialogs are #407: a project
//! without a file is saved with an explicit `file.save_as` location.
use super::NodeCraftApp;
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, NativeStorage, Snapshot};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Where the app keeps its recovery journal, and the project to open at startup.
#[derive(Clone, Debug)]
pub struct StorageSetup {
    /// The recovery journal file (in the settings folder); its `.lock` sibling keeps a second
    /// window from writing the same journal.
    pub recovery: PathBuf,
    pub open: Option<PathBuf>,
}

/// Most kept journals; past this, older work is still kept but no new copy is made (and storage
/// keeps working).
pub const MAX_KEPT: u32 = 99;

/// Whether the journal at `recovery` holds work worth keeping: edits after its checkpoint, or
/// damage or an old format that recovery has to look at. A journal holding only the checkpoint
/// (the session started and did nothing) isn't.
fn holds_work(recovery: &std::path::Path, storage: &NativeStorage) -> bool {
    // The caller already owns this journal's lock. Native replay dispatches classic
    // and streamed envelopes without buffering the whole recovery file.
    holds_work_with_metadata(std::fs::metadata(recovery).map(|metadata| metadata.len()), || storage.recover())
}

fn holds_work_with_metadata(length: std::io::Result<u64>, replay: impl FnOnce() -> Result<nodecraft_format::journal::Recovered, Error>) -> bool {
    match length {
        Ok(0) => return false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        // A retained handle cannot make an inaccessible/replaced pathname empty.
        // Keep/refuse rather than silently checkpointing over uncertain work.
        Err(_) => return true,
        Ok(_) => {}
    }
    match replay() {
        Ok(recovered) => recovered.applied > 0 || recovered.damage.is_some() || recovered.truncated_tail || recovered.legacy,
        // Unreadable bytes are kept: only a person should decide they're worthless.
        Err(_) => true,
    }
}

/// Read a kept journal through the native, bounded replay path. The returned asset
/// store owns its private ranges/context, so offers remain readable after this host drops.
pub(super) fn replay_kept(path: &Path) -> Result<nodecraft_format::journal::Recovered, Error> {
    NativeStorage::new(path.to_path_buf())?.recover()
}

/// The file beside a journal (live or kept) naming the project file its session had open, so
/// recovered work goes back to that file (#408). Absent for an untitled project.
pub fn location_file(journal: &Path) -> PathBuf {
    let mut name = journal.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".location");
    journal.with_file_name(name)
}

/// The project file recorded beside `journal`, if any.
pub fn recorded_location(journal: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(location_file(journal)).ok()?;
    let text = text.trim_end_matches('\n');
    (!text.is_empty() && text.len() <= MAX_LOCATION_BYTES).then(|| PathBuf::from(text))
}

/// Records `location` beside `journal` (written aside, then renamed over, so it's never half
/// written); `None` removes the record.
pub fn record_location(journal: &Path, location: Option<&Path>) -> std::io::Result<()> {
    let file = location_file(journal);
    let Some(location) = location.and_then(Path::to_str) else {
        return match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    };
    let mut aside = file.clone().into_os_string();
    aside.push(".new");
    std::fs::write(&aside, location)?;
    std::fs::rename(&aside, &file)
}

/// Longest project path recorded beside a journal.
const MAX_LOCATION_BYTES: usize = 4096;

/// The journals kept beside `recovery`, oldest first.
pub fn kept_journals(recovery: &Path) -> Vec<PathBuf> {
    let Some(name) = recovery.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{name}.unrecovered-");
    let Ok(entries) = recovery.parent().map_or_else(|| std::fs::read_dir("."), std::fs::read_dir) else {
        return Vec::new();
    };
    let mut kept: Vec<(u32, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let n = entry.file_name().to_str()?.strip_prefix(&prefix)?.parse::<u32>().ok()?;
            Some((n, entry.path()))
        })
        .collect();
    kept.sort();
    kept.into_iter().map(|(_, path)| path).collect()
}

/// Copies a journal holding work at `recovery` to a fresh `<name>.unrecovered-<n>` beside it,
/// flushed to disk, and returns where. Call with the journal's lock held.
fn keep_unrecovered(recovery: &std::path::Path, storage: &NativeStorage) -> Result<Option<PathBuf>, Error> {
    let unreadable = |_| Error::new(ErrorKind::Conflict, "an earlier session's recovery journal can't be kept, so it wasn't replaced");
    if !holds_work(recovery, storage) {
        return Ok(None);
    }
    let name = recovery.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "session.journal".into());
    for n in 1..=MAX_KEPT {
        let kept = recovery.with_file_name(format!("{name}.unrecovered-{n}"));
        if !kept.exists() {
            std::fs::copy(recovery, &kept).map_err(unreadable)?;
            // On disk before the original is checkpointed over.
            // Windows flushes only files opened for writing.
            std::fs::OpenOptions::new().write(true).open(&kept).and_then(|file| file.sync_all()).map_err(unreadable)?;
            // The project file it belongs to goes with it; without it, the work is recovered
            // untitled, which loses nothing.
            if let Some(location) = recorded_location(recovery)
                && let Err(error) = record_location(&kept, Some(&location))
            {
                log::warn!("the kept journal's project file wasn't recorded: {error}");
            }
            return Ok(Some(kept));
        }
    }
    log::warn!("{MAX_KEPT} unrecovered journals are already kept; not keeping another");
    Ok(None)
}

impl NodeCraftApp {
    /// A session over native storage, starting from the sample project or the file in `setup`,
    /// and the canvas to show, plus where an earlier session's unrecovered journal was kept.
    pub(super) fn storage_session(setup: StorageSetup) -> Result<(Session, nodecraft_types::CanvasId, Option<PathBuf>, Option<Error>), Error> {
        let kinds = nodecraft_engine::generation_catalog::default_registry()?.0;
        let (document, canvas) = Self::starter_document(&kinds)?;
        if let Some(folder) = setup.recovery.parent() {
            std::fs::create_dir_all(folder).map_err(|_| Error::new(ErrorKind::Unsupported, "the recovery folder can't be created"))?;
        }
        // The lock comes first: once held, no other window is using this journal.
        let storage = NativeStorage::new(setup.recovery.clone())?;
        // A journal left by a session that didn't close cleanly holds unsaved work. Starting a
        // session checkpoints over it, so keep a copy first (recovery offers it, #408).
        let kept = keep_unrecovered(&setup.recovery, &storage)?;
        let snapshot = Snapshot { project: document, assets: AssetStore::default(), epoch: 0 };
        let mut session = Session::with_storage(snapshot, Box::new(storage))?;
        let mut canvas = canvas;
        let mut opened = Ok(());
        if let Some(path) = setup.open {
            // A project that won't open leaves the new one, still saveable, and says why.
            opened = path
                .to_str()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the project path isn't valid UTF-8"))
                .and_then(|location| session.dispatch("file.open", &json!({"location": location, "discard_dirty": true})).map(|_| ()));
            if let Some(first) = session.document().canvases().keys().next() {
                canvas = *first;
            }
        }
        Ok((session, canvas, kept, opened.err()))
    }

    /// [`Self::storage_session`] wrapped in an app with `services`.
    #[cfg(test)]
    pub(super) fn with_services_and_storage(services: nodecraft_engine::commands::Services, setup: StorageSetup) -> Result<Self, Error> {
        let (session, canvas, _, _) = Self::storage_session(setup)?;
        Self::from_session(session.with_services(services), canvas)
    }

    /// On a clean quit: when everything is saved, or nothing was done (an untitled project with
    /// no edits), close the recovery journal so it isn't offered next time. Unsaved work keeps
    /// its journal for recovery (#408).
    pub(super) fn close_storage(&mut self) {
        let status = self.session.file_status();
        let untouched = status.location.is_none() && !self.session.can_undo() && !self.session.can_redo();
        if status.available
            && (!status.dirty || untouched)
            && let Err(error) = self.session.close_recovery(untouched)
        {
            log::warn!("recovery journal not closed: {}", error.message);
        }
    }

    /// What ⌘S, ⇧⌘S or ⌘O did, as a notice (`None` when a save went through). Choosing a file
    /// needs the file dialogs (#407); until then a project gets its file from the command line
    /// or `file.save_as`.
    pub(super) fn file_shortcut(&mut self, save: bool, save_as: bool) -> Option<String> {
        let status = self.session.file_status();
        if !status.available {
            return Some("Saving isn't available in this window.".into());
        }
        if save && !save_as && status.location.is_some() {
            return self.session.dispatch("file.save", &json!({})).err().map(|error| error.message);
        }
        Some(if save || save_as {
            "This project has no file yet. Start NodeCraft with a project path to save to it.".into()
        } else {
            "To open a project, start NodeCraft with its path.".into()
        })
    }

    /// The window title: the project's file name (or "Untitled"), a dot while unsaved.
    pub(super) fn window_title(&self) -> String {
        let status = self.session.file_status();
        let name = match &status.location {
            Some(nodecraft_format::StorageLocation::Path(path)) => path.file_name().map_or_else(|| "Untitled".into(), |n| n.to_string_lossy().into_owned()),
            Some(_) | None => "Untitled".to_owned(),
        };
        let dirty = if status.available && status.dirty { " •" } else { "" };
        format!("{name}{dirty} — NodeCraft")
    }
}

#[cfg(test)]
mod tests;
