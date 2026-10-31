//! The progress window for a background save or open (#367). It shows which step the operation
//! is on and offers Cancel while cancelling can still leave everything as it was: a save until
//! its file is published, an open until its recovery checkpoint is. A quick save never shows it;
//! the window appears only once an operation has run for [`SHOW_AFTER`].
use super::*;
use nodecraft_engine::file_cmds::{FileOperation, FileOperationKind, FilePhase};

/// How long an operation runs before the window appears, in seconds of frame time, so the
/// ordinary ~30 ms save doesn't flash a modal.
pub(super) const SHOW_AFTER: f64 = 0.3;

/// What the window says about `operation`, and why Cancel is unavailable when it is.
pub(super) struct Shown {
    pub title: &'static str,
    pub step: &'static str,
    /// `None` while Cancel still leaves the file and the open project unchanged.
    pub cannot_cancel: Option<&'static str>,
}
pub(super) fn describe(operation: FileOperation) -> Shown {
    let title = match operation.kind {
        FileOperationKind::Save => "Saving project…",
        FileOperationKind::Open => "Opening project…",
    };
    let step = match (operation.kind, operation.phase) {
        _ if operation.cancel_requested => "Cancelling… the current step finishes first.",
        (FileOperationKind::Save, FilePhase::Preparing | FilePhase::Reading) => "Preparing the project's files…",
        (FileOperationKind::Save, FilePhase::Writing) => "Writing the project file…",
        (FileOperationKind::Save, FilePhase::Checkpointing) => "Saved. Writing the recovery copy…",
        (FileOperationKind::Open, FilePhase::Preparing | FilePhase::Reading | FilePhase::Writing) => "Reading and checking the project file…",
        (FileOperationKind::Open, FilePhase::Checkpointing) => "Writing the recovery copy…",
    };
    let cannot_cancel = if operation.cancel_requested {
        Some("Cancelling was already asked for.")
    } else if operation.kind == FileOperationKind::Save && operation.phase == FilePhase::Checkpointing {
        Some("The file is already saved; this last step can't be undone by cancelling.")
    } else {
        None
    };
    Shown { title, step, cannot_cancel }
}

impl NodeCraftApp {
    /// Draws the progress window while a background save or open runs. The caller clears this
    /// frame's input afterwards, so nothing behind the window reacts while the file is busy.
    pub(super) fn file_progress(&mut self, ctx: &egui::Context) {
        let Some(operation) = self.session.file_status().operation else {
            self.files.progress_since = None;
            return;
        };
        // Phases move on the worker without a frame: keep looking.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        let now = ctx.input(|i| i.time);
        let since = *self.files.progress_since.get_or_insert(now);
        if now - since < SHOW_AFTER {
            return;
        }
        let shown = describe(operation);
        let t = *self.theme.tokens();
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("file_progress367")).show(ctx, |ui| {
            ui.set_max_width(t.card_width);
            ui.heading(shown.title);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(shown.step);
            });
            cancel = ui
                .add_enabled(shown.cannot_cancel.is_none(), egui::Button::new("Cancel"))
                .on_hover_text("Stop, keeping the file and the open project as they were")
                .on_disabled_hover_text(shown.cannot_cancel.unwrap_or_default())
                .clicked();
        });
        if cancel {
            match self.session.dispatch("file.cancel", &json!({})) {
                Ok(_) => ctx.request_repaint(),
                Err(error) => self.notice = Some(error.message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_offered_exactly_while_it_changes_nothing() {
        let op = |kind, phase, cancel_requested| describe(FileOperation { kind, phase, cancel_requested });
        for phase in [FilePhase::Preparing, FilePhase::Writing] {
            assert!(op(FileOperationKind::Save, phase, false).cannot_cancel.is_none());
        }
        // A save's file is already replaced by the time it checkpoints.
        let late = op(FileOperationKind::Save, FilePhase::Checkpointing, false);
        assert!(late.cannot_cancel.is_some() && late.step.starts_with("Saved."));
        // An open is still undone by cancelling until its checkpoint is published.
        for phase in [FilePhase::Reading, FilePhase::Checkpointing] {
            assert!(op(FileOperationKind::Open, phase, false).cannot_cancel.is_none());
        }
        let asked = op(FileOperationKind::Open, FilePhase::Reading, true);
        assert!(asked.cannot_cancel.is_some() && asked.step.starts_with("Cancelling"));
    }
}
