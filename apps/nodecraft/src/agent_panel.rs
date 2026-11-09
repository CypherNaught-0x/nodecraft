//! The agent sidebar (#114): a chat with the in-canvas agent beside the canvas, opened and closed
//! with Ctrl/Cmd+/. The person's message goes to the agent with what they have selected (and the
//! nodes they mention with @); the agent works through the same tools as the MCP server, over the
//! window's live session, so each step shows on the canvas as it happens and is undoable. In
//! assist mode changes and runs wait for Approve; in auto mode runs go ahead within the budget in
//! `[agent]` settings. Esc cancels whatever is in progress.
//!
//! The panel holds the [`Agent`] and its worker thread; the shell draws it and passes the app in,
//! so it never holds the app across frames.
use nodecraft_agent::{Agent, Config, Focus, FocusNode, Impact, Mode, Next, Status, Step, StepKind, ToolStatus};
use nodecraft_engine::settings::{AgentEndpoint, AgentMode, AgentSettings};
use nodecraft_types::{Currency, NodeId};
use nodecraft_ui_egui::NodeCraftApp;
use std::collections::BTreeMap;

mod toolbox;
mod worker;

use toolbox::LiveTools;
#[cfg(test)]
use worker::SendFn;
use worker::Worker;

/// Most nodes offered in the @ menu, and most mentions kept on one message.
const MAX_MENTIONS: usize = 100;
/// Longest node name shown on a mention, in characters.
const MAX_NAME_CHARS: usize = 40;

/// What the person did in the panel this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Ask,
    Approve,
    Decline,
    Cancel,
    Mode(Mode),
    Clear,
    Mention(NodeId),
    Unmention(NodeId),
    /// Open the agent's settings, or close them without saving (#1063).
    Settings(bool),
    /// Save the settings form as one settings write.
    SaveSettings,
}

/// The settings form while it is open (#1063): what the person has chosen, not yet saved.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub endpoint: AgentEndpoint,
    pub model: String,
    pub base_url: String,
    /// Why the last Save was refused.
    pub problem: Option<String>,
}

impl Draft {
    fn of(settings: &AgentSettings) -> Self {
        Self { endpoint: settings.endpoint, model: settings.model.clone(), base_url: settings.base_url.clone().unwrap_or_default(), problem: None }
    }

    /// The settings this form saves over `current`, or why it can't.
    fn apply(&self, current: &AgentSettings) -> Result<AgentSettings, String> {
        let model = self.model.trim();
        let url = self.base_url.trim();
        let next = AgentSettings {
            endpoint: self.endpoint,
            model: model.to_owned(),
            // A cloud endpoint keeps the local server's address for when it is chosen again.
            base_url: if local(self.endpoint) { (!url.is_empty()).then(|| url.to_owned()) } else { current.base_url.clone() },
            ..current.clone()
        };
        if !model.is_empty() && next.model().is_none() {
            return Err(format!("The model name must be at most {} bytes, with no control characters.", AgentSettings::MAX_TEXT_BYTES));
        }
        if worker::model(&next).is_none() {
            return Err(format!("Choose a model for {}.", endpoint_label(self.endpoint)));
        }
        if local(self.endpoint) && next.base_url.is_some() && next.base_url().is_none() {
            return Err("The server address must start with http:// or https://.".into());
        }
        Ok(next)
    }
}

/// The endpoints the agent can ask, in the order the form lists them.
const ENDPOINTS: [AgentEndpoint; 5] =
    [AgentEndpoint::Anthropic, AgentEndpoint::Openai, AgentEndpoint::Openrouter, AgentEndpoint::Ollama, AgentEndpoint::LmStudio];

fn endpoint_label(endpoint: AgentEndpoint) -> &'static str {
    match endpoint {
        AgentEndpoint::Anthropic => "Anthropic",
        AgentEndpoint::Openai => "OpenAI",
        AgentEndpoint::Openrouter => "OpenRouter",
        AgentEndpoint::Ollama => "Ollama",
        AgentEndpoint::LmStudio => "LM Studio",
    }
}

/// A server on this machine, at an address the person may set; it needs no key.
fn local(endpoint: AgentEndpoint) -> bool {
    endpoint.account().is_none()
}

pub struct AgentPanel {
    pub open: bool,
    agent: Agent,
    worker: Result<Worker, String>,
    /// The settings the last request was sent with; the worker reads the endpoint from them.
    settings: AgentSettings,
    pub input: String,
    mentions: Vec<NodeId>,
    /// A problem outside the conversation (no model chosen, the worker is busy).
    notice: Option<String>,
    new_id: Box<dyn FnMut() -> [u8; 16]>,
    focus_input: bool,
    /// The settings form, while it is open.
    pub draft: Option<Draft>,
}

/// The sidebar's opening width and the range it can be dragged in, for a window `width` wide:
/// `agent_width` to open, at least `agent_min_width`, at most `agent_max_fraction` of the window
/// (but never below the minimum, so the sidebar's controls stay usable).
fn sidebar_width(width: f32, t: &nodecraft_ui_egui::theme::Tokens) -> (f32, egui::Rangef) {
    let half = if width.is_finite() { width * t.agent_max_fraction } else { t.agent_width };
    let max = half.max(t.agent_min_width);
    (t.agent_width.clamp(t.agent_min_width, max), egui::Rangef::new(t.agent_min_width, max))
}

impl AgentPanel {
    /// The panel over `worker` (or why there is none), starting in the settings' mode.
    pub fn new(worker: Result<Worker, String>, settings: AgentSettings) -> Self {
        let config = Config { model: worker::model(&settings).unwrap_or_default(), mode: mode(settings.mode), budget: budget(&settings), max_tokens: None };
        Self {
            open: settings.open,
            agent: Agent::new(config),
            worker,
            settings,
            input: String::new(),
            mentions: Vec::new(),
            notice: None,
            new_id: Box::new(toolbox::id_source()),
            focus_input: false,
            draft: None,
        }
    }

    /// The live panel: requests go to the endpoint in `settings` with keys from the keychain.
    /// `providers` is the provider settings the app started with (#960).
    pub fn live(ctx: &egui::Context, settings: AgentSettings, providers: nodecraft_engine::settings::ProviderSettings) -> Self {
        let waker = ctx.clone();
        let worker = Worker::live(std::sync::Arc::new(move || waker.request_repaint()), providers).map_err(|e| e.message);
        Self::new(worker, settings)
    }

    #[cfg(test)]
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    #[cfg(test)]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    #[cfg(test)]
    pub fn mentions(&self) -> &[NodeId] {
        &self.mentions
    }

    fn input_id() -> egui::Id {
        egui::Id::new("nodecraft-agent-input")
    }

    /// Before the app's own frame: the panel's shortcuts, and replies that have arrived.
    pub fn logic(&mut self, app: &mut NodeCraftApp, ctx: &egui::Context) {
        // A dialog keeps its keys: Cmd+/ and Esc mean nothing to the sidebar while one is open.
        let modal = ctx.memory(|m| m.top_modal_layer()).is_some() || egui::Popup::is_any_open(ctx);
        if !modal && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Slash)) {
            self.open = !self.open;
            // Only take focus that nobody else holds, so a half-typed edit elsewhere stays put.
            self.focus_input = self.open && ctx.memory(|m| m.focused()).is_none();
            let open = self.open;
            app.update_agent_settings(|s| s.open = open);
        }
        // Esc stops the agent only from the sidebar's own text field; elsewhere it keeps its
        // meaning (closing a dialog, cancelling a drag on the canvas).
        // egui drops focus as Esc arrives, so the input counts as focused if it was last frame.
        let ours = ctx.memory(|m| m.has_focus(Self::input_id()) || m.had_focus_last_frame(Self::input_id()));
        if self.open && ours && !modal && self.agent.status() != Status::Idle && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.apply(vec![Intent::Cancel], app, ctx);
        }
        // While a background save, open or recovery runs, every change a reply asks for would be
        // refused as busy and the model would spend a paid turn retrying it (#367): replies wait
        // in the worker until it has settled. Likewise while the startup keep-aside holds the
        // session (#1053).
        let busy = app.session.file_status().operation.is_some() || app.keeping_aside();
        if let Ok(worker) = &self.worker
            && !busy
        {
            let mut replies = Vec::new();
            while let Some(reply) = worker.poll() {
                replies.push(reply);
            }
            for (ticket, reply) in replies {
                let mut tools = LiveTools { commands: crate::control::LiveCommands { app: &mut *app, ctx, roots: None }, new_id: &mut *self.new_id };
                let next = self.agent.on_reply(ticket, reply, &mut tools);
                self.follow(next);
            }
        }
        self.forget_unwanted();
    }

    /// Once the agent no longer waits for a reply (cancelled, cleared, answered), the worker drops
    /// the request it was sending and any queued behind it, so nothing stale is paid for.
    fn forget_unwanted(&self) {
        if self.agent.status() != Status::Thinking
            && let Ok(worker) = &self.worker
        {
            worker.forget();
        }
    }

    /// Carries out what the person did, with the app as the agent's tools.
    pub fn apply(&mut self, intents: Vec<Intent>, app: &mut NodeCraftApp, ctx: &egui::Context) {
        for intent in intents {
            match intent {
                Intent::Ask => self.ask(app, ctx),
                Intent::Cancel => self.agent.cancel(),
                // Approved steps would be refused as busy while a file operation runs: the
                // approval stays to be given once it has finished (#367).
                Intent::Approve if app.session.file_status().operation.is_some() || app.keeping_aside() => {}
                Intent::Approve | Intent::Decline => {
                    let mut tools = LiveTools { commands: crate::control::LiveCommands { app: &mut *app, ctx, roots: None }, new_id: &mut *self.new_id };
                    let next = if intent == Intent::Approve { self.agent.approve(&mut tools) } else { self.agent.decline(&mut tools) };
                    self.follow(next);
                }
                Intent::Mode(mode) => {
                    self.agent.set_mode(mode);
                    let saved = match mode {
                        Mode::Assist => AgentMode::Assist,
                        Mode::Auto => AgentMode::Auto,
                    };
                    app.update_agent_settings(|s| s.mode = saved);
                }
                Intent::Clear => {
                    if let Err(e) = self.agent.clear() {
                        self.notice = Some(e.message);
                    } else {
                        self.notice = None;
                    }
                }
                Intent::Mention(id) => {
                    if !self.mentions.contains(&id) && self.mentions.len() < MAX_MENTIONS {
                        self.mentions.push(id);
                    }
                    self.focus_input = true;
                }
                Intent::Unmention(id) => self.mentions.retain(|m| *m != id),
                Intent::Settings(true) => self.draft = Some(Draft::of(&app.agent_settings())),
                Intent::Settings(false) => self.draft = None,
                Intent::SaveSettings => self.save_settings(app),
            }
        }
        self.forget_unwanted();
    }

    /// Saves the form as one settings write, or says why it can't; the next message uses it.
    fn save_settings(&mut self, app: &mut NodeCraftApp) {
        let Some(draft) = &mut self.draft else { return };
        match draft.apply(&app.agent_settings()) {
            Ok(next) => {
                let written = app.save_agent_settings(next.clone());
                if let Some(model) = worker::model(&next) {
                    self.agent.set_model(model);
                }
                self.settings = next;
                self.draft = None;
                self.notice = written.err().map(|why| format!("Settings changed, but {why}."));
            }
            Err(problem) => draft.problem = Some(problem),
        }
    }

    fn ask(&mut self, app: &mut NodeCraftApp, ctx: &egui::Context) {
        if let Err(e) = &self.worker {
            self.notice = Some(format!("The agent can't reach a model: {e}"));
            return;
        }
        // Hand edits to `[agent]` apply from the next message; the mode only when the file's
        // changed, so a switch made here stands where settings aren't saved (the browser).
        let read = app.agent_settings();
        if read.mode != self.settings.mode {
            self.agent.set_mode(mode(read.mode));
        }
        self.settings = read;
        let Some(model) = worker::model(&self.settings) else {
            self.notice = Some("Choose a model for this endpoint in the agent's Settings.".into());
            return;
        };
        self.agent.set_model(model);
        self.agent.set_budget(budget(&self.settings));
        let focus = focus(app, &self.mentions);
        let tools = LiveTools { commands: crate::control::LiveCommands { app: &mut *app, ctx, roots: None }, new_id: &mut *self.new_id };
        let next = self.agent.ask(&self.input, &focus, &tools);
        match next {
            Ok(next) => {
                self.input.clear();
                self.mentions.clear();
                self.notice = None;
                self.follow(next);
            }
            Err(e) => self.notice = Some(e.message),
        }
    }

    /// Sends the request the agent asked for. If it can't be sent, the agent stops, so it never
    /// waits for a reply that won't come.
    fn follow(&mut self, next: Next) {
        if matches!(next, Next::Approval) {
            // A step waiting for approval is never hidden.
            self.open = true;
        }
        let Next::Send(ticket, request) = next else { return };
        let sent = match &self.worker {
            Ok(worker) => worker.send(ticket, &self.settings, request).map_err(|e| e.message),
            Err(e) => Err(e.clone()),
        };
        if let Err(e) = sent {
            self.agent.cancel();
            self.notice = Some(e);
        }
    }

    /// Draws the sidebar (when open) at the right of `ui`; what the person did, for
    /// [`Self::apply`].
    pub fn show(&mut self, ui: &mut egui::Ui, app: &NodeCraftApp) -> Vec<Intent> {
        let mut intents = Vec::new();
        if !self.open {
            // The open panel takes one automatic id; so does the closed one, so the canvas's
            // widgets keep their ids (and their state) when the sidebar opens or closes.
            ui.skip_ahead_auto_ids(1);
            return intents;
        }
        let t = app.theme.tokens();
        let frame = egui::Frame::NONE.fill(t.panel_fill).stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke)).inner_margin(t.card_padding);
        // Resizable between its narrowest and half the window, so a small window keeps a canvas (#1115).
        let (default, range) = sidebar_width(ui.max_rect().width(), t);
        egui::Panel::right("nodecraft-agent").frame(frame).resizable(true).default_size(default).size_range(range).show(ui, |ui| {
            let status = self.agent.status();
            ui.horizontal(|ui| {
                ui.heading("Agent");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(status == Status::Idle, egui::Button::new("Clear")).on_hover_text("Start a new conversation").clicked() {
                        intents.push(Intent::Clear);
                    }
                    let editing = self.draft.is_some();
                    if ui
                        .add_enabled(status == Status::Idle || editing, egui::Button::new("Settings").selected(editing))
                        .on_hover_text("Which endpoint and model the agent asks")
                        .clicked()
                    {
                        intents.push(Intent::Settings(!editing));
                    }
                    let current = self.agent.config().mode;
                    for (mode, label, hint) in [
                        (Mode::Auto, "Auto", "Changes happen as the agent makes them; runs start while they fit the budget"),
                        (Mode::Assist, "Assist", "Changes and runs wait for your approval"),
                    ] {
                        if ui.selectable_label(current == mode, label).on_hover_text(hint).clicked() && current != mode {
                            intents.push(Intent::Mode(mode));
                        }
                    }
                });
            });
            let model = if self.agent.config().model.is_empty() { "no model chosen".to_owned() } else { self.agent.config().model.clone() };
            let spent = self.agent.accepted().get(&Currency::USD).copied().unwrap_or(0);
            let limit = self.agent.config().budget.get(&Currency::USD).copied().unwrap_or(0);
            ui.label(egui::RichText::new(format!("{model} · runs accepted {} of {} budget", usd(spent), usd(limit))).color(t.text_muted).size(t.label_size));
            if let Some(notice) = &self.notice {
                ui.label(egui::RichText::new(notice).color(t.danger));
            }
            ui.separator();
            if let Some(draft) = &mut self.draft {
                settings_form(ui, draft, status == Status::Idle, &mut intents, t);
                ui.separator();
            }
            egui::Panel::bottom("nodecraft-agent-input").frame(egui::Frame::NONE).resizable(false).show(ui, |ui| {
                self.controls(ui, app, status, &mut intents);
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                let mut any = false;
                for step in self.agent.timeline() {
                    any = true;
                    step_row(ui, step, t);
                }
                if !any {
                    ui.label(
                        egui::RichText::new("Ask the agent to build, change or run your workflow. It sees what you have selected; mention other nodes with @.")
                            .color(t.text_muted),
                    );
                }
                if status == Status::Thinking {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Thinking…").color(t.text_muted));
                    });
                }
            });
        });
        intents
    }

    fn controls(&mut self, ui: &mut egui::Ui, app: &NodeCraftApp, status: Status, intents: &mut Vec<Intent>) {
        let t = app.theme.tokens();
        match status {
            Status::Waiting => {
                ui.label(egui::RichText::new("The agent wants to go ahead with the steps marked for approval.").color(t.text));
                ui.horizontal(|ui| {
                    let busy = app.session.file_status().operation.is_some() || app.keeping_aside();
                    if ui
                        .add_enabled(!busy, egui::Button::new("Approve"))
                        .on_disabled_hover_text("Available once the project has finished saving or opening")
                        .clicked()
                    {
                        intents.push(Intent::Approve);
                    }
                    if ui.button("Decline").clicked() {
                        intents.push(Intent::Decline);
                    }
                    if ui.button("Cancel").on_hover_text("Esc").clicked() {
                        intents.push(Intent::Cancel);
                    }
                });
            }
            Status::Thinking => {
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    intents.push(Intent::Cancel);
                }
            }
            Status::Idle => {}
        }
        let canvas = app.session.document().canvases().get(&app.canvas);
        ui.horizontal_wrapped(|ui| {
            for id in &self.mentions {
                let name = canvas.and_then(|c| c.nodes().get(id)).map_or_else(|| "a removed node".to_owned(), |n| node_name(&n.title, &n.kind));
                if ui.small_button(format!("@{name} ×")).on_hover_text("Don't mention this node").clicked() {
                    intents.push(Intent::Unmention(*id));
                }
            }
            ui.menu_button("@", |ui| {
                ui.set_max_height(t.comments_height);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let nodes = canvas.map(|c| c.nodes().values().take(MAX_MENTIONS).collect::<Vec<_>>()).unwrap_or_default();
                    if nodes.is_empty() {
                        ui.label("No nodes on this canvas");
                    }
                    for node in nodes {
                        if ui.button(node_name(&node.title, &node.kind)).clicked() {
                            intents.push(Intent::Mention(node.id));
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text("Mention a node");
        });
        let input = egui::TextEdit::multiline(&mut self.input)
            .id(Self::input_id())
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("Ask the agent… (Enter to send, Shift+Enter for a new line)");
        // Enter sends, so it's taken before the text field would type it wherever the cursor is.
        let focused = ui.memory(|m| m.has_focus(Self::input_id()));
        let enter = focused && ui.input_mut(|i| !i.modifiers.shift && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let response = ui.add(input);
        if std::mem::take(&mut self.focus_input) {
            response.request_focus();
        }
        let can_ask = status == Status::Idle && !self.input.trim().is_empty();
        let send = ui.add_enabled(can_ask, egui::Button::new("Send")).clicked();
        if (enter && can_ask) || send {
            intents.push(Intent::Ask);
        }
    }
}

fn step_row(ui: &mut egui::Ui, step: &Step, t: &nodecraft_ui_egui::theme::Tokens) {
    match &step.kind {
        StepKind::Asked(text) => {
            ui.add_space(t.control_gap);
            ui.label(egui::RichText::new("You").strong().color(t.text));
            ui.label(egui::RichText::new(text).color(t.text));
        }
        StepKind::Said(text) => {
            ui.add_space(t.control_gap);
            ui.label(egui::RichText::new(text).color(t.text));
        }
        StepKind::Tool(tool) => {
            let (status, colour) = match &tool.status {
                ToolStatus::Queued => ("waiting".to_owned(), t.text_muted),
                ToolStatus::Planned => ("needs your approval".to_owned(), t.accent_text),
                ToolStatus::Ran(summary) => (if summary.is_empty() { "done".to_owned() } else { summary.clone() }, t.text_muted),
                ToolStatus::Failed(why) => (format!("failed: {why}"), t.danger),
                ToolStatus::Declined => ("declined".to_owned(), t.text_muted),
                ToolStatus::Cancelled => ("cancelled".to_owned(), t.text_muted),
            };
            let what = tool.impact.as_ref().map(impact_text).unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("▸ {}", tool.label)).strong().color(t.text)).on_hover_text(&tool.input);
                if !what.is_empty() {
                    ui.label(egui::RichText::new(what).color(t.text_muted).size(t.label_size));
                }
            });
            ui.label(egui::RichText::new(status).color(colour).size(t.label_size));
        }
        StepKind::Error(why) => {
            ui.label(egui::RichText::new(why).color(t.danger));
        }
        StepKind::Cancelled => {
            ui.label(egui::RichText::new("Cancelled").color(t.text_muted));
        }
    }
}

/// What a step would do, in a few words.
fn impact_text(impact: &Impact) -> String {
    match impact {
        Impact::Read => "reads".into(),
        Impact::Edit => "changes the project".into(),
        Impact::Host => "asks the app".into(),
        Impact::Run(run) => {
            let mut out = match run.ceiling.get(&Currency::USD) {
                Some(micros) => format!("runs, up to {}", usd(*micros)),
                None if run.ceiling.is_empty() => "runs".to_owned(),
                None => "runs, priced in another currency".to_owned(),
            };
            if run.unpriced_jobs > 0 || run.partial {
                out.push_str(", part of the price unknown");
            }
            out
        }
    }
}

/// Micro-dollars as dollars and cents, rounded up so a cost never shows as less than it is.
fn usd(micros: u64) -> String {
    let cents = micros.div_ceil(10_000);
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn node_name(title: &str, kind: &str) -> String {
    let name = if title.trim().is_empty() { kind } else { title };
    let clipped: String = name.chars().take(MAX_NAME_CHARS).map(|c| if c.is_control() { ' ' } else { c }).collect();
    if name.chars().nth(MAX_NAME_CHARS).is_some() { format!("{clipped}…") } else { clipped }
}

/// The agent's settings form (#1063): the endpoint, its model and, for a local server, its
/// address. Saved only with Save; keys stay in the keychain (the API keys window).
fn settings_form(ui: &mut egui::Ui, draft: &mut Draft, idle: bool, intents: &mut Vec<Intent>, t: &nodecraft_ui_egui::theme::Tokens) {
    ui.label(egui::RichText::new("Endpoint").color(t.text_muted).size(t.label_size));
    egui::ComboBox::from_id_salt("nodecraft-agent-endpoint").selected_text(endpoint_label(draft.endpoint)).show_ui(ui, |ui| {
        for endpoint in ENDPOINTS {
            ui.selectable_value(&mut draft.endpoint, endpoint, endpoint_label(endpoint));
        }
    });
    let label = ui.label(egui::RichText::new("Model").color(t.text_muted).size(t.label_size));
    let hint = if draft.endpoint == AgentEndpoint::Anthropic { "the endpoint's default" } else { "the model's name, as the endpoint lists it" };
    ui.add(egui::TextEdit::singleline(&mut draft.model).hint_text(hint).char_limit(AgentSettings::MAX_TEXT_BYTES)).labelled_by(label.id);
    if local(draft.endpoint) {
        let label = ui.label(egui::RichText::new("Server address").color(t.text_muted).size(t.label_size));
        ui.add(egui::TextEdit::singleline(&mut draft.base_url).hint_text("its default address").char_limit(AgentSettings::MAX_TEXT_BYTES))
            .labelled_by(label.id);
    } else {
        ui.label(egui::RichText::new("Its key is the one in the API keys window, kept in the system keychain.").color(t.text_muted).size(t.label_size));
    }
    if let Some(problem) = &draft.problem {
        ui.label(egui::RichText::new(problem).color(t.danger));
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(idle, egui::Button::new("Save")).on_disabled_hover_text("Available once the agent has finished").clicked() {
            intents.push(Intent::SaveSettings);
        }
        if ui.button("Cancel").clicked() {
            intents.push(Intent::Settings(false));
        }
    });
}

fn mode(mode: AgentMode) -> Mode {
    match mode {
        AgentMode::Assist => Mode::Assist,
        AgentMode::Auto => Mode::Auto,
    }
}

fn budget(settings: &AgentSettings) -> BTreeMap<Currency, u64> {
    BTreeMap::from([(Currency::USD, settings.budget_usd_micros())])
}

/// The canvas the person is on, and the nodes they selected or mentioned (selection first).
fn focus(app: &NodeCraftApp, mentions: &[NodeId]) -> Focus {
    let canvas = app.session.document().canvases().get(&app.canvas);
    let mut ids: Vec<NodeId> = app.selection.nodes().iter().copied().collect();
    for id in mentions {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    let selection = ids
        .into_iter()
        .filter_map(|id| canvas?.nodes().get(&id))
        .map(|node| FocusNode { id: node.id, kind: node.kind.clone(), title: node.title.clone() })
        .collect();
    Focus { canvas: canvas.map(|_| app.canvas), selection }
}

#[cfg(test)]
mod tests;
