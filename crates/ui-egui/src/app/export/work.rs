//! One background file at a time, including numbered Export All continuations.
use super::{MAX_NUMBERS, NodeCraftApp, numbered};
use nodecraft_engine::export_job::{ExportPhase, ExportResult, ExportWatch};
use nodecraft_types::ProjectId;
use serde_json::{Value as Json, json};
use std::{collections::VecDeque, path::PathBuf, sync::Arc};

struct Item {
    command: &'static str,
    params: Json,
    base: String,
    attempts: usize,
}
struct Current {
    id: u64,
    watch: ExportWatch,
    item: Item,
    name: String,
}
pub(super) struct Work {
    session: Arc<()>,
    project: ProjectId,
    folder: Option<PathBuf>,
    files: VecDeque<Item>,
    current: Option<Current>,
    next: usize,
    total: usize,
    written: usize,
    failed: Option<String>,
    single_name: String,
    cancel_requested: bool,
}
fn short_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().chars().take(80).collect()).unwrap_or_default()
}
impl Work {
    pub(super) fn file(app: &NodeCraftApp, command: &'static str, params: Json, path: PathBuf) -> Self {
        Self {
            session: app.session.view_identity(),
            project: app.session.document().id(),
            folder: None,
            files: VecDeque::from([Item { command, params, base: short_name(&path), attempts: 0 }]),
            current: None,
            next: 1,
            total: 1,
            written: 0,
            failed: None,
            single_name: short_name(&path),
            cancel_requested: false,
        }
    }
    pub(super) fn folder(app: &NodeCraftApp, files: Vec<(Json, String)>, folder: PathBuf) -> Self {
        Self {
            session: app.session.view_identity(),
            project: app.session.document().id(),
            folder: Some(folder),
            total: files.len(),
            files: files.into_iter().map(|(params, base)| Item { command: "history.export", params, base, attempts: 0 }).collect(),
            current: None,
            next: 1,
            written: 0,
            failed: None,
            single_name: String::new(),
            cancel_requested: false,
        }
    }
    fn failed(&mut self, name: &str, message: String) {
        self.failed.get_or_insert_with(|| if self.folder.is_some() { format!("{name}: {message}") } else { message });
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn watch(&self) -> Option<ExportWatch> {
        self.current.as_ref().map(|current| current.watch.clone())
    }
    fn retry(&mut self, item: Item) {
        if self.cancel_requested {
            return;
        }
        if item.attempts < MAX_NUMBERS {
            self.files.push_front(item);
        } else {
            self.failed(&item.base, "no free number left for it".into());
        }
    }
    fn cancel(&mut self) {
        self.cancel_requested = true;
        self.files.clear();
    }
    fn settle(&mut self, current: Current, outcome: Option<ExportResult>) -> bool {
        self.settle_outcome(current.item, &current.name, outcome)
    }
    fn settle_outcome(&mut self, item: Item, name: &str, outcome: Option<ExportResult>) -> bool {
        match outcome {
            Some(ExportResult::Exported { .. }) => self.written = self.written.saturating_add(1),
            Some(ExportResult::Cancelled) => self.cancel(),
            _ if self.cancel_requested => {}
            Some(ExportResult::Failed { name_taken: true, .. }) if self.folder.is_some() => self.retry(item),
            Some(ExportResult::Failed { error, .. }) => self.failed(name, error.message),
            _ => self.failed(name, "The export result is no longer available; export again.".into()),
        }
        self.cancel_requested
    }
    fn notice(&self) -> String {
        if self.cancel_requested {
            return if self.folder.is_none() {
                "Export cancelled; no file was written.".into()
            } else {
                format!("Export cancelled after {} of {} files; completed files were kept.", self.written, self.total)
            };
        }
        match &self.failed {
            Some(why) if self.folder.is_none() => why.clone(),
            Some(why) => format!("Exported {} of {} files; {why}", self.written, self.total),
            None if self.folder.is_none() => format!("Exported {}.", self.single_name),
            None => format!("Exported {} files.", self.written),
        }
    }
}
impl NodeCraftApp {
    pub(super) fn poll_export_work(&mut self, ctx: &egui::Context) {
        let Some(mut work) = self.exports.work.take() else { return };
        if !Arc::ptr_eq(&work.session, &self.session.view_identity()) {
            // Retain the old receipt until cleanup, including when Quit arrives in the new
            // Session. Numeric ids may collide; never cancel a new Session's different job.
            work.files.clear();
            work.failed.get_or_insert("Another project opened; no remaining files were started.".into());
        }
        let project_changed = work.project != self.session.document().id();
        if project_changed {
            work.files.clear();
            work.failed.get_or_insert("The project changed; no remaining files were started.".into());
            if let Some(current) = &work.current
                && self.session.export_watch().is_some_and(|watch| watch.same_job(&current.watch))
            {
                let _ = self.session.dispatch("export.cancel", &json!({"id":current.id}));
            }
        }
        if let Some(current) = work.current.take() {
            let status = Some(current.watch.status());
            if self.exports.pointer_owned || status.as_ref().is_some_and(|status| status.result.is_none()) {
                work.current = Some(current);
                self.exports.work = Some(work);
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            if work.settle(current, status.and_then(|status| status.result)) {
                self.notice = Some(work.notice());
                return;
            }
        }
        if project_changed {
            // A racing occupied-name result must not requeue work from the old project.
            work.files.clear();
        }
        let Some(mut item) = work.files.pop_front() else {
            self.notice = Some(work.notice());
            return;
        };
        let name = if let Some(folder) = &work.folder {
            let name = numbered(&item.base, work.next);
            work.next = work.next.saturating_add(1);
            item.attempts = item.attempts.saturating_add(1);
            item.params["path"] = json!(folder.join(&name));
            name
        } else {
            item.base.clone()
        };
        item.params["background"] = json!(true);
        if let Some(runner) = &self.exports.runner {
            self.session.set_export_runner(Arc::clone(runner));
        }
        match self.session.dispatch(item.command, &item.params) {
            Ok(reply) => {
                if let Some(id) = reply.get("pending").and_then(Json::as_u64) {
                    if let Some(watch) = self.session.export_watch().filter(|watch| watch.status().id == id) {
                        work.current = Some(Current { id, watch, item, name });
                    } else {
                        work.failed(&name, "The export worker receipt is unavailable.".into());
                    }
                } else {
                    work.failed(&name, "The export worker did not return a receipt.".into());
                }
            }
            Err(error) => work.failed(&name, error.message),
        }
        self.exports.work = Some(work);
        ctx.request_repaint();
    }

    /// A small non-modal progress area owns only its pointer gestures; the rest of the canvas
    /// stays available while authentication, copying and filesystem sync happen on the worker.
    pub(in crate::app) fn export_overlay(&mut self, ctx: &egui::Context) -> bool {
        if !ctx.input(|i| i.pointer.primary_down() || i.pointer.any_released()) {
            self.exports.pointer_owned = false;
        }
        let Some(work) = &self.exports.work else {
            self.exports.rect = egui::Rect::NOTHING;
            return self.exports.pointer_owned;
        };
        let Some(current) = &work.current else { return self.exports.pointer_owned };
        let status = current.watch.status();
        let same_job = self.session.export_watch().is_some_and(|watch| watch.same_job(&current.watch));
        let t = self.theme.tokens();
        let other_owner = crate::canvas::text_input_focused(ctx)
            || egui::Popup::is_any_open(ctx)
            || self.keys.open
            || self.palette.is_some()
            || self.turn_into.is_some()
            || self.drag.is_some()
            || self.wiring.is_some()
            || self.reordering.is_some()
            || self.marquee.is_some()
            || self.navigation.pointer_owned
            || self.comments.drag.is_some()
            || self.comments.gesture_cancelled
            || self.drop_pointer_owned();
        let mut cancel = false;
        let (anchor, offset) = if self.timeline_editor.is_some() {
            // The fullscreen editor owns the top-left title/clip list. Keep the shared
            // non-modal export controls in its otherwise blank center below the toolbar.
            (egui::Align2::CENTER_TOP, egui::vec2(0.0, t.toolbar_height + t.card_padding))
        } else {
            (egui::Align2::LEFT_TOP, egui::vec2(t.card_padding, t.card_padding))
        };
        let shown = egui::Area::new(egui::Id::new("background_export")).order(egui::Order::Foreground).anchor(anchor, offset).show(ctx, |ui| {
            egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding).show(ui, |ui| {
                ui.set_max_width(t.card_width);
                let phase = match status.phase {
                    _ if !same_job && status.result.is_none() => "Cancelling previous export…",
                    phase if !status.cancellable && phase != ExportPhase::Finishing && phase != ExportPhase::Completed => "Cancelling export…",
                    ExportPhase::Queued => "Preparing export…",
                    ExportPhase::Checking => "Checking result bytes…",
                    ExportPhase::Writing => "Writing export…",
                    ExportPhase::Finishing => "Finishing export…",
                    ExportPhase::Completed => "Export finished",
                };
                ui.label(phase);
                ui.label(format!("{} ({} of {})", current.name, work.written.saturating_add(1), work.total));
                if let Some(progress) = &status.archive {
                    ui.label(format!(
                        "Media: {} of {} · {} of {} bytes copied",
                        progress.completed_media, progress.total_media, progress.copied_bytes, progress.total_bytes
                    ));
                }
                let button =
                    ui.add_enabled(status.cancellable && same_job && !other_owner, egui::Button::new("Cancel export")).on_disabled_hover_text(if !same_job {
                        "The previous project is releasing its export files."
                    } else if status.cancellable {
                        "Finish the current text edit, menu or canvas gesture before cancelling."
                    } else {
                        "The export is finishing or has completed; it can no longer be cancelled."
                    });
                cancel = button.clicked()
                    && ui.input(|i| {
                        i.events.iter().all(|event| {
                            !matches!(event,
                    egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed:false,..} if !button.rect.contains(*pos))
                        })
                    });
                if status.phase == ExportPhase::Finishing {
                    ui.label("Finishing cannot be cancelled.");
                }
            });
        });
        self.exports.rect = shown.response.rect;
        if !other_owner && ctx.input(|i| i.events.iter().any(|event| matches!(event,egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed:true,..} if self.exports.rect.contains(*pos)))) {
            self.exports.pointer_owned=true;
        }
        let id = current.id;
        if cancel {
            match self.session.dispatch("export.cancel", &json!({"id":id})) {
                Ok(_) => {
                    if let Some(work) = &mut self.exports.work {
                        work.cancel();
                    }
                    self.notice = Some("Cancelling export…".into());
                }
                Err(error) => self.notice = Some(error.message),
            }
        }
        self.exports.pointer_owned
            || (!other_owner && ctx.input(|i| !i.pointer.primary_down() && i.pointer.hover_pos().is_some_and(|pos| self.exports.rect.contains(pos))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_batch_cancel_never_retries_a_racing_name_conflict() {
        let item = || Item { command: "history.export", params: json!({}), base: "result.txt".into(), attempts: 1 };
        let mut work = Work {
            session: Arc::new(()),
            project: ProjectId::from_bytes([1; 16]),
            folder: Some(PathBuf::from("public-export")),
            files: VecDeque::from([item()]),
            current: None,
            next: 2,
            total: 3,
            written: 1,
            failed: None,
            single_name: String::new(),
            cancel_requested: false,
        };
        work.cancel();
        assert!(work.settle_outcome(
            item(),
            "result-1.txt",
            Some(ExportResult::Failed { error: nodecraft_types::Error::new(nodecraft_types::ErrorKind::Conflict, "name taken"), name_taken: true })
        ));
        assert!(work.files.is_empty(), "cancellation must not restart the numbered queue");
        assert_eq!(work.next, 2);
        assert_eq!(work.written, 1);
        assert_eq!(work.notice(), "Export cancelled after 1 of 3 files; completed files were kept.");
    }
}
