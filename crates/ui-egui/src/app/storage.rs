//! Project files in the desktop app (#406): the session is built over native storage, so
//! `file.open`, `file.save` and `file.save_as` work from the UI, the control channel and MCP
//! alike, and every edit is journalled for crash recovery (#408). Dialogs are #407: a project
//! without a file is saved with an explicit `file.save_as` location.
use super::NodeCraftApp;
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, NativeStorage, Snapshot};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// Writes saves on a thread of their own (#461), so a save's durability syncs never hold up
/// a frame.
pub(super) struct ThreadSaves;
impl nodecraft_engine::save_runner::SaveRunner for ThreadSaves {
    fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
        // The job stays reachable here, so a thread that can't start hands it back to run inline.
        let slot = Arc::new(Mutex::new(Some(job)));
        let worker = Arc::clone(&slot);
        let started = std::thread::Builder::new().name("nodecraft-save".into()).spawn(move || {
            let job = worker.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(job) = job {
                job.run();
            }
        });
        match started {
            Ok(_) => Ok(()),
            Err(_) => slot.lock().unwrap_or_else(PoisonError::into_inner).take().map_or(Ok(()), Err),
        }
    }
}

/// Where the app keeps its recovery journal, and the project to open at startup.
#[derive(Clone, Debug)]
pub struct StorageSetup {
    /// The recovery journal file (in the settings folder); its `.lock` sibling keeps a second
    /// window from writing the same journal.
    pub recovery: PathBuf,
    pub open: Option<PathBuf>,
}

/// Canonical safety-copy slots. Exhaustion refuses replacement of the live journal.
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
    let file = std::fs::File::open(location_file(journal)).ok()?;
    if file.metadata().ok()?.len() > MAX_LOCATION_FILE_BYTES as u64 {
        return None;
    }
    bounded_location(file)
}

fn bounded_location(reader: impl Read) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(MAX_LOCATION_FILE_BYTES + 1).ok()?;
    reader.take((MAX_LOCATION_FILE_BYTES + 1) as u64).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_LOCATION_FILE_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
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
    if location.len() > MAX_LOCATION_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "project location record exceeds its byte limit"));
    }
    let mut aside = file.clone().into_os_string();
    aside.push(".new");
    std::fs::write(&aside, location)?;
    std::fs::rename(&aside, &file)
}

/// Longest project path recorded beside a journal.
const MAX_LOCATION_BYTES: usize = 4096;
const MAX_LOCATION_FILE_BYTES: usize = MAX_LOCATION_BYTES + 1;

/// Canonical kept copies, ordered by slot number (not a proof of chronology).
/// Unknown names are untouched. At most MAX_KEPT paths and metadata probes are retained.
pub fn kept_journals(recovery: &Path) -> Vec<PathBuf> {
    let Some(name) = recovery.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    (1..=MAX_KEPT)
        .filter_map(|n| {
            let path = recovery.with_file_name(format!("{name}.unrecovered-{n}"));
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                // An uncertain or nonregular name is not free or implicitly deletable.
                _ => Some(path),
            }
        })
        .collect()
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
        let available = match std::fs::symlink_metadata(&kept) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(_) => false,
            Err(error) => return Err(unreadable(error)),
        };
        if available {
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
    Err(Error::new(
        ErrorKind::Conflict,
        "all recovery safety-copy slots are occupied, so this session's journal was not replaced and earlier work is untouched; move or delete old session.journal.unrecovered-* copies in the recovery folder, then restart",
    ))
}

impl NodeCraftApp {
    /// Shared startup path, also exercised without keychain/network workers in UI tests.
    pub(super) fn from_storage_setup(
        services: nodecraft_engine::commands::Services,
        setup: Option<StorageSetup>,
        missing_reason: &str,
    ) -> Result<(Self, Option<PathBuf>, bool), Error> {
        match setup.map(Self::storage_session) {
            Some(Ok((session, canvas, kept, open_error))) => {
                let mut app = Self::from_session(session.with_services(services), canvas)?;
                if let Some(error) = open_error {
                    app.notice = Some(format!(
                        "That project couldn't be opened: {}. The new project can still be saved; use File > Open to choose another file.",
                        error.message
                    ));
                }
                Ok((app, kept, true))
            }
            failed => {
                let (reason, owner_busy) = match failed {
                    Some(Err(error)) => {
                        log::error!("project storage unavailable: {}", error.message);
                        let busy = error.kind == ErrorKind::Busy && error.message == "recovery location is already in use";
                        let action = if busy {
                            "Another NodeCraft window holds the recovery journal. Focus that window and use its File menu to open or save a project. This window's edits cannot be saved."
                        } else {
                            "Storage could not start. Check the recovery folder's access and free space, then close this window and reopen NodeCraft. This window's edits cannot be saved."
                        };
                        (format!("Project files are unavailable: {}. {action}", error.message), busy)
                    }
                    _ => (missing_reason.to_owned(), false),
                };
                let mut app = Self::with_services(services)?;
                app.notice = Some(app.remember_storage_problem(reason, owner_busy));
                Ok((app, None, false))
            }
        }
    }
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
        let mut session = Session::with_sendable_storage(snapshot, Box::new(storage))?;
        session.set_save_runner(Arc::new(ThreadSaves));
        let mut canvas = canvas;
        let mut opened = Ok(());
        if let Some(path) = setup.open {
            // A project that won't open leaves the new one, still saveable, and says why.
            opened = path
                .to_str()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the project path isn't valid UTF-8"))
                .and_then(|location| session.dispatch("file.open", &json!({"location": location, "discard_dirty": true})).map(|_| ()));
            if let Some(first) = super::sheet_view::home_canvas(session.document()) {
                canvas = first;
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
        // Quitting waits for a save still writing; until it publishes, the project is unsaved.
        if let Some(Err(error)) = self.session.finish_file() {
            log::warn!("project not saved: {}", error.message);
        }
        let status = self.session.file_status();
        let untouched = status.location.is_none() && !self.session.can_undo() && !self.session.can_redo();
        if status.available
            && (!status.dirty || untouched)
            && let Err(error) = self.session.close_recovery(untouched)
        {
            log::warn!("recovery journal not closed: {}", error.message);
        }
    }

    /// The window title: the project's file name (or "Untitled"), a dot while unsaved.
    pub(super) fn window_title(&self) -> String {
        let status = self.session.file_status();
        let name = match &status.location {
            Some(nodecraft_format::StorageLocation::Path(path)) => path.file_name().map_or_else(|| "Untitled".into(), |n| n.to_string_lossy().into_owned()),
            Some(_) | None => "Untitled".to_owned(),
        };
        let dirty = if status.available && status.dirty { " •" } else { "" };
        let busy = match status.operation.map(|operation| operation.kind) {
            Some(nodecraft_engine::file_cmds::FileOperationKind::Save) => " (saving…)",
            Some(nodecraft_engine::file_cmds::FileOperationKind::Open) => " (opening…)",
            None => "",
        };
        format!("{name}{dirty}{busy} — NodeCraft")
    }

    /// Picks up a background save's outcome (#461): a failure becomes the notice, and while
    /// one is writing the frames keep coming so its end is seen promptly.
    pub(super) fn poll_save(&mut self, ctx: &egui::Context) {
        match self.session.poll_file() {
            Some(result) => self.file_save_completed(ctx, result),
            None if self.session.file_status().operation.is_some() => ctx.request_repaint_after(std::time::Duration::from_millis(16)),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests;
