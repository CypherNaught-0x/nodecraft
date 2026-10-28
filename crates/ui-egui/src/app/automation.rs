//! Settings › Automation (#116): the desktop app's built-in MCP server.
//!
//! The desktop app runs the server (it needs `nodecraft-automation`, which this crate can't
//! use) and hands the shell a [`McpServerHost`] through [`NodeCraftApp::install_mcp_server`].
//! The shell then registers the host service behind the `automation.mcp_server_*` commands, shows
//! the Automation entry in the top bar and keeps the on/off choice in settings. The checkbox
//! dispatches `automation.mcp_server_set` like any other control; the token is only ever shown
//! here, masked, and copied to the clipboard on request, never returned by a command.
use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use serde_json::Value as Json;

pub use nodecraft_engine::mcp_server_cmds::SERVICE;

/// Where the built-in server stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpState {
    Off,
    /// Reading the token from the keychain and opening the port.
    Starting,
    Running {
        url: String,
    },
    Failed(String),
}

impl McpState {
    fn name(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Running { .. } => "running",
            Self::Failed(_) => "failed",
        }
    }

    /// The status the `automation.mcp_server_*` commands return. Never the token.
    pub fn status(&self, enabled: bool) -> Json {
        let url = match self {
            Self::Running { url } => Some(url.as_str()),
            _ => None,
        };
        let error = match self {
            Self::Failed(why) => Some(why.as_str()),
            _ => None,
        };
        json!({"enabled": enabled, "state": self.name(), "url": url, "error": error})
    }
}

/// The desktop app's server, as the shell sees it. Calls must return at once: starting happens
/// in the background, and the state says how it went.
pub trait McpServerHost {
    /// Whether the person turned it on (it may still be starting, or have failed).
    fn enabled(&self) -> bool;
    fn set_enabled(&mut self, enabled: bool);
    fn state(&self) -> McpState;
    /// The bearer token clients send, once the server has one.
    fn token(&self) -> Option<String>;
    /// Replaces the token (clients must be given the new one), restarting a running server.
    fn new_token(&mut self);
    /// Something the person should know, such as a token that lasts only until the app closes.
    fn note(&self) -> Option<String>;
}

/// The host, shared by the shell and the service the commands call.
pub type SharedMcpHost = Rc<RefCell<dyn McpServerHost>>;

/// The Automation panel.
#[derive(Default)]
pub struct AutomationPanel {
    pub open: bool,
    host: Option<SharedMcpHost>,
    /// The last thing that went wrong here (a refused command).
    status: Option<String>,
}

impl AutomationPanel {
    pub fn installed(&self) -> bool {
        self.host.is_some()
    }
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the MCP server is busy; try again")
}

/// What the client command looks like with `token` in it.
fn client_command(url: &str, token: &str) -> String {
    format!("claude mcp add --transport http nodecraft {url} --header \"Authorization: Bearer {token}\"")
}

/// The last four characters of `token`, for telling it apart without showing it.
fn masked(token: &str) -> String {
    let tail: Vec<char> = token.chars().rev().take(4).collect();
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

impl NodeCraftApp {
    /// Gives the shell the desktop app's MCP server: registers the host service the
    /// `automation.mcp_server_*` commands use, shows Automation in the top bar and, if settings
    /// say it was on, turns it on again.
    pub fn install_mcp_server(&mut self, host: SharedMcpHost) -> Result<(), Error> {
        let service = Rc::clone(&host);
        self.session.register_host_service(SERVICE, move |request: &Json| {
            let mut host = service.try_borrow_mut().map_err(|_| busy())?;
            match request.get("op").and_then(Json::as_str) {
                Some("set") => {
                    let on =
                        request.get("enabled").and_then(Json::as_bool).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "`enabled` must be true or false"))?;
                    host.set_enabled(on);
                }
                Some("status") => {}
                _ => return Err(Error::new(ErrorKind::InvalidInput, "malformed MCP server request")),
            }
            Ok(host.state().status(host.enabled()))
        })?;
        if self.mcp_setting() {
            host.try_borrow_mut().map_err(|_| busy())?.set_enabled(true);
        }
        self.automation.host = Some(host);
        Ok(())
    }

    /// Settings › Automation, or the defaults without a settings file.
    pub fn automation_settings(&self) -> nodecraft_engine::settings::AutomationSettings {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &self.store {
            return store.get().automation.clone();
        }
        nodecraft_engine::settings::AutomationSettings::default()
    }

    /// Whether settings say the server should run.
    fn mcp_setting(&self) -> bool {
        self.automation_settings().mcp_server
    }

    /// Keeps the on/off choice in settings, however it was made (the panel, the palette).
    pub(super) fn sync_mcp_setting(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(host), Some(store)) = (&self.automation.host, &mut self.store)
            && let Ok(host) = host.try_borrow()
        {
            let on = host.enabled();
            if store.get().automation.mcp_server != on {
                store.update(|settings| settings.automation.mcp_server = on);
            }
        }
    }

    pub(super) fn automation_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.automation.open {
            return;
        }
        let Some(host) = self.automation.host.clone() else {
            self.automation.open = false;
            return;
        };
        // Read everything first: the checkbox dispatches a command whose service borrows the host.
        let Ok((enabled, state, token, note)) = host.try_borrow().map(|h| (h.enabled(), h.state(), h.token(), h.note())) else {
            ctx.request_repaint();
            return;
        };
        if state == McpState::Starting {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut open = true;
        let mut toggle = None;
        let mut rotate = false;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Automation").id(egui::Id::new("nodecraft.automation")).open(&mut open).collapsible(false).resizable(false).frame(frame).show(ctx, |ui| {
            let mut on = enabled;
            if ui.checkbox(&mut on, "Enable MCP server").on_hover_text("Let AI agents on this computer work on this window through MCP").changed() {
                toggle = Some(on);
            }
            ui.label(
                egui::RichText::new("Only programs on this computer can connect, and only with the token. Agents get the commands `nodecraft-cli mcp --bridge` offers: people-only commands stay yours, and paid runs keep their quotes and budget caps.")
                    .color(t.text_muted),
            );
            ui.add_space(t.control_gap);
            let (text, color) = match &state {
                McpState::Off => ("Off".to_owned(), t.text_muted),
                McpState::Starting => ("Starting…".to_owned(), t.text_muted),
                McpState::Running { url } => (format!("Running at {url}"), t.text),
                McpState::Failed(why) => (format!("Not running: {why}"), t.danger),
            };
            ui.label(egui::RichText::new(text).color(color));
            if let McpState::Running { url } = &state {
                ui.horizontal(|ui| {
                    if ui.button("Copy address").clicked() {
                        ui.ctx().copy_text(url.clone());
                    }
                    if let Some(token) = &token {
                        ui.label(egui::RichText::new(format!("Token {}", masked(token))).color(t.text).monospace());
                        if ui.button("Copy token").on_hover_text("Clients send it as `Authorization: Bearer <token>`").clicked() {
                            ui.ctx().copy_text(token.clone());
                        }
                        if ui.button("New token").on_hover_text("Clients using the old token must be given the new one").clicked() {
                            rotate = true;
                        }
                    }
                });
                if let Some(token) = &token {
                    ui.add_space(t.control_gap);
                    ui.label(egui::RichText::new("Connect Claude Code:").color(t.text_muted));
                    ui.label(egui::RichText::new(client_command(url, &masked(token))).color(t.text).monospace());
                    if ui.button("Copy command").on_hover_text("The command, with the full token").clicked() {
                        ui.ctx().copy_text(client_command(url, token));
                    }
                }
            }
            if let Some(note) = &note {
                ui.label(egui::RichText::new(note).color(t.text_muted));
            }
            if let Some(status) = &self.automation.status {
                ui.label(egui::RichText::new(status).color(t.danger));
            }
        });
        self.automation.open = open;
        if let Some(on) = toggle {
            self.automation.status = self.session.dispatch("automation.mcp_server_set", &json!({"enabled": on})).err().map(|e| e.message);
        }
        if rotate {
            match host.try_borrow_mut() {
                Ok(mut host) => host.new_token(),
                Err(_) => self.automation.status = Some(busy().message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// A server that starts at once, recording what the shell asked of it.
    #[derive(Default)]
    struct Fake {
        enabled: bool,
        rotated: usize,
    }

    impl McpServerHost for Fake {
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn set_enabled(&mut self, enabled: bool) {
            self.enabled = enabled;
        }
        fn state(&self) -> McpState {
            if self.enabled { McpState::Running { url: "http://127.0.0.1:41116/mcp".into() } } else { McpState::Off }
        }
        fn token(&self) -> Option<String> {
            self.enabled.then(|| format!("{}{}", "s3cret".repeat(10), self.rotated))
        }
        fn new_token(&mut self) {
            self.rotated += 1;
        }
        fn note(&self) -> Option<String> {
            None
        }
    }

    fn harness(fake: &Rc<RefCell<Fake>>) -> Harness<'static, NodeCraftApp> {
        let mut app = NodeCraftApp::headless().unwrap();
        app.install_mcp_server(Rc::clone(fake) as SharedMcpHost).unwrap();
        Harness::builder()
            .with_size(Vec2::new(1600.0, 900.0))
            .with_step_dt(1.0 / 60.0)
            .with_max_steps(120)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
    }

    #[test]
    fn the_automation_panel_turns_the_server_on_shows_where_to_connect_and_off_again() {
        let fake = Rc::new(RefCell::new(Fake::default()));
        let mut h = harness(&fake);
        h.run();
        crate::app::topbar_tests::reveal(&mut h, "Automation");
        h.get_by_label("Automation").click();
        h.run();
        assert!(h.state().automation.open);
        assert!(h.query_by_label_contains("Running at").is_none(), "off by default");
        h.get_by_label("Enable MCP server").click();
        h.run();
        assert!(fake.borrow().enabled, "the checkbox dispatched automation.mcp_server_set");
        h.get_by_label_contains("Running at http://127.0.0.1:41116/mcp");
        assert!(h.query_by_label_contains("s3crets3cret").is_none(), "the token is never shown in full");
        h.get_by_label_contains("Token …ret0");
        h.get_by_label_contains("--header \"Authorization: Bearer …ret0\"");
        h.get_by_label("Copy token").click();
        h.run();
        h.get_by_label("New token").click();
        h.run();
        assert_eq!(fake.borrow().rotated, 1);
        // The command reports the same state, without the token.
        let status = h.state_mut().session.dispatch("automation.mcp_server_status", &json!({})).unwrap();
        assert_eq!(status, json!({"enabled": true, "state": "running", "url": "http://127.0.0.1:41116/mcp", "error": null}));
        assert!(!status.to_string().contains("s3cret"));
        h.get_by_label("Enable MCP server").click();
        h.run();
        assert!(!fake.borrow().enabled);
        h.get_by_label("Off");
    }

    #[test]
    fn without_a_server_host_there_is_no_automation_entry_and_the_commands_are_refused() {
        let mut h = super::super::tests::harness();
        h.run();
        assert!(h.query_by_label("Automation").is_none());
        let refused = h.state_mut().session.dispatch("automation.mcp_server_set", &json!({"enabled": true})).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Unsupported);
    }

    #[test]
    fn a_running_server_is_reported_by_state_and_the_token_is_masked() {
        assert_eq!(McpState::Failed("port in use".into()).status(true), json!({"enabled": true, "state": "failed", "url": null, "error": "port in use"}));
        assert_eq!(McpState::Starting.status(true)["state"], "starting");
        assert_eq!(masked("abcdef"), "…cdef");
        assert_eq!(masked("ab"), "…ab");
    }
}
