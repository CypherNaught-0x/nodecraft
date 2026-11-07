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

/// How [`NodeCraftApp::start_storage`] began.
enum Started {
    Ready(Session, nodecraft_types::CanvasId, Option<PathBuf>),
    KeepingAside(Box<super::keep_aside::KeepAside>, Session, nodecraft_types::CanvasId, Option<PathBuf>),
}

/// Canonical safety-copy slots. Exhaustion refuses replacement of the live journal.
pub const MAX_KEPT: u32 = 99;

/// Whether a journal of `length` holds work worth keeping: edits after its checkpoint, or
/// damage or an old format that recovery has to look at. A journal holding only the checkpoint
/// (the session started and did nothing) isn't. See [`super::keep_aside::live_work`].
#[cfg(test)]
fn holds_work_with_metadata(length: std::io::Result<u64>, replay: impl FnOnce() -> Result<nodecraft_format::journal::Recovered, Error>) -> bool {
    super::keep_aside::live_work(length, replay).is_some()
}

/// Read a kept journal through the native, bounded replay path. The returned asset
/// store owns its private ranges/context, so offers remain readable after this host drops.
pub(super) fn replay_kept(path: &Path) -> Result<nodecraft_format::journal::Recovered, Error> {
    replay_kept_with(path, &nodecraft_format::asset_source::TransferCancel::default())
}

/// [`replay_kept`], stopping at `cancel`.
pub(super) fn replay_kept_with(path: &Path, cancel: &nodecraft_format::asset_source::TransferCancel) -> Result<nodecraft_format::journal::Recovered, Error> {
    NativeStorage::new(path.to_path_buf())?.recover_with(cancel, &nodecraft_format::asset_source::StreamMetrics::default())
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

impl NodeCraftApp {
    /// Shared startup path, also exercised without keychain/network workers in UI tests.
    pub(super) fn from_storage_setup(
        services: nodecraft_engine::commands::Services,
        setup: Option<StorageSetup>,
        missing_reason: &str,
    ) -> Result<(Self, Option<PathBuf>, bool), Error> {
        Self::from_storage_setup_with(services, setup, missing_reason, &super::keep_aside::thread_spawn())
    }
    /// [`Self::from_storage_setup`], keeping an earlier session's journal aside on `spawn`
    /// (#1053). When there is something to keep aside or offer, the window starts without
    /// storage and the keep-aside runs behind its first frame; the session takes the journal
    /// over once it has settled ([`Self::keep_aside_frame`]), so `kept` is `None` and the window
    /// doesn't yet own storage.
    pub(super) fn from_storage_setup_with(
        services: nodecraft_engine::commands::Services,
        setup: Option<StorageSetup>,
        missing_reason: &str,
        spawn: &super::keep_aside::Spawn,
    ) -> Result<(Self, Option<PathBuf>, bool), Error> {
        match setup.map(|setup| Self::start_storage(setup, spawn)) {
            Some(Ok(Started::Ready(session, canvas, open))) => {
                let mut app = Self::from_session(session.with_services(services), canvas)?;
                // Opened in the background by the first frame, once the window can show it.
                app.files.startup = open;
                Ok((app, None, true))
            }
            Some(Ok(Started::KeepingAside(mut job, session, canvas, open))) => {
                let mut app = Self::from_session(session.with_services(services), canvas)?;
                job.after.open = open;
                app.keep_aside = Some(job);
                Ok((app, None, false))
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
    /// Storage over the journal in `setup`, its lock taken on the calling thread. With nothing
    /// to keep aside or offer, the session is built over it here; otherwise the keep-aside
    /// starts on `spawn` and the session starts without storage.
    fn start_storage(setup: StorageSetup, spawn: &super::keep_aside::Spawn) -> Result<Started, Error> {
        let kinds = nodecraft_engine::generation_catalog::default_registry()?.0;
        let (document, canvas) = Self::starter_document(&kinds)?;
        if let Some(folder) = setup.recovery.parent() {
            std::fs::create_dir_all(folder).map_err(|_| Error::new(ErrorKind::Unsupported, "the recovery folder can't be created"))?;
        }
        // The lock comes first: once held, no other window is using this journal.
        let storage = NativeStorage::new(setup.recovery.clone())?;
        if super::keep_aside::nothing_to_keep(&setup.recovery) {
            let snapshot = Snapshot { project: document, assets: AssetStore::default(), epoch: 0 };
            let mut session = Session::with_sendable_storage(snapshot, Box::new(storage))?;
            session.set_save_runner(Arc::new(ThreadSaves));
            return Ok(Started::Ready(session, canvas, setup.open));
        }
        // The keep-aside's thread checkpoints this same document once the journal is kept.
        let snapshot = Snapshot { project: document.clone(), assets: AssetStore::default(), epoch: 0 };
        let mut session = Session::new(document)?;
        session.set_save_runner(Arc::new(ThreadSaves));
        // Nothing may change the sample project meanwhile, from any surface (the control
        // channel, MCP, the agent): the startup open and the question come once it settles.
        session.hold_for_storage(true);
        let job = super::keep_aside::KeepAside::start(setup.recovery, storage, snapshot, spawn);
        Ok(Started::KeepingAside(Box::new(job), session, canvas, setup.open))
    }
    /// A session over native storage, starting from the sample project, and the canvas to show,
    /// plus where an earlier session's unrecovered journal was kept and the file in `setup` still
    /// to open. That open is not made here (#367): it runs in the background from the window's
    /// first frame ([`Self::begin_startup_open`]), so a large project shows its progress and
    /// can be cancelled instead of holding the window shut.
    ///
    /// Inline, for tests: the desktop keeps the journal aside off the UI thread
    /// ([`Self::from_storage_setup`]).
    #[cfg(test)]
    pub(super) fn storage_session(setup: StorageSetup) -> Result<(Session, nodecraft_types::CanvasId, Option<PathBuf>, Option<PathBuf>), Error> {
        let kinds = nodecraft_engine::generation_catalog::default_registry()?.0;
        let (document, canvas) = Self::starter_document(&kinds)?;
        if let Some(folder) = setup.recovery.parent() {
            std::fs::create_dir_all(folder).map_err(|_| Error::new(ErrorKind::Unsupported, "the recovery folder can't be created"))?;
        }
        // The lock comes first: once held, no other window is using this journal.
        let storage = NativeStorage::new(setup.recovery.clone())?;
        // A journal left by a session that didn't close cleanly holds unsaved work. Starting a
        // session checkpoints over it, so keep a copy first (recovery offers it, #408).
        let progress = super::keep_aside::Progress::default();
        let kept = super::keep_aside::keep(&setup.recovery, &storage, &nodecraft_format::asset_source::TransferCancel::default(), &progress, false)?.kept;
        let snapshot = Snapshot { project: document, assets: AssetStore::default(), epoch: 0 };
        // This first checkpoint is of the small sample project, so it stays inline.
        let mut session = Session::with_sendable_storage(snapshot, Box::new(storage))?;
        session.set_save_runner(Arc::new(ThreadSaves));
        Ok((session, canvas, kept, setup.open))
    }

    /// [`Self::storage_session`] wrapped in an app with `services`; the file in `setup` opens
    /// with the first frame.
    #[cfg(test)]
    pub(super) fn with_services_and_storage(services: nodecraft_engine::commands::Services, setup: StorageSetup) -> Result<Self, Error> {
        let (session, canvas, _, open) = Self::storage_session(setup)?;
        let mut app = Self::from_session(session.with_services(services), canvas)?;
        app.files.startup = open;
        Ok(app)
    }

    /// On a clean quit: when everything is saved, or nothing was done (an untitled project with
    /// no edits), close the recovery journal so it isn't offered next time. Unsaved work keeps
    /// its journal for recovery (#408).
    pub(super) fn close_storage(&mut self) {
        // A keep-aside still running is stopped first (#1053); the journal stays as it was.
        self.stop_keep_aside();
        // Quitting waits for a save still writing; until it publishes, the project is unsaved.
        // An open or a recovery is cancelled instead (#367): waiting would hold the closing
        // window for the whole read with no way to stop it, the open project stays as it was,
        // and a recovery's kept copy stays to be offered next time.
        if self.session.file_status().operation.is_some_and(|operation| operation.kind != nodecraft_engine::file_cmds::FileOperationKind::Save)
            && let Err(error) = self.session.dispatch("file.cancel", &json!({}))
        {
            log::warn!("the open could not be cancelled before quitting: {}", error.message);
        }
        let saving = self.session.file_status().saving;
        match self.session.finish_file() {
            Some(Err(error)) if saving => log::warn!("project not saved: {}", error.message),
            Some(Err(error)) => log::warn!("the file operation running at quit didn't finish: {}", error.message),
            _ => {}
        }
        // Run results that arrived while the save wrote waited for it (#367): take them in now,
        // before the journal's state is decided, so the recovery journal holds them. Not while
        // the startup question is undecided: its answer may still replace the document.
        if saving
            && !self.recovery_deciding()
            && let Some(runs) = &mut self.runs
        {
            // Records only: nothing starts or is sent at quit.
            runs.record_for_quit(&mut self.session);
            // Media a result brings are adopted off this thread; wait a moment for them, bounded.
            for _ in 0..50 {
                if !runs.is_preparing_results() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                runs.record_for_quit(&mut self.session);
            }
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
            Some(nodecraft_engine::file_cmds::FileOperationKind::Recover) => " (recovering…)",
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
