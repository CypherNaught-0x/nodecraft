//! The ComfyUI servers panel in the desktop app (#9, #864): every server with its address, how
//! many of a run's jobs it takes at once and its health, one of them the default; a check reads a
//! server's nodes and health with `comfy.nodes.refresh {server}`.
//!
//! Servers are kept in settings (`[[providers.comfyui]]`, `comfyui_default`). Changing or
//! removing one drops only that server's adapter and what was read from it (the node list too
//! when it was the default), then checks it again.
use super::NodeCraftApp;
use crate::theme::Tokens;
use nodecraft_engine::secrets::Account;
use nodecraft_engine::settings::{ComfyServer, MAX_COMFY_SERVERS, MAX_SERVER_JOBS, ProviderSettings, check_server_name, server_name_for};
use nodecraft_engine::{comfy_server_cmds, comfy_workflow_cmds};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;

/// The address a local ComfyUI listens on.
const HINT: &str = "http://127.0.0.1:8188";

#[derive(Default)]
pub(super) struct ComfyServerPanel {
    pub(super) open: bool,
    /// Each saved server's fields being edited, filled from settings when the panel opens.
    rows: Vec<Row>,
    /// The server being added.
    pub(super) new_name: String,
    pub(super) new_url: String,
    loaded: bool,
}

/// One server's fields as edited.
#[derive(Clone, Default)]
struct Row {
    name: String,
    url: String,
    cloud: bool,
    /// Jobs at once, as typed; empty for the scheduler's default.
    jobs: String,
}

impl Row {
    fn of(server: &ComfyServer) -> Self {
        Self { name: server.name.clone(), url: server.url.clone(), cloud: server.is_cloud(), jobs: server.max_jobs.map(|n| n.to_string()).unwrap_or_default() }
    }
}

/// What a click in the panel asks for.
enum Asked {
    Save(Row),
    Check(String),
    Default(String),
    Remove(String),
    Add,
    AddCloud,
}

impl NodeCraftApp {
    pub(super) fn comfy_server_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.comfy_server.open {
            self.comfy_server.loaded = false;
            return;
        }
        let settings = self.provider_settings();
        let saved: Vec<Row> = settings.comfy_servers().into_iter().map(Row::of).collect();
        if !self.comfy_server.loaded {
            self.comfy_server.rows = saved.clone();
            self.comfy_server.loaded = true;
        }
        let default = settings.default_comfy().map(|server| server.name.clone());
        let waiting = self.comfy_nodes.as_ref().is_some_and(|fetcher| fetcher.is_waiting());
        let nodes = comfy_workflow_cmds::last(&self.session).1;
        let mut open = true;
        let mut asked = None;
        let anchor = [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap];
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("ComfyUI servers")
            .id(egui::Id::new("nodecraft.comfy_server"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, anchor)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_max_width(t.search_width);
                ui.label(
                    egui::RichText::new(
                        "ComfyUI models run on the default server unless a card picks another. Workflows are imported against the default's nodes.",
                    )
                    .color(t.text_muted),
                );
                if saved.is_empty() {
                    ui.label(egui::RichText::new("No server yet. Add one, such as a local ComfyUI, then check it.").color(t.text_muted));
                }
                let session = &self.session;
                for (row, kept) in self.comfy_server.rows.iter_mut().zip(&saved) {
                    let is_default = default.as_deref() == Some(row.name.as_str());
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&row.name).strong());
                        if is_default {
                            ui.label(egui::RichText::new("default").color(t.text_muted));
                        } else if ui.small_button("Make default").clicked() {
                            asked = Some(Asked::Default(row.name.clone()));
                        }
                        if ui.small_button("Remove").on_hover_text(format!("Remove {}", row.name)).clicked() {
                            asked = Some(Asked::Remove(row.name.clone()));
                        }
                    });
                    if row.cloud {
                        ui.label(egui::RichText::new("Comfy Cloud · hosted GPU").color(t.text_muted));
                        ui.label(&row.url);
                        ui.label(egui::RichText::new("GPU usage is billed. The amount is unknown before a run; finite budgets block it.").color(t.text_muted));
                    } else {
                        ui.horizontal(|ui| {
                            let label = ui.label("Address");
                            ui.add(egui::TextEdit::singleline(&mut row.url).hint_text(HINT).desired_width(f32::INFINITY)).labelled_by(label.id);
                        });
                    }
                    ui.horizontal(|ui| {
                        let label = ui.label("Jobs at once");
                        ui.add(egui::TextEdit::singleline(&mut row.jobs).hint_text("4").desired_width(t.comfy_jobs_width))
                            .labelled_by(label.id)
                            .on_hover_text("How many of a run's jobs this server takes at once; one for a single GPU");
                        let edited = row.url.trim() != kept.url.trim() || row.jobs.trim() != kept.jobs.trim();
                        let label = if edited {
                            "Save and check"
                        } else if waiting {
                            "Checking…"
                        } else {
                            "Check"
                        };
                        if ui.add_enabled(edited || !waiting, egui::Button::new(label)).on_hover_text(format!("Check {}", row.name)).clicked() {
                            asked = Some(if edited { Asked::Save(row.clone()) } else { Asked::Check(row.name.clone()) });
                        }
                    });
                    let report = comfy_server_cmds::report(session, &row.name);
                    status(ui, t, report, is_default.then_some(nodes).flatten());
                    if kept.cloud {
                        ui.label(egui::RichText::new("Add your Comfy API key in API keys, comfy-cloud.").color(t.text_muted));
                    } else if let Ok(account) = Account::server("comfyui", kept.url.trim()) {
                        ui.label(egui::RichText::new(format!("A token, if it asks for one: API keys, {}.", account.as_str())).color(t.text_muted));
                    }
                }
                ui.separator();
                if saved.len() < MAX_COMFY_SERVERS {
                    ui.horizontal(|ui| {
                        let label = ui.label("New server");
                        ui.add(egui::TextEdit::singleline(&mut self.comfy_server.new_url).hint_text(HINT).desired_width(f32::INFINITY)).labelled_by(label.id);
                    });
                    ui.horizontal(|ui| {
                        let label = ui.label("Name");
                        let hint = server_name_for(&self.comfy_server.new_url);
                        ui.add(egui::TextEdit::singleline(&mut self.comfy_server.new_name).hint_text(hint).desired_width(t.comfy_name_width))
                            .labelled_by(label.id);
                        if ui.add_enabled(!self.comfy_server.new_url.trim().is_empty(), egui::Button::new("Add and check")).clicked() {
                            asked = Some(Asked::Add);
                        }
                    });
                    if ui
                        .button("Add Comfy Cloud")
                        .on_hover_text("Hosted workflows with the comfy-cloud key; GPU usage has an unknown price before running")
                        .clicked()
                    {
                        asked = Some(Asked::AddCloud);
                    }
                }
            });
        self.comfy_server.open &= open;
        let done = match asked {
            Some(Asked::Save(row)) => self.save_comfy_server(&row.name, &row.url, &row.jobs),
            Some(Asked::Check(name)) => self.check_comfy_server(&name),
            Some(Asked::Default(name)) => self.default_comfy_server(&name),
            Some(Asked::Remove(name)) => self.remove_comfy_server(&name),
            Some(Asked::Add) => {
                let url = self.comfy_server.new_url.clone();
                let typed = self.comfy_server.new_name.trim().to_owned();
                let name = if typed.is_empty() { server_name_for(&url) } else { typed };
                self.add_comfy_server(&name, &url, "")
            }
            Some(Asked::AddCloud) => {
                let typed = self.comfy_server.new_name.trim();
                let name = if typed.is_empty() { "Comfy Cloud".to_owned() } else { typed.to_owned() };
                self.add_comfy_cloud(&name)
            }
            None => Ok(()),
        };
        if let Err(error) = done {
            self.notice = Some(error.message);
        }
    }

    /// Adds the server `name` at `url`, taking `jobs` at once (empty for the default), and checks
    /// it. The first server added is the default.
    pub(super) fn add_comfy_server(&mut self, name: &str, url: &str, jobs: &str) -> Result<(), Error> {
        self.add_comfy_profile(server_from(name, url, jobs)?)
    }

    pub(super) fn add_comfy_cloud(&mut self, name: &str) -> Result<(), Error> {
        check_server_name(name).map_err(|why| Error::new(ErrorKind::InvalidInput, why))?;
        self.add_comfy_profile(ComfyServer::cloud(name))
    }

    fn add_comfy_profile(&mut self, server: ComfyServer) -> Result<(), Error> {
        let name = server.name.clone();
        let name = name.as_str();
        let mut settings = self.provider_settings();
        if settings.comfy_server(name).is_some() || settings.comfyui.iter().any(|kept| kept.name == name) {
            return Err(Error::new(ErrorKind::Conflict, format!("there's already a ComfyUI server called “{name}”")));
        }
        if settings.comfyui.len() >= MAX_COMFY_SERVERS {
            return Err(Error::new(ErrorKind::Budget, format!("NodeCraft keeps at most {MAX_COMFY_SERVERS} ComfyUI servers")));
        }
        let was_default = settings.default_comfy().map(|server| server.name.clone());
        settings.comfyui.push(server);
        if was_default.is_none() {
            settings.comfyui_default = Some(name.to_owned());
        }
        self.keep_comfy_servers(&settings, name, false)?;
        self.comfy_server.new_name.clear();
        self.comfy_server.new_url.clear();
        self.followup_comfy_server(Some(name))
    }

    /// Saves a changed address or job cap for the server `name`, then checks it. Only its own
    /// adapter and what was read from it are dropped.
    pub(super) fn save_comfy_server(&mut self, name: &str, url: &str, jobs: &str) -> Result<(), Error> {
        let mut server = server_from(name, url, jobs)?;
        let mut settings = self.provider_settings();
        let was_default = settings.default_comfy().is_some_and(|kept| kept.name == name);
        let Some(kept) = settings.comfyui.iter_mut().find(|kept| kept.name == name) else {
            return Err(Error::new(ErrorKind::NotFound, format!("there's no ComfyUI server called “{name}”")));
        };
        if kept.is_cloud() {
            if url.trim() != nodecraft_providers::comfy_cloud::ORIGIN {
                return Err(Error::new(ErrorKind::InvalidInput, "the Comfy Cloud profile has a fixed address"));
            }
            server.backend = nodecraft_engine::settings::ComfyBackend::Cloud;
        }
        *kept = server;
        self.keep_comfy_servers(&settings, name, was_default)?;
        self.followup_comfy_server(Some(name))
    }

    /// Makes `name` the default server, and reads its nodes, which workflows are imported against.
    pub(super) fn default_comfy_server(&mut self, name: &str) -> Result<(), Error> {
        let mut settings = self.provider_settings();
        if settings.comfy_server(name).is_none() {
            return Err(Error::new(ErrorKind::NotFound, format!("there's no ComfyUI server called “{name}”")));
        }
        settings.comfyui_default = Some(name.to_owned());
        self.save_providers(&settings)?;
        // Cards that follow the default are routed again, a waiting one included.
        if let Some(runs) = &mut self.runs {
            runs.forget_comfy_server(&mut self.session, name, &settings)?;
        }
        // The node list was the old default's; its health stays in its row.
        comfy_workflow_cmds::forget(&mut self.session);
        if let Some(fetcher) = &mut self.comfy_nodes {
            fetcher.abandon();
        }
        self.followup_comfy_server(Some(name))
    }

    /// Removes the server `name`; cards that picked it follow the default.
    pub(super) fn remove_comfy_server(&mut self, name: &str) -> Result<(), Error> {
        let mut settings = self.provider_settings();
        let was_default = settings.default_comfy().is_some_and(|kept| kept.name == name);
        settings.comfyui.retain(|kept| kept.name != name);
        if was_default {
            settings.comfyui_default = settings.comfy_servers().first().map(|server| server.name.clone());
        }
        self.keep_comfy_servers(&settings, name, was_default)?;
        if was_default && settings.default_comfy().is_some() {
            return self.followup_comfy_server(None);
        }
        Ok(())
    }

    /// Reads the nodes and health of the server `name`.
    pub(super) fn check_comfy_server(&mut self, name: &str) -> Result<(), Error> {
        self.session.dispatch("comfy.nodes.refresh", &json!({"server": name})).map(|_| ())
    }

    /// Queues a panel mutation's follow-up behind another server's running check.
    fn followup_comfy_server(&mut self, server: Option<&str>) -> Result<(), Error> {
        if let Some(fetcher) = &mut self.comfy_nodes {
            fetcher.allow_followup(true);
        }
        let result = self.session.dispatch("comfy.nodes.refresh", &json!({"server": server})).map(|_| ());
        if let Some(fetcher) = &mut self.comfy_nodes {
            fetcher.allow_followup(false);
        }
        result
    }

    /// Saves `settings`, in which the server `changed` was added, edited or removed, and drops
    /// that server's adapter and what was read from it.
    fn keep_comfy_servers(&mut self, settings: &ProviderSettings, changed: &str, was_default: bool) -> Result<(), Error> {
        self.save_providers(settings)?;
        if let Some(runs) = &mut self.runs {
            runs.forget_comfy_server(&mut self.session, changed, settings)?;
        }
        // A read of the old server still running is dropped; another server's carries on.
        if let Some(fetcher) = &mut self.comfy_nodes {
            fetcher.abandon_for(changed, was_default);
        }
        comfy_server_cmds::forget(&mut self.session, changed, was_default);
        if self.comfy_server.loaded {
            // Reconcile only the saved/removed row: drafts in other rows are still the person's
            // edits, even when settings changed after an unrelated panel action.
            let mut rows = std::mem::take(&mut self.comfy_server.rows);
            self.comfy_server.rows = settings
                .comfy_servers()
                .into_iter()
                .map(|server| {
                    if server.name == changed {
                        Row::of(server)
                    } else if let Some(position) = rows.iter().position(|row| row.name == server.name) {
                        rows.remove(position)
                    } else {
                        Row::of(server)
                    }
                })
                .collect();
        }
        Ok(())
    }

    /// Writes the ComfyUI servers of `settings` to the settings file.
    fn save_providers(&mut self, settings: &ProviderSettings) -> Result<(), Error> {
        let Some(store) = &mut self.store else {
            return Err(Error::new(ErrorKind::Unsupported, "settings can't be saved here, so ComfyUI servers can't be changed"));
        };
        if store.is_read_only() {
            return Err(Error::new(ErrorKind::Unsupported, "the settings file is read-only, so ComfyUI servers can't be changed"));
        }
        store.update(|saved| {
            saved.providers.comfyui = settings.comfyui.clone();
            saved.providers.comfyui_default = settings.comfyui_default.clone();
        });
        Ok(())
    }
}

/// The ComfyUI servers a card menu offers: every server's name, and the default's.
pub(super) struct ServerChoices {
    names: Vec<String>,
    default: Option<String>,
}

impl ServerChoices {
    pub(super) fn of(settings: &ProviderSettings) -> Self {
        Self {
            names: settings.comfy_servers().into_iter().map(|server| server.name.clone()).collect(),
            default: settings.default_comfy().map(|server| server.name.clone()),
        }
    }
}

/// The "ComfyUI server" submenu of a card whose model runs on ComfyUI, when there's a server to
/// pick: returns the choice clicked, a server's name or `None` to follow the default.
pub(super) fn card_menu(
    ui: &mut egui::Ui,
    node: &nodecraft_graph::Node,
    catalog: Option<&nodecraft_engine::generation_catalog::GenerationCatalog>,
    choices: &ServerChoices,
) -> Option<Option<String>> {
    if choices.names.is_empty() || !catalog.is_some_and(|catalog| comfy_server_cmds::runs_on_comfy(catalog, node)) {
        return None;
    }
    let picked = match node.config.get(nodecraft_engine::provider_setup::COMFY_SERVER_CONFIG) {
        Some(nodecraft_types::Value::Text(name)) if choices.names.contains(name) => Some(name.as_str()),
        _ => None,
    };
    let mut chosen = None;
    ui.menu_button("ComfyUI server", |ui| {
        let default = choices.default.as_deref().map_or_else(|| "Default server".to_owned(), |name| format!("Default ({name})"));
        if ui.radio(picked.is_none(), default).on_hover_text("Run on whichever server is the default").clicked() {
            chosen = Some(None);
            ui.close();
        }
        for name in &choices.names {
            if ui.radio(picked == Some(name.as_str()), name).clicked() {
                chosen = Some(Some(name.clone()));
                ui.close();
            }
        }
    });
    chosen
}

impl NodeCraftApp {
    /// Runs the card `id` on the ComfyUI server `server`, or the default with `None`.
    pub(super) fn use_comfy_server(&mut self, id: nodecraft_types::NodeId, server: Option<String>) -> Result<(), Error> {
        self.session.dispatch("comfy.server.use", &json!({"canvas": self.canvas, "id": id, "server": server})).map(|_| ())
    }
}

/// A server from what was typed: a good name, an address, and a job cap that's empty or 1–16.
fn server_from(name: &str, url: &str, jobs: &str) -> Result<ComfyServer, Error> {
    check_server_name(name).map_err(|why| Error::new(ErrorKind::InvalidInput, why))?;
    let Some(url) = comfy_workflow_cmds::check_url(url)? else {
        return Err(Error::new(ErrorKind::InvalidInput, "a ComfyUI server needs an address"));
    };
    let max_jobs = match jobs.trim() {
        "" => None,
        typed => match typed.parse::<u32>() {
            Ok(n) if (1..=MAX_SERVER_JOBS).contains(&n) => Some(n),
            _ => return Err(Error::new(ErrorKind::InvalidInput, format!("jobs at once is a whole number from 1 to {MAX_SERVER_JOBS}"))),
        },
    };
    Ok(ComfyServer { max_jobs, ..ComfyServer::new(name, &url) })
}

/// What the last check of a server found; `nodes` is the default's node list.
fn status(ui: &mut egui::Ui, t: &Tokens, report: Option<&comfy_workflow_cmds::Report>, nodes: Option<usize>) {
    let Some(report) = report else {
        ui.label(egui::RichText::new("Not checked yet.").color(t.text_muted));
        return;
    };
    if let Some(error) = &report.error {
        ui.label(egui::RichText::new(format!("Check failed: {error}")).color(t.danger));
    } else if let Some(health) = &report.server {
        ui.label(health.summary());
    } else {
        ui.label(egui::RichText::new("Reached; it didn't report its version or devices.").color(t.text_muted));
    }
    if let Some(nodes) = nodes {
        ui.label(egui::RichText::new(format!("{nodes} node type{} read; workflows can be imported.", if nodes == 1 { "" } else { "s" })).color(t.text_muted));
    }
}

#[cfg(test)]
mod tests;
