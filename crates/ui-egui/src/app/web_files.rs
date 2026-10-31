//! Project files in the browser (#118). The session's storage holds no files of its own: a
//! picked file's bytes are handed to it for one `file.open`, and a save's bytes are taken from it
//! and written by the browser afterwards, to the picked file (File System Access) or as a
//! download. Every change still goes through `file.open` / `file.save` / `file.save_as`.
//!
//! A location is `<n>/<file name>`, unique per pick; the [`Port`] keeps the browser's file
//! handles under ids, so nothing here holds a browser object. The session's recovery checkpoint
//! and journal go to the site's storage (see `web_journal`); leaving the page with unsaved work
//! still asks first, since recovery is a safety net, not a save.
use super::NodeCraftApp;
use super::web_journal::{Journal, WebRecovery};
use crate::card::EditorState;
use nodecraft_format::{AssetStore, MAX_ARCHIVE_BYTES, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub(super) use browser::guard_unload;
#[cfg(target_arch = "wasm32")]
pub(crate) use browser::pick_package;

pub(super) type PickFuture<T, E = String> = Pin<Box<dyn Future<Output = Result<T, E>>>>;

/// A file the person chose to open.
pub(super) struct Picked {
    pub name: String,
    pub bytes: Vec<u8>,
    /// Where saves go: the port's handle id, or `None` to download.
    pub target: Option<u64>,
}

/// Where the person chose to save.
pub(super) struct Named {
    pub name: String,
    pub target: Option<u64>,
}

/// The browser's side: pickers, and writing a saved project out.
pub(super) trait Port {
    /// Asks for a project to open, refusing one over `max` bytes.
    fn open(&mut self, max: u64) -> PickFuture<Option<Picked>>;
    /// Asks where to save, suggesting `suggested`.
    fn save_as(&mut self, suggested: &str) -> PickFuture<Option<Named>>;
    /// Writes `bytes` to `target`, or downloads them as `name`.
    fn write(&mut self, target: Option<u64>, name: &str, bytes: Vec<u8>) -> PickFuture<()>;
}

/// No browser: every pick says so.
pub(super) struct Detached;

impl Port for Detached {
    fn open(&mut self, _max: u64) -> PickFuture<Option<Picked>> {
        Box::pin(std::future::ready(Err(UNAVAILABLE.to_owned())))
    }
    fn save_as(&mut self, _suggested: &str) -> PickFuture<Option<Named>> {
        Box::pin(std::future::ready(Err(UNAVAILABLE.to_owned())))
    }
    fn write(&mut self, _target: Option<u64>, _name: &str, _bytes: Vec<u8>) -> PickFuture<()> {
        Box::pin(std::future::ready(Err(UNAVAILABLE.to_owned())))
    }
}

const UNAVAILABLE: &str = "Saving and opening are unavailable in this window.";

/// Bytes passing between the browser and the session.
#[derive(Default)]
struct Staged {
    /// The picked file, for the `file.open` under way.
    incoming: Option<(String, Vec<u8>)>,
    /// The last save, for the browser to write.
    written: Option<(String, Vec<u8>)>,
}

/// The session's storage in the browser (see the module docs), and the journal its recovery
/// writes go to, when the browser keeps one.
pub(super) struct HostStorage(Rc<RefCell<Staged>>, Option<Rc<RefCell<Journal>>>);

impl HostStorage {
    fn journal(&self) -> Result<Option<std::cell::RefMut<'_, Journal>>, Error> {
        self.1.as_ref().map(|journal| journal.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "recovery is busy"))).transpose()
    }
}

fn key(location: &StorageLocation) -> Result<&str, Error> {
    match location {
        StorageLocation::Key(key) => Ok(key),
        // Only native builds (the tests) read a command's location as a path.
        StorageLocation::Path(path) => path.to_str().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the project location isn't valid UTF-8")),
    }
}

impl ProjectStorage for HostStorage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        let key = key(location)?;
        let mut staged = self.0.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "project files are busy"))?;
        match staged.incoming.take() {
            Some((picked, bytes)) if picked == key => Ok(bytes),
            other => {
                staged.incoming = other;
                Err(Error::new(ErrorKind::NotFound, "pick the project to open it in the browser"))
            }
        }
    }
    fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error> {
        let key = key(location)?;
        if archive.len() > MAX_ARCHIVE_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, "project exceeds archive size limit"));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(archive.len()).map_err(|_| Error::new(ErrorKind::Internal, "not enough memory to save the project"))?;
        bytes.extend_from_slice(archive);
        self.0.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "project files are busy"))?.written = Some((key.to_owned(), bytes));
        Ok(())
    }
    // Without a journal the page still asks before unsaved work is left.
    fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
        self.journal()?.map_or(Ok(()), |mut journal| journal.checkpoint(recovery))
    }
    fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
        self.journal()?.map_or(Ok(()), |mut journal| journal.append(frame))
    }
    fn discard(&mut self) -> Result<(), Error> {
        match self.journal()? {
            Some(mut journal) => {
                journal.discard();
                Ok(())
            }
            None => Err(Error::new(ErrorKind::Unsupported, "recovery discard is not supported by this host")),
        }
    }
}

enum Phase {
    Opening(PickFuture<Option<Picked>>),
    /// The picked project waits while the person decides about unsaved changes.
    Confirm(Picked),
    Naming(PickFuture<Option<Named>>),
    Writing {
        future: PickFuture<()>,
        name: String,
        download: bool,
    },
}

pub(super) struct WebFiles {
    port: Box<dyn Port>,
    staged: Rc<RefCell<Staged>>,
    phase: Option<Phase>,
    /// Where each location's saves go.
    targets: BTreeMap<String, Option<u64>>,
    picks: u64,
    /// The last save wasn't written: the session counts it saved, the file doesn't have it.
    unwritten: bool,
    /// Whether leaving the page would lose work, for the page's unload guard.
    pub(super) unsaved: Rc<Cell<bool>>,
}

impl WebFiles {
    /// The files over `port`, and the storage to build the session on.
    pub(super) fn new(port: Box<dyn Port>) -> (Self, HostStorage) {
        let staged = Rc::new(RefCell::new(Staged::default()));
        let files = Self { port, staged: Rc::clone(&staged), phase: None, targets: BTreeMap::new(), picks: 0, unwritten: false, unsaved: Rc::default() };
        (files, HostStorage(staged, None))
    }

    /// Files with no browser behind them, until the host gives one.
    pub(super) fn detached() -> Self {
        Self::new(Box::new(Detached)).0
    }

    /// The browser's own port.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn browser() -> (Self, HostStorage) {
        Self::new(Box::<browser::Browser>::default())
    }

    #[cfg(test)]
    fn busy(&self) -> bool {
        self.phase.is_some()
    }

    /// Busy with something other than waiting for a pick to open, which any other file action
    /// replaces: a file field may never say it was cancelled.
    fn blocking(&self) -> bool {
        !matches!(self.phase, None | Some(Phase::Opening(_)))
    }

    /// A fresh location for a file named `name`.
    fn location(&mut self, name: &str) -> String {
        self.picks = self.picks.wrapping_add(1);
        let name: String = name.chars().filter(|c| !c.is_control() && *c != '/').take(200).collect();
        let name = if name.trim().is_empty() { "Untitled.ncraft".to_owned() } else { name };
        format!("{}/{name}", self.picks)
    }
}

/// The file name part of a location.
fn file_name(location: &str) -> &str {
    location.split_once('/').map_or(location, |(_, name)| name)
}

struct Repaint(egui::Context);
impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.request_repaint();
    }
}

pub(super) fn poll<T, E>(future: &mut PickFuture<T, E>, ctx: &egui::Context) -> Poll<Result<T, E>> {
    let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
    future.as_mut().poll(&mut Context::from_waker(&waker))
}

impl NodeCraftApp {
    /// The browser app on `services`, its project files over `files` (see the module docs), and
    /// unsaved work kept through `recovery` when the browser can keep it.
    pub(super) fn with_web_files(
        services: nodecraft_engine::commands::Services,
        (files, mut storage): (WebFiles, HostStorage),
        recovery: Option<WebRecovery>,
    ) -> Result<Self, Error> {
        storage.1 = recovery.as_ref().map(WebRecovery::journal);
        let kinds = nodecraft_engine::generation_catalog::default_registry()?.0;
        let (project, canvas) = Self::starter_document(&kinds)?;
        let snapshot = Snapshot { project, assets: AssetStore::default(), epoch: 0 };
        let session = nodecraft_engine::Session::with_storage(snapshot, Box::new(storage))?.with_services(services);
        let mut app = Self::from_session(session, canvas)?;
        nodecraft_engine::library::install(&mut app.session, Box::new(nodecraft_engine::library::MemoryLibrary::default()))?;
        nodecraft_engine::element_library::install(&mut app.session, Rc::new(nodecraft_engine::element_library::MemoryElements::default()));
        nodecraft_engine::shader_library::install(&mut app.session, Box::new(nodecraft_engine::library::MemoryLibrary::default()))?;
        nodecraft_engine::skills::install(&mut app.session, Rc::new(nodecraft_engine::skills::MemorySkills::default()))?;
        app.web_files = files;
        app.web_recovery = recovery;
        Ok(app)
    }

    /// Open, save or save as in the browser: `file.open`, `file.save` or `file.save_as`.
    pub(super) fn web_file(&mut self, id: &str, ctx: &egui::Context) {
        if self.web_files.blocking() {
            self.notice = Some("Wait for the current save or open to finish.".into());
            return;
        }
        if self.editors.values().any(EditorState::has_uncommitted) || self.bulk.has_uncommitted() || self.sheet_drafts_pending() {
            self.notice = Some("Finish or discard the current field edits before saving or opening a project.".into());
            return;
        }
        if !self.commit_mesh_views(ctx, true) {
            return;
        }
        let status = self.session.file_status();
        let current = status.location.as_ref().and_then(|location| key(location).ok()).map(str::to_owned);
        match id {
            "file.open" => self.web_files.phase = Some(Phase::Opening(self.web_files.port.open(MAX_ARCHIVE_BYTES as u64))),
            "file.save" if !status.read_only && current.as_ref().is_some_and(|key| self.web_files.targets.contains_key(key)) => {
                if let Some(key) = current {
                    self.web_save(None, &key);
                }
            }
            "file.save" | "file.save_as" => {
                let suggested = current.as_deref().map_or("Untitled.ncraft", file_name).to_owned();
                self.web_files.phase = Some(Phase::Naming(self.web_files.port.save_as(&suggested)));
            }
            _ => return,
        }
        self.web_files_frame(ctx);
    }

    /// Moves a pick or write along, and shows the unsaved-changes question. True while that
    /// question is up, so the canvas leaves input to it.
    pub(super) fn web_files_frame(&mut self, ctx: &egui::Context) -> bool {
        match self.web_files.phase.take() {
            Some(Phase::Opening(mut future)) => match poll(&mut future, ctx) {
                Poll::Pending => self.web_files.phase = Some(Phase::Opening(future)),
                Poll::Ready(Ok(Some(picked))) if self.web_unsaved() => self.web_files.phase = Some(Phase::Confirm(picked)),
                // Nothing here would be lost.
                Poll::Ready(Ok(Some(picked))) => self.web_open(picked, true),
                Poll::Ready(Ok(None)) => {}
                Poll::Ready(Err(error)) => self.notice = Some(error),
            },
            Some(Phase::Naming(mut future)) => match poll(&mut future, ctx) {
                Poll::Pending => self.web_files.phase = Some(Phase::Naming(future)),
                Poll::Ready(Ok(Some(named))) => {
                    let location = self.web_files.location(&named.name);
                    self.web_save(Some(named.target), &location);
                    // The write is polled from the next frame.
                    ctx.request_repaint();
                }
                Poll::Ready(Ok(None)) => {}
                Poll::Ready(Err(error)) => self.notice = Some(error),
            },
            Some(Phase::Writing { mut future, name, download }) => match poll(&mut future, ctx) {
                Poll::Pending => self.web_files.phase = Some(Phase::Writing { future, name, download }),
                Poll::Ready(Ok(())) => {
                    self.web_files.unwritten = false;
                    if download {
                        self.notice = Some(format!("Downloaded {name}. Open it again from your downloads to keep working on that copy."));
                    }
                }
                Poll::Ready(Err(error)) => {
                    self.web_files.unwritten = true;
                    self.notice = Some(format!("{name} wasn't saved: {error} The project is still open here; save it again."));
                }
            },
            Some(Phase::Confirm(picked)) => self.web_files.phase = Some(Phase::Confirm(picked)),
            None => {
                // A save made some other way than the File menu still reaches the file.
                let left = self.web_files.staged.try_borrow_mut().ok().and_then(|mut staged| staged.written.take());
                if let Some((saved, bytes)) = left {
                    self.web_write(&saved, bytes);
                    ctx.request_repaint();
                }
            }
        }
        let asking = self.web_confirm(ctx);
        let unsaved = self.web_unsaved();
        self.web_files.unsaved.set(unsaved);
        asking
    }

    /// Whether leaving or opening another project would lose work: edits since the last save
    /// (not the untouched starter project), a write under way, or a save the browser didn't write.
    pub(super) fn web_unsaved(&self) -> bool {
        let status = self.session.file_status();
        let edited = status.dirty && (status.location.is_some() || self.session.can_undo());
        edited || self.web_recovered_unsaved() || self.web_files.unwritten || matches!(self.web_files.phase, Some(Phase::Writing { .. }))
    }

    /// The browser's File menu: open, save and save as.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn web_file_button(&mut self, ui: &egui::Ui, t: &crate::theme::Tokens) {
        let mut chosen = None;
        egui::Area::new(egui::Id::new("nodecraft.web_files.menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(ui.max_rect().min + egui::Vec2::new(t.card_padding, t.card_padding * 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding * 0.5).show(ui, |ui| {
                    ui.add_enabled_ui(!self.web_files.blocking() && self.model_choice.is_none() && !self.model_frame_owned, |ui| {
                        ui.menu_button("File", |ui| {
                            for (id, label, icon) in [
                                ("file.open", "Open project…", crate::icons::Icon::Open),
                                ("file.save", "Save project", crate::icons::Icon::Save),
                                ("file.save_as", "Save project as…", crate::icons::Icon::Save),
                            ] {
                                if ui.add(icon.button(ui, label, t)).clicked() {
                                    chosen = Some(id);
                                    ui.close();
                                }
                            }
                        });
                    });
                });
            });
        if let Some(id) = chosen {
            self.web_file(id, ui.ctx());
        }
    }

    /// Asks before a picked project replaces one with unsaved changes.
    fn web_confirm(&mut self, ctx: &egui::Context) -> bool {
        let Some(Phase::Confirm(picked)) = &self.web_files.phase else { return false };
        let name: String = picked.name.chars().filter(|c| !c.is_control()).take(80).collect();
        let t = self.theme.tokens();
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .id(egui::Id::new("nodecraft.web_files.confirm"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(
                egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius),
            )
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("Open {name}? This project's unsaved changes will be lost.")).color(t.text));
                ui.add_space(t.control_gap);
                ui.horizontal(|ui| {
                    if ui.button("Discard and open").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        choice = Some(false);
                    }
                });
            });
        if let Some(discard) = choice {
            self.web_answer(discard);
            ctx.request_repaint();
        }
        true
    }

    /// The answer to the unsaved-changes question: open the picked project, or keep this one.
    fn web_answer(&mut self, discard: bool) {
        if let Some(Phase::Confirm(picked)) = self.web_files.phase.take()
            && discard
        {
            self.web_open(picked, true);
        }
    }

    fn web_open(&mut self, picked: Picked, discard: bool) {
        let location = self.web_files.location(&picked.name);
        if let Ok(mut staged) = self.web_files.staged.try_borrow_mut() {
            staged.incoming = Some((location.clone(), picked.bytes));
        }
        let result = self.session.dispatch("file.open", &json!({"location": location, "discard_dirty": discard}));
        // Opened or refused, the bytes aren't needed any more.
        if let Ok(mut staged) = self.web_files.staged.try_borrow_mut() {
            staged.incoming = None;
        }
        match result {
            Ok(_) => {
                self.web_files.targets.insert(location, picked.target);
                self.web_files.unwritten = false;
                self.after_file_open();
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Saves to `location` (a new one when `target` is given, from a save-as pick), then hands
    /// the bytes to the browser to write.
    fn web_save(&mut self, target: Option<Option<u64>>, location: &str) {
        let result = match target {
            Some(_) => self.session.dispatch("file.save_as", &json!({"location": location})),
            None => self.session.dispatch("file.save", &json!({})),
        };
        let written = self.web_files.staged.try_borrow_mut().ok().and_then(|mut staged| staged.written.take());
        match result {
            Ok(value) if value.get("saved").and_then(serde_json::Value::as_bool) == Some(true) => {}
            Ok(_) => {
                self.notice = Some("The save did not confirm publication; the current project was kept.".into());
                return;
            }
            Err(error) => {
                self.notice = Some(error.message);
                return;
            }
        }
        if let Some(target) = target {
            self.web_files.targets.insert(location.to_owned(), target);
        }
        let Some((saved, bytes)) = written.filter(|(saved, _)| saved == location) else {
            self.web_files.unwritten = true;
            self.notice = Some("The project was saved but nothing was handed to the browser to write; save it again.".into());
            return;
        };
        self.web_write(&saved, bytes);
    }

    /// Hands a save's bytes to the browser, for the file picked for `saved` or as a download.
    fn web_write(&mut self, saved: &str, bytes: Vec<u8>) {
        let target = self.web_files.targets.get(saved).copied().flatten();
        let name = file_name(saved).to_owned();
        let future = self.web_files.port.write(target, &name, bytes);
        self.web_files.phase = Some(Phase::Writing { future, name, download: target.is_none() });
    }
}

#[cfg(test)]
mod tests;
