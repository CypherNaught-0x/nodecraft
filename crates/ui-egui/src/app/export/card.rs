//! Export to folder cards (#746): the human-only folder grant and the Export button.
//!
//! *Choose folder…* on the card opens the native folder dialog; the folder a person picks
//! becomes an [`ExportHost`] held here, as view state of this window, for that card only. It is
//! never written to the project, settings or autosave, and no command can set it, so an agent,
//! the CLI or MCP cannot grant a folder. *Export* dispatches `export.node_run` with that grant on
//! the shared background export worker; the progress window's Cancel calls `export.cancel`.
use super::{NodeCraftApp, Target};
use nodecraft_engine::{
    export_job::ExportResult,
    local_export::{ExportHost, dispatch_with_host},
};
use nodecraft_types::{CanvasId, NodeId};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

/// The folders people chose for cards in this window, for the project they were chosen in.
#[derive(Default)]
pub(in crate::app) struct Grants {
    session: Option<Arc<()>>,
    hosts: BTreeMap<(CanvasId, NodeId), ExportHost>,
}

/// Why this build cannot write a card's files into a folder, if it cannot.
pub(crate) fn unavailable() -> Option<&'static str> {
    if cfg!(target_arch = "wasm32") {
        Some("Exporting to a folder needs the desktop app.")
    } else if !cfg!(unix) {
        Some("Exporting to a folder is available on macOS and Linux; save results one at a time with Save as here.")
    } else {
        None
    }
}

impl NodeCraftApp {
    fn grant(&self, canvas: CanvasId, id: NodeId) -> Option<&ExportHost> {
        let grants = &self.exports.grants;
        grants.session.as_ref().filter(|session| Arc::ptr_eq(session, &self.session.view_identity()))?;
        grants.hosts.get(&(canvas, id))
    }

    /// The name of the folder chosen for card `id`, for the card to show.
    pub(in crate::app) fn card_export_folder(&self, id: NodeId) -> Option<String> {
        self.grant(self.canvas, id).map(ExportHost::label)
    }

    /// The card's *Choose folder…*: a person picks the folder; nothing else can.
    pub(in crate::app) fn choose_card_folder(&mut self, id: NodeId) {
        if let Some(why) = unavailable() {
            self.notices.error(why);
            return;
        }
        self.pick_export(Target::Card(self.canvas, id), "");
    }

    pub(super) fn card_folder_chosen(&mut self, canvas: CanvasId, id: NodeId, path: PathBuf) {
        match ExportHost::authorize(&path) {
            Ok(host) => {
                let identity = self.session.view_identity();
                let grants = &mut self.exports.grants;
                if !grants.session.as_ref().is_some_and(|session| Arc::ptr_eq(session, &identity)) {
                    // Another project: grants for the last one's cards go with it.
                    grants.hosts.clear();
                    grants.session = Some(identity);
                }
                if grants.hosts.len() >= MAX_GRANTS {
                    // Cards that were deleted give their grants back first.
                    let document = self.session.document();
                    grants.hosts.retain(|(canvas, node), _| document.canvases().get(canvas).is_some_and(|c| c.nodes().contains_key(node)));
                }
                if grants.hosts.len() >= MAX_GRANTS && !grants.hosts.contains_key(&(canvas, id)) {
                    self.notices.error("Too many Export to folder cards have a folder chosen; reopen the project to start over.");
                    return;
                }
                grants.hosts.insert((canvas, id), host);
            }
            Err(error) => self.notices.report(error),
        }
    }

    /// The card's *Export*: previews what the card describes and, when that would replace files
    /// already in the folder, asks first; then writes it into its chosen folder, off the UI
    /// thread. The card's parameters can be changed by anyone who edits the project (an agent
    /// included), so a replacement is always the person's explicit yes here.
    pub(in crate::app) fn export_card(&mut self, id: NodeId) {
        let Some(host) = self.export_ready(id) else {
            return;
        };
        let plan = json!({"canvas": self.canvas, "node": id});
        let preview = match dispatch_with_host(&mut self.session, &host, "export.node_plan", &plan) {
            Ok(preview) => preview,
            Err(error) => {
                self.notices.report(error);
                return;
            }
        };
        let replacing = preview["on_collision"] == "overwrite";
        match preview["existing"].as_u64() {
            Some(0) => self.write_card(id, host),
            Some(count) if replacing => self.exports.confirm = Some((self.session.view_identity(), self.canvas, id, count)),
            Some(_) => self.write_card(id, host),
            // Unknown: never replace without knowing what would be replaced.
            None if replacing => self.notices.error("Can't check which files would be replaced; choose Keep or Number on the card."),
            None => self.write_card(id, host),
        }
    }

    /// Why card `id` can't export right now (in a notice), or its grant.
    fn export_ready(&mut self, id: NodeId) -> Option<ExportHost> {
        if let Some(why) = unavailable() {
            self.notices.error(why);
            return None;
        }
        if self.exports.pending.is_some()
            || self.exports.work.is_some()
            || self.exports.results.is_some()
            || self.exports.card.is_some()
            || self.exports.confirm.is_some()
        {
            self.notices.warning("Another export is in progress; finish or cancel that first.");
            return None;
        }
        let Some(host) = self.grant(self.canvas, id).cloned() else {
            self.notices.info("Choose a folder on the card first.");
            return None;
        };
        Some(host)
    }

    fn write_card(&mut self, id: NodeId, host: ExportHost) {
        if let Some(runner) = &self.exports.runner {
            self.session.set_export_runner(Arc::clone(runner));
        }
        let params = json!({"canvas": self.canvas, "node": id, "background": true});
        match dispatch_with_host(&mut self.session, &host, "export.node_run", &params) {
            Ok(_) => match self.session.export_watch() {
                Some(watch) => self.exports.card = Some((watch, host.label())),
                None => self.notices.error("The export didn't start; export again."),
            },
            Err(error) => self.notices.report(error),
        }
    }

    /// The progress window of a card export, with Cancel; `true` while it owns input.
    pub(in crate::app) fn card_export_overlay(&mut self, ctx: &egui::Context) -> bool {
        if self.exports.confirm.is_some() {
            self.confirm_overlay(ctx);
            return true;
        }
        let Some((watch, folder)) = self.exports.card.take() else {
            return false;
        };
        let status = watch.status();
        if let Some(result) = status.result {
            self.notices.success(summary(&result, &folder));
            return false;
        }
        let t = *self.theme.tokens();
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("card_export746"))
            .frame(egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding))
            .backdrop_color(t.canvas_bg)
            .show(ctx, |ui| {
                ui.set_max_width(t.card_width * 2.0);
                ui.heading(format!("Exporting to {folder}…"));
                match status.progress {
                    Some(progress) => {
                        ui.label(format!("{} of {} files • {} written • {} failed", progress.completed, progress.total, progress.written, progress.failed));
                        let ratio = if progress.total == 0 { 0.0 } else { progress.completed as f32 / progress.total as f32 };
                        ui.add(egui::ProgressBar::new(ratio));
                    }
                    None => {
                        ui.label("Waiting for the background worker…");
                    }
                }
                ui.label("Cancel stops before the next file; files already written are kept.");
                let current = self.session.export_watch().is_some_and(|current| current.same_job(&watch));
                cancel = ui.add_enabled(status.cancellable && current, egui::Button::new("Cancel export")).clicked();
            });
        if cancel && let Err(error) = self.session.dispatch("export.cancel", &json!({"id": status.id})) {
            self.notices.report(error);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        self.exports.card = Some((watch, folder));
        true
    }
}

impl NodeCraftApp {
    /// Asks before a card export replaces files already in its folder.
    fn confirm_overlay(&mut self, ctx: &egui::Context) {
        let Some((session, canvas, id, count)) = self.exports.confirm.clone() else {
            return;
        };
        let folder = self.grant(canvas, id).map(ExportHost::label);
        let t = *self.theme.tokens();
        let (mut replace, mut cancel) = (false, false);
        egui::Modal::new(egui::Id::new("card_export746_confirm"))
            .frame(egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding))
            .backdrop_color(t.canvas_bg)
            .show(ctx, |ui| {
                ui.set_max_width(t.card_width * 2.0);
                ui.heading("Replace existing files?");
                let files = if count == 1 { "1 file".to_owned() } else { format!("{count} files") };
                ui.label(format!("{files} in {} will be replaced by this export.", folder.as_deref().unwrap_or("the chosen folder")));
                ui.label(
                    egui::RichText::new("The card is set to replace existing files. Choose Keep or Number on the card to leave them.").color(t.text_muted),
                );
                ui.horizontal(|ui| {
                    replace = ui.button("Replace files").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if cancel {
            self.exports.confirm = None;
        } else if replace {
            self.exports.confirm = None;
            // The project or the grant may have gone while the question was open.
            if !Arc::ptr_eq(&session, &self.session.view_identity()) || canvas != self.canvas {
                self.notices.warning("The project changed; nothing was exported.");
            } else if let Some(host) = self.export_ready(id) {
                self.write_card(id, host);
            }
        }
    }
}

/// Most cards with a folder chosen at once: each grant holds an open folder descriptor.
const MAX_GRANTS: usize = 64;

fn count(outcome: &Value, state: &str) -> u64 {
    outcome["counts"][state].as_u64().unwrap_or(0)
}

/// The notice a finished card export leaves: what was written, kept, failed or not reached, and
/// the first reason a file was not written.
fn summary(result: &ExportResult, folder: &str) -> String {
    match result {
        ExportResult::Batch { outcome } => {
            let (written, skipped, failed, left) =
                (count(outcome, "written"), count(outcome, "skipped"), count(outcome, "failed"), count(outcome, "not_started"));
            let mut text = format!("Exported {written} file{} to {folder}.", if written == 1 { "" } else { "s" });
            if skipped > 0 {
                text.push_str(&format!(" {skipped} kept: a file with that name already exists."));
            }
            if left > 0 {
                text.push_str(&format!(" Cancelled; {left} not started."));
            }
            if failed > 0 {
                let reason = outcome["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|entry| entry["state"] == "failed")
                    .and_then(|entry| entry["reason"].as_str())
                    .unwrap_or("see the export result");
                let reason: String = reason.chars().take(200).collect();
                text.push_str(&format!(" {failed} failed: {reason}."));
            }
            text
        }
        ExportResult::Cancelled => "Export cancelled; files already written were kept.".into(),
        ExportResult::Failed { error, .. } => error.message.clone(),
        ExportResult::Exported { .. } | ExportResult::Results { .. } => format!("Exported to {folder}."),
    }
}
