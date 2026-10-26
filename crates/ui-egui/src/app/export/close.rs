//! Hold a native close request until the user chooses and the export releases its files.
use super::NodeCraftApp;
use nodecraft_engine::export_job::{ExportPhase, ExportWatch};
use std::time::Duration;

#[derive(Default)]
pub(super) enum Closing {
    #[default]
    Idle,
    Asking,
    Waiting(ExportWatch),
}

impl NodeCraftApp {
    pub(in crate::app) fn export_close_owns_input(&self) -> bool {
        !matches!(self.exports.closing, Closing::Idle)
    }

    /// Runs in logic, including minimized frames, before craft-quit can terminate the app.
    pub(in crate::app) fn export_close_request(&mut self, ctx: &egui::Context) {
        if ctx.viewport_id() != egui::ViewportId::ROOT {
            return;
        }
        let requested = ctx.input(|input| input.viewport().close_requested());
        if let Closing::Waiting(watch) = &self.exports.closing {
            if watch.status().result.is_some() {
                self.exports.closing = Closing::Idle;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                if requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            return;
        }
        let busy = self.exports.work.is_some() || self.session.export_status().is_some_and(|status| status.result.is_none());
        if matches!(self.exports.closing, Closing::Asking) && !busy {
            self.exports.closing = Closing::Idle;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if requested && busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.exports.closing = Closing::Asking;
            // A native Quit can arrive while hidden; make the decision reachable.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.request_repaint();
        }
    }

    fn cancel_export_and_close(&mut self, ctx: &egui::Context) {
        let watch = self.session.export_watch();
        if let Some(watch) = &watch {
            let status = watch.status();
            if status.result.is_none()
                && status.cancellable
                && let Err(error) = self.session.dispatch("export.cancel", &serde_json::json!({"id": status.id}))
                && watch.status().cancellable
            {
                self.notice = Some(error.message);
                return;
            }
        }
        // Do not start another numbered item or accept a later save-dialog answer.
        self.exports.pending = None;
        self.exports.work = None;
        if let Some(watch) = watch {
            self.exports.closing = Closing::Waiting(watch);
            self.export_close_request(ctx);
        } else {
            self.exports.closing = Closing::Idle;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(in crate::app) fn export_close_overlay(&mut self, ctx: &egui::Context) {
        let (asking, finishing) = match &self.exports.closing {
            Closing::Idle => return,
            Closing::Asking => (true, false),
            Closing::Waiting(watch) => (false, watch.status().phase == ExportPhase::Finishing),
        };
        let tokens = *self.theme.tokens();
        let mut cancel_and_quit = false;
        let mut stay = false;
        egui::Modal::new(egui::Id::new("export_close"))
            .frame(egui::Frame::NONE.fill(tokens.panel_fill).inner_margin(tokens.card_padding))
            .backdrop_color(tokens.canvas_bg)
            .show(ctx, |ui| {
                ui.heading(if asking {
                    "An export is still running."
                } else if finishing {
                    "Finishing export before quitting…"
                } else {
                    "Cancelling export before quitting…"
                });
                ui.label("Completed files will be kept.");
                if !asking {
                    ui.label("NodeCraft will quit after the export releases its files.");
                }
                ui.horizontal(|ui| {
                    if asking {
                        cancel_and_quit = ui.button("Cancel export and quit").clicked();
                    }
                    stay = ui.button(if asking { "Keep exporting" } else { "Keep app open" }).clicked();
                });
            });
        if stay {
            self.exports.closing = Closing::Idle;
        } else if cancel_and_quit {
            self.cancel_export_and_close(ctx);
        }
    }
}
