//! UI-only location acquisition. Every file mutation still goes through Session dispatch.
use super::*;
use nodecraft_engine::settings::ProjectSettings;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

type PickFuture = Pin<Box<dyn Future<Output = Result<Option<PathBuf>, String>>>>;
type Picker = Box<dyn FnMut(bool) -> Result<PickFuture, String>>;

#[derive(Clone)]
struct Stamp {
    session: Arc<()>,
    revision: u64,
}
impl Stamp {
    fn capture(app: &NodeCraftApp) -> Self {
        Self { session: app.session.view_identity(), revision: app.session.revision() }
    }
    fn matches(&self, app: &NodeCraftApp) -> bool {
        Arc::ptr_eq(&self.session, &app.session.view_identity())
            && self.revision == app.session.revision()
            && !app.editors.values().any(EditorState::has_uncommitted)
    }
}
enum Purpose {
    Open,
    Save { then_open: Option<PathBuf> },
}
enum Phase {
    Picking { future: PickFuture, purpose: Purpose },
    Confirm(PathBuf),
    Saving { then_open: Option<PathBuf>, saved_path: Option<PathBuf> },
}
struct Pending {
    stamp: Stamp,
    phase: Phase,
}
pub(super) struct FileDialogs {
    pending: Option<Pending>,
    picker: Picker,
    pub(super) projects: ProjectSettings,
    observed: Option<String>,
    menu_open: bool,
    // Completed-frame number: every pass of the choice/completion frame stays owned.
    owner_frame: Option<u64>,
}
impl Default for FileDialogs {
    fn default() -> Self {
        #[cfg(not(test))]
        let picker: Picker = Box::new(native_picker);
        #[cfg(test)]
        let picker: Picker = Box::new(|_| Ok(Box::pin(std::future::ready(Ok(None)))));
        Self { pending: None, picker, projects: Default::default(), observed: None, menu_open: false, owner_frame: None }
    }
}
#[cfg_attr(test, allow(dead_code))]
fn native_picker(save: bool) -> Result<PickFuture, String> {
    // Construction is intentionally on the UI main thread: AppKit discovers its window here.
    let dialog = rfd::AsyncFileDialog::new();
    if save {
        // A save filter appends its extension to explicit other extensions on Windows.
        // Suggest the format through the filename instead, preserving the chosen path.
        let future = dialog.set_title("Save project as").set_file_name("Untitled.ncraft").save_file();
        Ok(Box::pin(async move { future.await.map(|file| selected_path(file.path())).transpose() }))
    } else {
        let future = dialog.add_filter("NodeCraft project", &["ncraft"]).add_filter("All files", &["*"]).set_title("Open project").pick_file();
        Ok(Box::pin(async move { future.await.map(|file| selected_path(file.path())).transpose() }))
    }
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

fn selected_path(path: &Path) -> Result<PathBuf, String> {
    if !path.to_str().is_some_and(ProjectSettings::valid_path) {
        return Err("Choose an absolute UTF-8 project path of at most 4096 bytes without NUL.".into());
    }
    Ok(path.to_path_buf())
}
impl FileDialogs {
    pub(super) fn owns_input(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn owns_navigation(&self, ctx: &egui::Context) -> bool {
        self.pending.is_some() || self.menu_open || self.owner_frame == Some(ctx.cumulative_frame_nr())
    }
    pub(super) fn blocks_model_picker(&self) -> bool {
        self.pending.is_some() || self.menu_open || self.owner_frame.is_some()
    }
    fn own_frame(&mut self, ctx: &egui::Context) {
        self.owner_frame = Some(ctx.cumulative_frame_nr());
    }
}

fn admitted(path: &Path, save: bool) -> Result<&str, String> {
    let text = path
        .to_str()
        .filter(|text| ProjectSettings::valid_path(text))
        .ok_or_else(|| "Choose an absolute UTF-8 project path of at most 4096 bytes without NUL.".to_owned())?;
    nodecraft_format::StorageLocation::from_command(text.to_owned()).map_err(|e| e.message)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            // Complete canonical aliases are supported by NativeStorage; dangling aliases and
            // nonregular sources/destinations never reach an open that could block on a pipe.
            let resolved = path.canonicalize().map_err(|_| "The selected path cannot be resolved; choose its actual target.".to_owned())?;
            if !std::fs::metadata(resolved).is_ok_and(|m| m.is_file()) {
                return Err("Choose an ordinary project file, not a folder or device.".into());
            }
        }
        Err(e) if save && e.kind() == std::io::ErrorKind::NotFound => {
            if !path.parent().is_some_and(|parent| parent.is_dir()) || path.file_name().is_none() {
                return Err("Choose a file in an existing writable folder.".into());
            }
        }
        Err(_) => return Err("The selected project cannot be found or inspected; choose another file.".into()),
    }
    Ok(text)
}
impl NodeCraftApp {
    fn file_busy(&self, ctx: &egui::Context) -> bool {
        self.session.file_status().operation.is_some()
            || self.model_choice.is_some()
            || self.model_frame_owned
            || self.recovery.is_some()
            || self.files.menu_open
            || egui::Popup::is_any_open(ctx)
            || self.palette.is_some()
            || self.turn_into.is_some()
            || self.drag.is_some()
            || self.marquee.is_some()
            || self.wiring.is_some()
            || self.reordering.is_some()
            || self.comments.drag.is_some()
            || self.comments.gesture_cancelled
            || self.navigation.pointer_owned
            || self.navigation.drag.is_some()
            || ctx.input(|i| i.pointer.primary_down())
    }
    pub(super) fn file_shortcut(&mut self, ctx: &egui::Context, save: bool, save_as: bool) -> Option<String> {
        let id = if save_as {
            "file.save_as"
        } else if save {
            "file.save"
        } else {
            "file.open"
        };
        self.begin_file(id, ctx);
        self.notice.take()
    }
    pub(super) fn begin_file(&mut self, id: &str, ctx: &egui::Context) {
        if self.files.pending.is_some() || self.file_busy(ctx) {
            return;
        }
        if !self.session.file_status().available {
            self.notice = Some("Saving and opening are unavailable in this window.".into());
            return;
        }
        if self.editors.values().any(EditorState::has_uncommitted) {
            self.notice = Some("Finish or discard the current field edits before saving or opening a project.".into());
            return;
        }
        self.files.own_frame(ctx);
        self.notice = None;
        let status = self.session.file_status();
        if id == "file.save" && status.location.is_some() && !status.read_only {
            self.start_file_save(None, None, Stamp::capture(self));
            return;
        }
        let purpose = if id == "file.open" { Purpose::Open } else { Purpose::Save { then_open: None } };
        self.start_picker(Stamp::capture(self), purpose);
        ctx.request_repaint();
    }
    fn start_picker(&mut self, stamp: Stamp, purpose: Purpose) {
        let save = matches!(purpose, Purpose::Save { .. });
        let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.files.picker)(save)));
        match future {
            Ok(Ok(future)) => self.files.pending = Some(Pending { stamp, phase: Phase::Picking { future, purpose } }),
            _ => self.notice = Some("The native file picker could not be opened. Try again or use an explicit file command.".into()),
        }
    }
    fn remember_project(&mut self, path: &Path) {
        let Some(text) = path.to_str().filter(|text| ProjectSettings::valid_path(text)) else {
            return;
        };
        self.files.observed = Some(text.to_owned());
        self.files.projects.remember(text);
        if let Some(store) = &mut self.store {
            let projects = self.files.projects.clone();
            store.update(|s| s.projects = projects);
            if store.is_read_only() || store.flush().is_err() {
                self.notice = Some("The project operation succeeded, but recent projects could not be saved in settings.".into());
            }
        }
    }
    fn open_selected(&mut self, path: PathBuf, discard: bool) {
        let result = admitted(&path, false)
            .and_then(|location| self.session.dispatch("file.open", &json!({"location":location,"discard_dirty":discard})).map(|_| ()).map_err(|e| e.message));
        match result {
            Ok(()) => {
                self.after_file_open();
                self.remember_project(&path);
            }
            Err(error) => self.notice = Some(error),
        }
    }
    fn after_file_open(&mut self) {
        if let Some(canvas) = self.session.document().canvases().keys().next() {
            self.canvas = *canvas;
        }
        self.selection.clear();
        self.editors.clear();
        self.centred = false;
    }
    fn choose_open(&mut self, path: PathBuf, stamp: Stamp) {
        if let Err(error) = admitted(&path, false) {
            self.notice = Some(error);
            return;
        }
        if self.session.file_status().dirty {
            self.files.pending = Some(Pending { stamp, phase: Phase::Confirm(path) });
        } else {
            self.open_selected(path, false);
        }
    }
    fn save_then_open(&mut self, path: PathBuf, stamp: Stamp) {
        let status = self.session.file_status();
        if status.location.is_none() || status.read_only {
            self.start_picker(stamp, Purpose::Save { then_open: Some(path) });
        } else {
            self.start_file_save(None, Some(path), stamp);
        }
    }
    fn start_file_save(&mut self, path: Option<PathBuf>, then_open: Option<PathBuf>, stamp: Stamp) {
        let result = match &path {
            Some(path) => admitted(path, true)
                .and_then(|location| self.session.dispatch("file.save_as", &json!({"location":location,"background":true})).map_err(|error| error.message)),
            None => self.session.dispatch("file.save", &json!({"background":true})).map_err(|error| error.message),
        };
        match result {
            Ok(result) if result.get("saving").and_then(serde_json::Value::as_bool) == Some(true) => {
                self.files.pending = Some(Pending { stamp, phase: Phase::Saving { then_open, saved_path: path } });
            }
            Ok(result) if result.get("saved").and_then(serde_json::Value::as_bool) == Some(true) => {
                self.complete_file_save(stamp, then_open, path, Ok(()));
            }
            Ok(_) => self.notice = Some("The save did not confirm publication; the current project was kept.".into()),
            Err(error) => self.notice = Some(error),
        }
    }
    fn complete_file_save(&mut self, stamp: Stamp, then_open: Option<PathBuf>, saved_path: Option<PathBuf>, result: Result<(), String>) {
        if !stamp.matches(self) {
            self.notice = Some("The project changed while saving. Open the project again.".into());
            return;
        }
        if let Err(error) = result {
            self.notice = Some(error);
            return;
        }
        if let Some(path) = saved_path {
            self.remember_project(&path);
        }
        if let Some(path) = then_open {
            self.open_selected(path, false);
        }
    }
    pub(super) fn file_save_completed(&mut self, ctx: &egui::Context, result: Result<serde_json::Value, Error>) {
        if !self.files.pending.as_ref().is_some_and(|pending| matches!(pending.phase, Phase::Saving { .. })) {
            if let Err(error) = result {
                self.notice = Some(error.message);
            }
            return;
        }
        self.files.own_frame(ctx);
        if let Some(Pending { stamp, phase: Phase::Saving { then_open, saved_path } }) = self.files.pending.take() {
            let result = result.and_then(|value| {
                if value.get("saved").and_then(serde_json::Value::as_bool) == Some(true) {
                    Ok(())
                } else {
                    Err(Error::new(ErrorKind::Internal, "The save did not confirm publication; the current project was kept."))
                }
            });
            self.complete_file_save(stamp, then_open, saved_path, result.map_err(|error| error.message));
        }
        self.clear_file_input(ctx);
    }
    /// Called before canvas input. A completion/choice keeps ownership for this entire frame.
    pub(super) fn file_dialog_frame(&mut self, ctx: &egui::Context) -> bool {
        // The public model-opening intent has no egui Context. Keep this marker only for
        // the current completed-frame epoch, including later passes after a File completion.
        if self.files.owner_frame != Some(ctx.cumulative_frame_nr()) {
            self.files.owner_frame = None;
        }
        if self.files.pending.is_some() || self.files.menu_open {
            self.files.own_frame(ctx);
        }
        // Successful startup/programmatic Open/SaveAs are visible through the adopted clean
        // location too. This is recent-list presentation, never deletion/save authority.
        let status = self.session.file_status();
        if !status.dirty
            && let Some(nodecraft_format::StorageLocation::Path(path)) = status.location
            && path.to_str().is_some_and(|text| ProjectSettings::valid_path(text) && self.files.observed.as_deref() != Some(text))
        {
            self.remember_project(&path);
        }
        let Some(mut pending) = self.files.pending.take() else {
            // A later egui pass in this same completed-frame epoch still owns input.
            return self.files.owns_navigation(ctx);
        };
        let mut progressed = false;
        match pending.phase {
            Phase::Picking { ref mut future, .. } => {
                let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
                let mut cx = Context::from_waker(&waker);
                let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(&mut cx)));
                let path = match poll {
                    Ok(Poll::Pending) => {
                        self.files.pending = Some(pending);
                        self.clear_file_input(ctx);
                        return true;
                    }
                    Ok(Poll::Ready(Ok(path))) => path,
                    Ok(Poll::Ready(Err(error))) => {
                        self.notice = Some(error);
                        self.clear_file_input(ctx);
                        return true;
                    }
                    Err(_) => {
                        self.notice = Some("The native file picker failed; no project was changed.".into());
                        self.clear_file_input(ctx);
                        return true;
                    }
                };
                progressed = true;
                if let Some(path) = path {
                    if !pending.stamp.matches(self) {
                        self.notice = Some("The project changed while the file picker was open. Choose the file again.".into());
                    } else if let Phase::Picking { purpose, .. } = pending.phase {
                        match purpose {
                            Purpose::Open => self.choose_open(path, pending.stamp),
                            Purpose::Save { then_open } => {
                                self.start_file_save(Some(path), then_open, pending.stamp);
                            }
                        }
                    }
                }
            }
            Phase::Saving { then_open, saved_path } => {
                if pending.stamp.matches(self) {
                    self.files.pending = Some(Pending { stamp: pending.stamp, phase: Phase::Saving { then_open, saved_path } });
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                } else {
                    self.notice = Some("The project changed while saving. Open the project again.".into());
                }
            }
            Phase::Confirm(path) => {
                if !pending.stamp.matches(self) {
                    self.notice = Some("The project changed while the confirmation was open. Open the project again.".into());
                } else {
                    let mut choice = None;
                    egui::Modal::new(egui::Id::new("file_dirty_confirmation")).show(ctx, |ui| {
                        ui.heading("Save changes before opening?");
                        ui.label("Save keeps the current project. Discard opens the selected file without saving these changes.");
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                choice = Some(0);
                            }
                            if ui.button("Discard").clicked() {
                                choice = Some(1);
                            }
                            if ui.button("Cancel").clicked() {
                                choice = Some(2);
                            }
                        });
                    });
                    progressed = choice.is_some();
                    match choice {
                        Some(0) => self.save_then_open(path, pending.stamp),
                        Some(1) => self.open_selected(path, true),
                        Some(_) => {}
                        None => self.files.pending = Some(Pending { stamp: pending.stamp, phase: Phase::Confirm(path) }),
                    }
                }
            }
        }
        if progressed && self.files.pending.is_some() {
            ctx.request_repaint();
        }
        self.clear_file_input(ctx);
        true
    }
    pub(super) fn clear_file_input(&self, ctx: &egui::Context) {
        ctx.input_mut(|i| {
            i.events.clear();
            i.raw.events.clear();
            i.raw.dropped_files.clear();
            i.keys_down.clear();
            i.modifiers = egui::Modifiers::NONE;
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        });
    }
    pub(super) fn file_button(&mut self, ui: &egui::Ui, t: &Tokens) {
        egui::Area::new(egui::Id::new("file_menu407"))
            .order(egui::Order::Foreground)
            .fixed_pos(ui.max_rect().min + Vec2::new(t.card_padding, t.card_padding * 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding * 0.5).show(ui, |ui| {
                    if ui
                        .add_enabled(
                            self.recovery.is_none() && self.files.pending.is_none() && self.model_choice.is_none() && !self.model_frame_owned,
                            egui::Button::new("File"),
                        )
                        .clicked()
                    {
                        self.files.menu_open = !self.files.menu_open;
                        self.files.own_frame(ui.ctx());
                        ui.ctx().request_repaint();
                    }
                });
            });
    }
    pub(super) fn file_menu(&mut self, ui: &egui::Ui, t: &Tokens) -> bool {
        if self.recovery.is_some() || self.files.pending.is_some() {
            return self.files.pending.is_some();
        }
        let was_open = self.files.menu_open;
        if was_open {
            self.files.own_frame(ui.ctx());
        }
        if was_open && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.files.menu_open = false;
        }
        let mut action = None;
        let mut recent = None;
        let mut remove = None;
        let mut clear = false;
        egui::Window::new("Project files")
            .id(egui::Id::new("project_files407"))
            .open(&mut self.files.menu_open)
            .collapsible(false)
            .resizable(false)
            .default_pos(ui.max_rect().min + Vec2::new(t.card_padding, t.card_padding * 7.0))
            .default_width(t.card_width)
            .max_height(ui.max_rect().height() * 0.8)
            .vscroll(true)
            .show(ui.ctx(), |ui| {
                for (id, label) in [("file.open", "Open project…"), ("file.save", "Save project"), ("file.save_as", "Save project as…")] {
                    if ui.button(label).clicked() {
                        action = Some(id);
                    }
                }
                ui.separator();
                ui.label("Recent projects");
                let paths = self.files.projects.recent();
                if paths.is_empty() {
                    ui.label("No recent projects");
                }
                for path in paths {
                    ui.horizontal(|ui| {
                        let label = Path::new(&path)
                            .file_name()
                            .map(|x| x.to_string_lossy().chars().filter(|c| !c.is_control()).take(80).collect::<String>())
                            .unwrap_or_default();
                        if ui.button(label).clicked() {
                            recent = Some(PathBuf::from(&path));
                        }
                        if ui.small_button("Remove").clicked() {
                            remove = Some(path.clone());
                        }
                    });
                }
                if ui.button("Clear recent projects").clicked() {
                    clear = true;
                }
            });
        let owns = was_open || self.files.menu_open || action.is_some() || recent.is_some();
        if action.is_some() || recent.is_some() {
            self.files.menu_open = false;
        }
        if let Some(id) = action {
            self.begin_file(id, ui.ctx());
        }
        if let Some(path) = recent
            && self.files.pending.is_none()
            && !self.file_busy(ui.ctx())
        {
            if self.editors.values().any(EditorState::has_uncommitted) {
                self.notice = Some("Finish or discard the current field edits before opening a project.".into());
            } else {
                self.choose_open(path, Stamp::capture(self));
            }
        }
        if clear || remove.is_some() {
            if clear {
                self.files.projects.recent.clear();
            } else if let Some(path) = remove {
                self.files.projects.forget(&path);
            }
            if let Some(store) = &mut self.store {
                let projects = self.files.projects.clone();
                store.update(|s| s.projects = projects);
                if store.is_read_only() || store.flush().is_err() {
                    self.notice = Some("Recent projects could not be saved in settings.".into());
                }
            }
        }
        owns
    }
}

#[cfg(test)]
mod tests;
