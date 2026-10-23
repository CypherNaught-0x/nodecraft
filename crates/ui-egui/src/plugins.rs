//! The plugins panel (#172): the packages in the local store, and every change to them.
//!
//! Every action is a `plugins.*` command dispatched through the app's session. The desktop's
//! `plugins` service runs on a worker (`engine::plugin_cmds::PluginWorker`), so commands answer
//! `{pending: id}`; the panel asks `plugins.result` each frame until the answer is in, then
//! acknowledges it with `plugins.forget`. A host that answers at once (tests) is handled too.
//!
//! Nothing is installed or granted before the person has seen what it is. "Check" runs a dry run
//! and shows who signed the package, its licence and notice text, the IDs it registers and the
//! rights it asks for; only "Install" (or "Update") then imports it, disabled. Granting shows the
//! capabilities and origins being agreed to, and required ones can't be left out.

use std::collections::{BTreeMap, BTreeSet};

use nodecraft_engine::Session;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

use crate::theme::Tokens;

/// Most packages the panel lists, and most items it shows in one list.
const MAX_ROWS: usize = 256;
const MAX_ITEMS: usize = 64;

/// A package as `plugins.list` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub package_id: String,
    pub version: String,
    pub digest: String,
    pub enabled: bool,
    pub license: String,
    pub signed_by: Option<String>,
    /// Other versions kept, by digest, that it can roll back to.
    pub kept: Vec<String>,
    /// The live grant's ID and the capabilities it covers.
    pub grant: Option<(String, Vec<String>)>,
}

impl Row {
    fn parse(v: &Value) -> Option<Self> {
        let text = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        let grant = v.get("grant").filter(|g| !g.is_null()).and_then(|g| Some((g.get("grant_id")?.as_str()?.to_owned(), strings(g.get("capabilities")))));
        Some(Self {
            package_id: text("package_id")?,
            version: text("version").unwrap_or_default(),
            digest: text("digest")?,
            enabled: v.get("state").and_then(Value::as_str) == Some("enabled"),
            license: text("license").unwrap_or_default(),
            signed_by: text("signed_by"),
            kept: strings(v.get("kept")),
            grant,
        })
    }
}

/// A dry run's report, waiting for the person to install it or not.
#[derive(Debug, Clone)]
pub struct Preview {
    pub update: bool,
    pub location: String,
    pub report: Value,
}

/// Rights about to be granted to a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consent {
    pub package_id: String,
    pub digest: String,
    pub required: Vec<String>,
    pub optional: Vec<String>,
    pub origins: Vec<String>,
    /// The optional capabilities ticked.
    pub chosen: BTreeSet<String>,
}

/// What a request was for, to know what its answer means.
#[derive(Debug, Clone)]
enum Ask {
    List,
    Check {
        update: bool,
        location: String,
    },
    Install {
        digest: String,
        update: bool,
        /// The package had a grant, which an update drops.
        granted: bool,
    },
    Rights,
    /// A change to an installed package; the text says what was done.
    Change(String),
}

#[derive(Default)]
pub struct PluginsPanel {
    pub open: bool,
    rows: Vec<Row>,
    /// Whether the list was asked for since the panel opened.
    listed: bool,
    /// Requests the worker hasn't answered, by pending ID.
    pending: BTreeMap<String, Ask>,
    /// The package file typed or chosen.
    pub location: String,
    /// Whether the file updates an installed package.
    pub update: bool,
    preview: Option<Preview>,
    consent: Option<Consent>,
    confirm_uninstall: Option<String>,
    /// The last failure, and the last thing done.
    pub notice: Option<String>,
    pub status: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    picking: Option<pick::Picking>,
}

/// The strings in a JSON array, at most [`MAX_ITEMS`].
fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).take(MAX_ITEMS).map(str::to_owned).collect()).unwrap_or_default()
}

/// A digest short enough to read: its first twelve hex digits.
pub fn short(digest: &str) -> &str {
    let hex = digest.strip_prefix("blake3:").unwrap_or(digest);
    hex.get(..12).unwrap_or(hex)
}

/// What a capability lets a package do, in words.
pub fn capability_text(capability: &str) -> String {
    match capability {
        "asset.read" => "read the project's media",
        "asset.write" => "add media to the project",
        "network" => "reach the internet, only at the addresses listed",
        "filesystem" => "read and write files on this computer",
        "process" => "start programs on this computer",
        "gpu" => "use the graphics card",
        "secret" => "use one of your API keys",
        // Named, so the person sees exactly what they would agree to.
        _ => return format!("something this NodeCraft doesn't know ({capability})"),
    }
    .to_owned()
}

impl PluginsPanel {
    /// The packages listed.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether a request is waiting for the worker.
    pub fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    pub fn consent(&self) -> Option<&Consent> {
        self.consent.as_ref()
    }

    /// Dispatches a `plugins.*` command and records the request or its immediate answer.
    fn send(&mut self, session: &mut Session, id: &str, params: Value, ask: Ask) {
        // A new request from the person replaces the last failure; a list refresh never does, so
        // the reason something was refused stays until they act again.
        if !matches!(ask, Ask::List) {
            self.notice = None;
        }
        match session.dispatch(id, &params) {
            Ok(answer) => match answer.get("pending").and_then(Value::as_str) {
                Some(pending) => {
                    self.pending.insert(pending.to_owned(), ask);
                }
                None => self.apply(session, ask, Ok(answer)),
            },
            Err(e) => self.notice = Some(e.message),
        }
    }

    fn list(&mut self, session: &mut Session) {
        self.listed = true;
        if !self.pending.values().any(|ask| matches!(ask, Ask::List)) {
            self.send(session, "plugins.list", json!({}), Ask::List);
        }
    }

    /// Asks the worker for the answers it has, applies them and acknowledges them.
    pub fn poll(&mut self, session: &mut Session) {
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        for id in ids {
            let outcome = match session.dispatch("plugins.result", &json!({"pending": id})) {
                Ok(reply) => match reply.get("status").and_then(Value::as_str) {
                    Some("completed") => Some(Ok(reply.get("result").cloned().unwrap_or(Value::Null))),
                    Some("failed") => {
                        let message = reply.pointer("/error/message").and_then(Value::as_str).unwrap_or("the plugin request failed");
                        Some(Err(Error::new(ErrorKind::Internal, message)))
                    }
                    _ => None,
                },
                // The worker is busy answering: ask again next frame.
                Err(e) if e.kind == ErrorKind::Busy => None,
                Err(e) => Some(Err(e)),
            };
            if let Some(result) = outcome
                && let Some(ask) = self.pending.remove(&id)
            {
                // A finished result that isn't acknowledged only takes a slot until it is evicted.
                let _ = session.dispatch("plugins.forget", &json!({"pending": id}));
                self.apply(session, ask, result);
            }
        }
    }

    /// Applies an answer.
    fn apply(&mut self, session: &mut Session, ask: Ask, result: Result<Value, Error>) {
        let answer = match result {
            Ok(answer) => answer,
            Err(e) => {
                self.notice = Some(e.message);
                self.status = None;
                if matches!(ask, Ask::Install { .. } | Ask::Change(_)) {
                    self.list(session);
                }
                return;
            }
        };
        match ask {
            Ask::List => {
                self.rows =
                    answer.get("packages").and_then(Value::as_array).map(|p| p.iter().take(MAX_ROWS).filter_map(Row::parse).collect()).unwrap_or_default();
                // A package changed elsewhere (another window, the CLI) isn't what the consent showed.
                if self.consent.as_ref().is_some_and(|c| !self.rows.iter().any(|r| r.package_id == c.package_id && r.digest == c.digest)) {
                    self.consent = None;
                }
            }
            Ask::Check { update, location } => {
                self.status = None;
                self.preview = Some(Preview { update, location, report: answer });
            }
            Ask::Install { digest, update, granted } => {
                let id = answer.get("package_id").and_then(Value::as_str).unwrap_or_default();
                let version = answer.get("version").and_then(Value::as_str).unwrap_or_default();
                self.status = Some(match (update, granted) {
                    (false, _) => format!("Installed {id} {version}. It stays disabled until you enable it."),
                    (true, false) => format!("Updated {id} to {version}. It stays disabled until you enable it."),
                    (true, true) => format!("Updated {id} to {version}. Its grant was removed: grant it again, then enable it."),
                });
                if answer.get("digest").and_then(Value::as_str) != Some(digest.as_str()) {
                    self.notice = Some("The file changed after it was checked: look at what was installed before you enable it.".into());
                }
                self.preview = None;
                // A consent shown for the digest this replaced no longer applies.
                self.consent = None;
                self.location.clear();
                self.list(session);
            }
            Ask::Rights => {
                if let Some(why) = answer.get("error").and_then(Value::as_str) {
                    self.notice = Some(format!("This package can't be granted: {why}"));
                    return;
                }
                let text = |key: &str| answer.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
                if answer.get("runs").and_then(Value::as_bool) == Some(false) {
                    self.status = Some(format!("{} is a data package: it runs nothing, so it needs no grant.", text("package_id")));
                    return;
                }
                self.consent = Some(Consent {
                    package_id: text("package_id"),
                    digest: text("digest"),
                    required: strings(answer.get("required")),
                    optional: strings(answer.get("optional")),
                    origins: strings(answer.get("origins")),
                    chosen: BTreeSet::new(),
                });
            }
            Ask::Change(done) => {
                let mut status = done;
                let dependents = strings(answer.get("dependents"));
                if !dependents.is_empty() {
                    status.push_str(&format!(" Also affected: {}.", dependents.join(", ")));
                }
                let unbound = strings(answer.get("unbound"));
                if !unbound.is_empty() {
                    status.push_str(&format!(" Not connected yet: {}.", unbound.join(", ")));
                }
                self.status = Some(status);
                self.list(session);
            }
        }
    }

    /// Runs a dry run of the typed file.
    pub fn check(&mut self, session: &mut Session) {
        let location = self.location.trim().to_owned();
        if location.is_empty() {
            return;
        }
        self.preview = None;
        let (id, update) = if self.update { ("plugins.update", true) } else { ("plugins.import", false) };
        self.send(session, id, json!({"location": location, "dry_run": true}), Ask::Check { update, location });
    }

    /// Installs what the preview showed.
    pub fn install(&mut self, session: &mut Session) {
        let Some(preview) = self.preview.clone() else { return };
        let digest = preview.report.get("digest").and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = if preview.update { "plugins.update" } else { "plugins.import" };
        let granted = preview.update && self.drops_grant(&preview);
        // The engine refuses a file that no longer has the digest the person looked at.
        self.send(session, id, json!({"location": preview.location, "digest": digest}), Ask::Install { digest, update: preview.update, granted });
    }

    /// Whether installing `preview` updates a package that is granted now; every update drops it.
    fn drops_grant(&self, preview: &Preview) -> bool {
        let id = preview.report.get("package_id").and_then(Value::as_str);
        preview.update && self.rows.iter().any(|r| Some(r.package_id.as_str()) == id && r.grant.is_some())
    }

    fn change(&mut self, session: &mut Session, id: &str, params: Value, done: String) {
        self.consent = None;
        self.confirm_uninstall = None;
        self.send(session, id, params, Ask::Change(done));
    }

    /// Asks what `package` would be granted, to show it before granting.
    pub fn ask_rights(&mut self, session: &mut Session, package: &str) {
        self.consent = None;
        self.send(session, "plugins.rights", json!({"package_id": package}), Ask::Rights);
    }

    /// Grants what the consent shows, with the optional capabilities ticked.
    pub fn grant(&mut self, session: &mut Session) {
        let Some(c) = self.consent.clone() else { return };
        let mut capabilities = c.required.clone();
        capabilities.extend(c.chosen.iter().filter(|cap| c.optional.contains(cap)).cloned());
        let mut params = json!({"package_id": c.package_id, "digest": c.digest, "capabilities": capabilities});
        if capabilities.iter().any(|cap| cap == "network")
            && let Some(o) = params.as_object_mut()
        {
            o.insert("origins".into(), json!(c.origins));
        }
        let enabled = self.rows.iter().any(|r| r.package_id == c.package_id && r.enabled);
        let done = if enabled { format!("Granted {}.", c.package_id) } else { format!("Granted {}; enable it to let it run.", c.package_id) };
        self.change(session, "plugins.grant", params, done);
    }

    /// Draws the panel when open.
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session, t: &Tokens) {
        self.poll(session);
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_pick(ctx);
        if !self.open {
            self.listed = false;
            self.confirm_uninstall = None;
            return;
        }
        let available = session.enabled("plugins.list", &json!({})).is_ok();
        if available && !self.listed {
            self.list(session);
        }
        let mut open = self.open;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Plugins").id(egui::Id::new("nodecraft.plugins")).open(&mut open).collapsible(false).frame(frame).show(ctx, |ui| {
            if !available {
                ui.label(egui::RichText::new("Plugin packages aren't available here.").color(t.text_muted));
                return;
            }
            egui::ScrollArea::vertical().max_height(t.card_width * 2.5).show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Packages add actions and catalogue entries. Each installs disabled, and one that runs actions does nothing until you grant it.",
                    )
                    .color(t.text_muted),
                );
                ui.add_space(t.control_gap);
                self.add_section(ui, session, t);
                ui.separator();
                self.package_list(ui, session, t);
                if let Some(notice) = &self.notice {
                    ui.label(egui::RichText::new(notice).color(t.danger));
                }
                if let Some(status) = &self.status {
                    ui.label(egui::RichText::new(status).color(t.text_muted));
                }
            });
        });
        if !open {
            self.preview = None;
            self.consent = None;
        }
        self.open = open;
    }

    fn checking(&self) -> bool {
        self.pending.values().any(|ask| matches!(ask, Ask::Check { .. } | Ask::Install { .. }))
    }

    /// The file to install, and the preview of a checked one.
    fn add_section(&mut self, ui: &mut egui::Ui, session: &mut Session, t: &Tokens) {
        ui.label(egui::RichText::new("Install a package").color(t.text));
        ui.horizontal(|ui| {
            let label = ui.label(egui::RichText::new("Package file").color(t.text_muted));
            let typed = ui
                .add(egui::TextEdit::singleline(&mut self.location).hint_text("path to a .ncraft-plugin file").desired_width(t.card_width * 1.5))
                .labelled_by(label.id);
            // A preview is of the file checked, not of the one now named.
            if typed.changed() {
                self.preview = None;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if ui.add_enabled(self.picking.is_none(), egui::Button::new("Choose…")).clicked() {
                self.picking = Some(pick::Picking::start());
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.update, "It updates an installed package");
            let ready = !self.location.trim().is_empty() && !self.checking();
            if ui.add_enabled(ready, egui::Button::new("Check")).on_hover_text("See what it is before installing anything").clicked() {
                self.check(session);
            }
        });
        let Some(preview) = self.preview.clone() else { return };
        egui::Frame::NONE.inner_margin(t.card_padding).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius).show(ui, |ui| {
            preview_body(ui, &preview.report, t);
            if self.drops_grant(&preview) {
                ui.label(egui::RichText::new("Updating removes its grant: it can't run until you grant it again.").color(t.danger));
            }
            ui.horizontal(|ui| {
                let label = if preview.update { "Update" } else { "Install" };
                if ui.add_enabled(!self.checking(), egui::Button::new(label)).clicked() {
                    self.install(session);
                }
                if ui.button("Cancel").clicked() {
                    self.preview = None;
                }
            });
        });
    }

    /// The installed packages and what can be done with each.
    fn package_list(&mut self, ui: &mut egui::Ui, session: &mut Session, t: &Tokens) {
        let rows = self.rows.clone();
        if rows.is_empty() {
            ui.label(egui::RichText::new("No packages installed.").color(t.text_muted));
        }
        for row in &rows {
            ui.push_id(&row.package_id, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(&row.package_id).color(t.text).monospace());
                    ui.label(egui::RichText::new(&row.version).color(t.text_muted));
                    ui.label(egui::RichText::new(if row.enabled { "Enabled" } else { "Disabled" }).color(if row.enabled { t.text } else { t.text_muted }));
                });
                let signer = row.signed_by.as_deref().map_or_else(|| "not signed".to_owned(), |s| format!("signed by {s}"));
                let rights = match &row.grant {
                    Some((_, caps)) if caps.is_empty() => "may run".to_owned(),
                    Some((_, caps)) => format!("may run, with {}", caps.join(", ")),
                    None => "not granted".to_owned(),
                };
                ui.label(egui::RichText::new(format!("Licence {} · {signer} · {rights}", row.license)).color(t.text_muted));
                ui.horizontal_wrapped(|ui| {
                    let id = row.package_id.as_str();
                    if row.enabled {
                        if ui.button("Disable").clicked() {
                            self.change(session, "plugins.disable", json!({"package_id": id}), format!("Disabled {id}."));
                        }
                    } else if ui.button("Enable").clicked() {
                        self.change(session, "plugins.enable", json!({"package_id": id}), format!("Enabled {id}."));
                    }
                    match &row.grant {
                        Some((grant_id, _)) => {
                            if ui.button("Revoke").on_hover_text("Stop it running; packages that depend on it are disabled").clicked() {
                                self.change(session, "plugins.revoke", json!({"grant_id": grant_id}), format!("{id} may no longer run."));
                            }
                        }
                        None => {
                            if ui.button("Grant…").on_hover_text("See what it asks for, then let it run").clicked() {
                                self.ask_rights(session, id);
                            }
                        }
                    }
                    if !row.enabled {
                        for kept in &row.kept {
                            if ui.button(format!("Roll back to {}", short(kept))).clicked() {
                                self.change(session, "plugins.rollback", json!({"package_id": id, "digest": kept}), format!("Rolled {id} back."));
                            }
                        }
                    }
                    let confirming = self.confirm_uninstall.as_deref() == Some(id);
                    if ui.button(if confirming { "Confirm uninstall" } else { "Uninstall" }).clicked() {
                        if confirming {
                            self.change(session, "plugins.uninstall", json!({"package_id": id}), format!("Uninstalled {id}."));
                        } else {
                            self.confirm_uninstall = Some(id.to_owned());
                        }
                    }
                });
                if let Some(consent) = self.consent.clone().filter(|c| c.package_id == row.package_id) {
                    self.consent_body(ui, session, consent, t);
                }
                ui.add_space(t.control_gap);
            });
        }
        if !rows.is_empty()
            && ui
                .button("Remove unused versions")
                .on_hover_text("Delete kept versions nothing uses, keeping those this project's cards were made with")
                .clicked()
        {
            self.change(session, "plugins.prune", json!({}), "Removed unused versions.".into());
        }
    }

    /// What granting would allow, with the optional capabilities to tick.
    fn consent_body(&mut self, ui: &mut egui::Ui, session: &mut Session, mut consent: Consent, t: &Tokens) {
        egui::Frame::NONE.inner_margin(t.card_padding).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius).show(ui, |ui| {
            if consent.required.is_empty() && consent.optional.is_empty() {
                ui.label(egui::RichText::new(format!("Let {} run? It asks for nothing beyond its own work.", consent.package_id)).color(t.text));
            } else {
                ui.label(egui::RichText::new(format!("Let {} run? It may:", consent.package_id)).color(t.text));
            }
            for cap in &consent.required {
                let mut on = true;
                ui.add_enabled(false, egui::Checkbox::new(&mut on, format!("{} (required)", capability_text(cap))));
            }
            for cap in &consent.optional {
                let mut on = consent.chosen.contains(cap);
                if ui.checkbox(&mut on, capability_text(cap)).changed() {
                    if on {
                        consent.chosen.insert(cap.clone());
                    } else {
                        consent.chosen.remove(cap);
                    }
                }
            }
            let network = consent.required.iter().chain(consent.chosen.iter()).any(|c| c == "network");
            if network && !consent.origins.is_empty() {
                ui.label(egui::RichText::new(format!("It may reach: {}", consent.origins.join(", "))).color(t.text_muted));
            }
            let mut grant = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                grant = ui.button("Grant").clicked();
                cancel = ui.button("Cancel").clicked();
            });
            self.consent = Some(consent);
            if grant {
                self.grant(session);
            } else if cancel {
                self.consent = None;
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn poll_pick(&mut self, ctx: &egui::Context) {
        let Some(picking) = self.picking.take() else { return };
        match picking.poll(ctx) {
            pick::Polled::Waiting(picking) => self.picking = Some(picking),
            pick::Polled::Cancelled => {}
            pick::Polled::Chosen(path) => match path.to_str() {
                Some(path) => {
                    self.location = path.to_owned();
                    self.preview = None;
                }
                None => self.notice = Some("That file's path isn't valid UTF-8; move it somewhere with a plainer name.".into()),
            },
            pick::Polled::Failed(message) => self.notice = Some(message),
        }
    }
}

/// A dry run's report: who made the package and what it will do.
fn preview_body(ui: &mut egui::Ui, report: &Value, t: &Tokens) {
    let text = |key: &str| report.get(key).and_then(Value::as_str).unwrap_or_default();
    ui.label(egui::RichText::new(format!("{} {}", text("package_id"), text("version"))).color(t.text).strong());
    let key = text("key_id");
    let (who, color) = match text("provenance") {
        "verified" => (format!("Signed by {} with a key you trust ({key}).", report.get("signed_by").and_then(Value::as_str).unwrap_or(key)), t.text),
        "unknown_key" => (format!("Signed with a key you don't trust ({key}): nobody you trust vouches for who made it."), t.danger),
        _ => ("Not signed: nobody vouches for who made it.".to_owned(), t.danger),
    };
    ui.label(egui::RichText::new(who).color(color));
    ui.label(egui::RichText::new(format!("Licence: {}", text("license"))).color(t.text_muted));
    if let Some(replaced) = report.get("replaced").and_then(Value::as_str) {
        ui.label(egui::RichText::new(format!("Replaces the installed version {}.", short(replaced))).color(t.text_muted));
    }
    let registrations = strings(report.get("registrations"));
    if !registrations.is_empty() {
        ui.label(egui::RichText::new(format!("Adds: {}", registrations.join(", "))).color(t.text_muted));
    }
    match report.get("rights") {
        Some(rights) if rights.get("error").is_some() => {
            let why = rights.get("error").and_then(Value::as_str).unwrap_or_default();
            ui.label(egui::RichText::new(format!("It can't be granted the right to run: {why}")).color(t.danger));
        }
        Some(rights) => {
            let asks: Vec<String> = strings(rights.get("required"))
                .iter()
                .map(|c| format!("{} (required)", capability_text(c)))
                .chain(strings(rights.get("optional")).iter().map(|c| format!("{} (optional)", capability_text(c))))
                .collect();
            if asks.is_empty() {
                ui.label(egui::RichText::new("It asks for no capabilities.").color(t.text_muted));
            } else {
                ui.label(egui::RichText::new(format!("If granted, it may {}.", asks.join("; "))).color(t.text));
            }
            let origins = strings(rights.get("origins"));
            if !origins.is_empty() {
                ui.label(egui::RichText::new(format!("Addresses it may reach: {}", origins.join(", "))).color(t.text_muted));
            }
        }
        None => {}
    }
    if let Some(delta) = report.get("rights_delta") {
        let added: Vec<String> =
            strings(delta.get("added_capabilities")).iter().map(|c| capability_text(c)).chain(strings(delta.get("added_origins"))).collect();
        if !added.is_empty() {
            ui.label(egui::RichText::new(format!("New in this version: {}. It must be granted again.", added.join("; "))).color(t.danger));
        }
    }
    for notice in report.get("notice_texts").and_then(Value::as_array).into_iter().flatten().take(MAX_ITEMS) {
        let path = notice.get("path").and_then(Value::as_str).unwrap_or("notice");
        egui::CollapsingHeader::new(path).id_salt(("nodecraft.plugins.notice", path)).show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(t.card_width).show(ui, |ui| {
                ui.label(egui::RichText::new(notice.get("text").and_then(Value::as_str).unwrap_or_default()).monospace().color(t.text));
            });
            if notice.get("truncated").and_then(Value::as_bool) == Some(true) {
                ui.label(egui::RichText::new("(cut short; the whole text is in the package)").color(t.text_muted));
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod pick {
    use std::{
        future::Future,
        path::PathBuf,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };

    type Pick = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

    /// A package file being chosen; the dialog never blocks the frame.
    pub(super) struct Picking(Pick);

    pub(super) enum Polled {
        Waiting(Picking),
        Cancelled,
        Chosen(PathBuf),
        Failed(String),
    }

    #[cfg(not(test))]
    fn choose() -> Pick {
        // Built on the UI thread: AppKit finds its window here.
        let future = rfd::AsyncFileDialog::new().set_title("Choose a plugin package").add_filter("NodeCraft plugin", &["ncraft-plugin"]).pick_file();
        Box::pin(async move { future.await.map(|file| file.path().to_path_buf()) })
    }

    /// Tests never open a real dialog: the pick is cancelled.
    #[cfg(test)]
    fn choose() -> Pick {
        Box::pin(std::future::ready(None))
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

    impl Picking {
        pub(super) fn start() -> Self {
            Self(choose())
        }

        pub(super) fn poll(mut self, ctx: &egui::Context) -> Polled {
            let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
            let mut cx = Context::from_waker(&waker);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.0.as_mut().poll(&mut cx))) {
                Ok(Poll::Pending) => Polled::Waiting(self),
                Ok(Poll::Ready(None)) => Polled::Cancelled,
                Ok(Poll::Ready(Some(path))) => Polled::Chosen(path),
                Err(_) => Polled::Failed("The file picker failed; nothing was changed.".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests;
