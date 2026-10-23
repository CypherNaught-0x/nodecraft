//! The ComfyUI server panel in the desktop app (#9): where the server is, and a check that reads
//! its nodes and health with `comfy.nodes.refresh`.
//!
//! The address is kept in settings (`[providers] comfyui_url`). Saving a new one drops the
//! adapters built for the old server and the node list read from it, then checks the new one.
use super::NodeCraftApp;
use crate::theme::Tokens;
use nodecraft_engine::comfy_workflow_cmds;
use nodecraft_engine::secrets::Account;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;

/// The address a local ComfyUI listens on.
const HINT: &str = "http://127.0.0.1:8188";

#[derive(Default)]
pub(super) struct ComfyServerPanel {
    pub(super) open: bool,
    /// The address being edited; filled from settings when the panel opens.
    pub(super) url: String,
    loaded: bool,
}

impl NodeCraftApp {
    /// The toolbar button that opens the panel, where the server's nodes can be read.
    pub(super) fn comfy_server_button(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        if self.comfy_nodes.is_some()
            && ui
                .add(egui::Button::new(egui::RichText::new("ComfyUI").color(t.text_muted)).frame(false))
                .on_hover_text("Set the ComfyUI server, check it and read its nodes")
                .clicked()
        {
            self.comfy_server.open = !self.comfy_server.open;
        }
    }

    pub(super) fn comfy_server_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.comfy_server.open {
            self.comfy_server.loaded = false;
            return;
        }
        let saved = self.provider_settings().comfyui_url.unwrap_or_default();
        if !self.comfy_server.loaded {
            self.comfy_server.url = saved.clone();
            self.comfy_server.loaded = true;
        }
        let waiting = self.comfy_nodes.as_ref().is_some_and(|fetcher| fetcher.is_waiting());
        let edited = self.comfy_server.url.trim() != saved.trim();
        let (report, nodes) = comfy_workflow_cmds::last(&self.session);
        let (report, nodes) = (report.cloned(), nodes);
        let mut open = true;
        let mut check = false;
        let anchor = [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap];
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("ComfyUI server")
            .id(egui::Id::new("nodecraft.comfy_server"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, anchor)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_max_width(t.search_width);
                ui.label(egui::RichText::new("ComfyUI models run on this server, and workflows are imported against its nodes.").color(t.text_muted));
                ui.horizontal(|ui| {
                    let label = ui.label("Address");
                    ui.add(egui::TextEdit::singleline(&mut self.comfy_server.url).hint_text(HINT).desired_width(f32::INFINITY)).labelled_by(label.id);
                });
                let label = if edited {
                    "Save and check"
                } else if waiting {
                    "Checking…"
                } else {
                    "Check"
                };
                // A new address can be saved while the old one is still being checked.
                let can = edited || (!waiting && !saved.trim().is_empty());
                check = ui.add_enabled(can, egui::Button::new(label)).clicked();
                ui.separator();
                status(ui, t, report.as_ref(), nodes, saved.trim().is_empty());
                if let Ok(account) = Account::server("comfyui", saved.trim()) {
                    ui.label(
                        egui::RichText::new(format!("A server that asks for a token: add it under API keys as {}.", account.as_str())).color(t.text_muted),
                    );
                }
            });
        self.comfy_server.open &= open;
        if check {
            let typed = self.comfy_server.url.clone();
            if let Err(error) = self.check_comfy_server(&typed) {
                self.notice = Some(error.message);
            }
        }
    }

    /// Saves `typed` as the server when it changed, then reads the server's nodes and health.
    pub(super) fn check_comfy_server(&mut self, typed: &str) -> Result<(), Error> {
        let url = comfy_workflow_cmds::check_url(typed)?;
        let mut settings = self.provider_settings();
        if url != settings.comfyui_url.as_deref().map(str::trim).filter(|saved| !saved.is_empty()).map(str::to_owned) {
            let Some(store) = &self.store else {
                return Err(Error::new(ErrorKind::Unsupported, "settings can't be saved here, so the ComfyUI server can't be changed"));
            };
            if store.is_read_only() {
                return Err(Error::new(ErrorKind::Unsupported, "the settings file is read-only, so the ComfyUI server can't be changed"));
            }
            settings.comfyui_url = url.clone();
            // Forgotten before the save, so a failure here leaves the old address to retry from;
            // a card waiting for its adapter is built for the new server.
            if let Some(runs) = &mut self.runs {
                runs.forget_adapters_for(&mut self.session, &settings)?;
            }
            if let Some(store) = &mut self.store {
                store.update(|saved| saved.providers.comfyui_url = url.clone());
            }
            // A read still running is the old server's: its answer is dropped.
            if let Some(fetcher) = &mut self.comfy_nodes {
                fetcher.abandon();
            }
            comfy_workflow_cmds::forget(&mut self.session);
        }
        self.comfy_server.url = url.clone().unwrap_or_default();
        if url.is_none() {
            self.notice = Some("ComfyUI isn't set up: no server address.".into());
            return Ok(());
        }
        self.session.dispatch("comfy.nodes.refresh", &json!({})).map(|_| ())
    }
}

/// What the last check found.
fn status(ui: &mut egui::Ui, t: &Tokens, report: Option<&comfy_workflow_cmds::Report>, nodes: Option<usize>, unset: bool) {
    match report {
        _ if unset => {
            ui.label(egui::RichText::new("No server yet. Add its address, such as a local ComfyUI's, then check it.").color(t.text_muted));
        }
        None => {
            ui.label(egui::RichText::new("Not checked yet.").color(t.text_muted));
        }
        Some(report) => {
            if let Some(error) = &report.error {
                ui.label(egui::RichText::new(format!("Check failed: {error}")).color(t.danger));
            } else if let Some(health) = &report.server {
                ui.label(health.summary());
            } else {
                ui.label(egui::RichText::new("Reached; it didn't report its version or devices.").color(t.text_muted));
            }
            if let Some(nodes) = nodes {
                ui.label(
                    egui::RichText::new(format!("{nodes} node type{} read; workflows can be imported.", if nodes == 1 { "" } else { "s" })).color(t.text_muted),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
