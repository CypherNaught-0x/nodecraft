//! Project files in the desktop app (#406): the session is built over native storage, so
//! `file.open`, `file.save` and `file.save_as` work from the UI, the control channel and MCP
//! alike, and every edit is journalled for crash recovery (#408). Dialogs are #407: a project
//! without a file is saved with an explicit `file.save_as` location.
use super::NodeCraftApp;
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, NativeStorage, Snapshot};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::path::PathBuf;

/// Where the app keeps its recovery journal, and the project to open at startup.
#[derive(Clone, Debug)]
pub struct StorageSetup {
    /// The recovery journal file (in the settings folder); its `.lock` sibling keeps a second
    /// window from writing the same journal.
    pub recovery: PathBuf,
    pub open: Option<PathBuf>,
}

/// Copies a non-empty journal at `recovery` to a fresh `<name>.unrecovered-<n>` beside it, and
/// returns where. Call with the journal's lock held.
fn keep_unrecovered(recovery: &std::path::Path) -> Result<Option<PathBuf>, Error> {
    let unreadable = |_| Error::new(ErrorKind::Conflict, "an earlier session's recovery journal can't be kept, so it wasn't replaced");
    match std::fs::metadata(recovery) {
        Ok(meta) if meta.len() > 0 => {}
        // Nothing to keep: no journal yet, or an empty one.
        _ => return Ok(None),
    }
    let name = recovery.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "session.journal".into());
    for n in 1..=999u32 {
        let kept = recovery.with_file_name(format!("{name}.unrecovered-{n}"));
        if !kept.exists() {
            std::fs::copy(recovery, &kept).map_err(unreadable)?;
            return Ok(Some(kept));
        }
    }
    Err(Error::new(ErrorKind::Conflict, "too many unrecovered journals are kept; recover or remove some first"))
}

impl NodeCraftApp {
    /// A session over native storage, starting from the sample project or the file in `setup`,
    /// and the canvas to show, plus where an earlier session's unrecovered journal was kept.
    pub(super) fn storage_session(setup: StorageSetup) -> Result<(Session, nodecraft_types::CanvasId, Option<PathBuf>), Error> {
        let kinds = nodecraft_nodes::Registry::builtins()?;
        let (document, canvas) = Self::starter_document(&kinds)?;
        if let Some(folder) = setup.recovery.parent() {
            std::fs::create_dir_all(folder).map_err(|_| Error::new(ErrorKind::Unsupported, "the recovery folder can't be created"))?;
        }
        // The lock comes first: once held, no other window is using this journal.
        let storage = NativeStorage::new(setup.recovery.clone())?;
        // A journal left by a session that didn't close cleanly holds unsaved work. Starting a
        // session checkpoints over it, so keep a copy first (recovery offers it, #408).
        let kept = keep_unrecovered(&setup.recovery)?;
        let snapshot = Snapshot { project: document, assets: AssetStore::default(), epoch: 0 };
        let mut session = Session::with_storage(snapshot, Box::new(storage))?;
        let mut canvas = canvas;
        if let Some(path) = setup.open {
            let location = path.to_str().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the project path isn't valid UTF-8"))?;
            session.dispatch("file.open", &json!({"location": location, "discard_dirty": true}))?;
            canvas = session.document().canvases().keys().next().copied().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the project has no canvas"))?;
        }
        Ok((session, canvas, kept))
    }

    /// [`Self::storage_session`] wrapped in an app with `services`.
    #[cfg(test)]
    pub(super) fn with_services_and_storage(services: nodecraft_engine::commands::Services, setup: StorageSetup) -> Result<Self, Error> {
        let (session, canvas, _) = Self::storage_session(setup)?;
        Self::from_session(session.with_services(services), canvas)
    }

    /// On a clean quit: when everything is saved, close the recovery journal so it isn't
    /// offered next time. Unsaved work keeps its journal for recovery (#408).
    pub(super) fn close_storage(&mut self) {
        let status = self.session.file_status();
        if status.available
            && !status.dirty
            && let Err(error) = self.session.close_recovery(false)
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
