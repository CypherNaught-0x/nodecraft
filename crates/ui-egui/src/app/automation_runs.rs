//! Paid runs from automation (#1079): the control channel (and the MCP bridge over it), the
//! built-in MCP server and the in-canvas agent may quote and start runs in the window, but only
//! as the person allows.
//!
//! Before such a client's `run.quote`, `run.changed`, `run.workflow`, `run.estimate` or
//! `run.start` reaches the session, [`NodeCraftApp::automation_run_gate`] routes the cards it
//! names to their providers as Generate does (without opening anything: until the providers are
//! set up the gate says to wait), and before a `run.start` that may spend money it checks what
//! the person decided for that channel. The first time, the window asks: Allow once (this run,
//! by its quote token), Always allow (up to a per-run ceiling, kept in settings) or Never. A run
//! accepted entirely at no charge needs no consent. Nothing here relaxes a quote, a budget, the
//! accepted ceiling or unknown-price consent: the engine checks those on every `run.start`.
//!
//! The dialog's buttons and the Automation panel dispatch `automation.run_consent_answer` and
//! `automation.run_consent_set`, which automation can't run; any client may read where it stands
//! with `automation.run_consent_list`.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::generate::{Quote, RunKind};
use super::*;
use nodecraft_engine::settings::{RunConsentDecision, RunConsentSetting};
use serde_json::Value as Json;

pub use nodecraft_engine::run_consent_cmds::SERVICE;

/// The run commands an automation client's call is gated on.
const RUN_COMMANDS: [&str; 5] = ["run.quote", "run.changed", "run.workflow", "run.estimate", "run.start"];
/// Most currencies an accepted amount may name before the gate refuses it.
const MAX_ACCEPTED_CURRENCIES: usize = 64;
/// The largest ceiling the dialog takes, in a currency's whole units.
const MAX_CEILING_UNITS: f64 = 1_000_000_000.0;

/// What a client is told while the window sets up the providers its run needs.
pub const SETTING_UP: &str = "NodeCraft is setting up the providers this run needs; send the same request again in a moment. Nothing was started or charged.";
/// While the window asks the person about this client's run.
pub const ASKING: &str = "NodeCraft is asking the person in its window whether this client may start paid runs; send the same run.start again after they answer. Nothing was started or charged.";
/// While the window asks the person about another client's run.
pub const ASKING_OTHER: &str =
    "NodeCraft is asking the person about another paid run; send the same run.start again after they answer. Nothing was started or charged.";
/// The person said never.
pub const NEVER: &str =
    "The person doesn't allow paid runs from this client. They can change that in NodeCraft's Automation panel. Nothing was started or charged.";
/// The person declined this run.
pub const DECLINED: &str = "The person declined this run in the NodeCraft window. Nothing was started or charged.";

/// An automation channel. Each one's decision is remembered apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AutomationClient {
    /// `nodecraft --control`, and `nodecraft-cli mcp --bridge`, which forwards over it.
    Control,
    /// Settings › Automation › Enable MCP server (#116).
    McpServer,
    /// The in-canvas agent (#114).
    Agent,
}

impl AutomationClient {
    pub const ALL: [Self; 3] = [Self::Control, Self::McpServer, Self::Agent];

    /// The key its decision has in settings and in the commands.
    pub fn key(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::McpServer => "mcp_server",
            Self::Agent => "agent",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|client| client.key() == key)
    }

    /// How the window names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Control => "The control channel",
            Self::McpServer => "The built-in MCP server",
            Self::Agent => "The in-canvas agent",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Control => "Programs on this computer using the token of nodecraft --control, such as nodecraft-cli mcp --bridge.",
            Self::McpServer => "MCP clients on this computer connected to Settings › Automation's server with its token.",
            Self::Agent => "The agent in the sidebar, in auto mode.",
        }
    }
}

/// Whether an automation call may go ahead now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunGate {
    Ready,
    /// Not yet, for this reason (providers being set up, the person being asked). Nothing ran:
    /// the same call may be sent again.
    Waiting(&'static str),
}

/// The question the window is asking.
#[derive(Clone, Debug)]
pub(super) struct ConsentAsk {
    client: AutomationClient,
    token: String,
    price: String,
    steps: Option<usize>,
    canvas: String,
    /// What the run accepts, per currency, in micros.
    amounts: BTreeMap<String, u64>,
    /// Part of the price is unknown (`partial` or `unpriced_jobs`).
    unknown: bool,
    /// How many jobs of unknown price it may admit.
    unpriced_jobs: Option<u64>,
    /// The `accept` asked about, exactly: Allow once allows a start with this one alone, so a
    /// client can't raise what the run may spend after the person said yes.
    accept: Json,
    /// Why a channel that is allowed is asked again.
    again: Option<String>,
    /// What Always allow would remember, per currency, in whole units, as the person edits it.
    ceiling: BTreeMap<String, f64>,
    error: Option<String>,
}

/// The decisions, the open question and the one-time answers. Shared with the commands' host
/// service.
#[derive(Default)]
pub(super) struct Consent {
    /// The standing decisions, by channel key: what settings hold, and the only copy without a
    /// settings file.
    decisions: BTreeMap<String, RunConsentSetting>,
    /// A command changed them since they were last written to settings.
    dirty: bool,
    /// Allow once: the `accept` (quote token and what it may spend) each channel may start one
    /// time.
    once: BTreeMap<AutomationClient, Json>,
    /// Decline: the token refused, told once.
    declined: BTreeMap<AutomationClient, String>,
    asking: Option<ConsentAsk>,
}

pub(super) type SharedConsent = Rc<RefCell<Consent>>;

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the window is busy with paid-run consent; try again")
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// `micros` of `code`, as the person reads it: `0.04 USD`.
pub(super) fn money(code: &str, micros: u64) -> String {
    let whole = micros / 1_000_000;
    let mut fraction = format!("{:06}", micros % 1_000_000);
    while fraction.len() > 2 && fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole}.{fraction} {code}")
}

fn amounts_text(amounts: &BTreeMap<String, u64>) -> String {
    amounts.iter().map(|(code, micros)| money(code, *micros)).collect::<Vec<_>>().join(" + ")
}

/// Whole units to micros, for a ceiling the person typed: `None` unless it is above zero.
fn to_micros(units: f64) -> Option<u64> {
    if !units.is_finite() || units <= 0.0 {
        return None;
    }
    let micros = (units.min(MAX_CEILING_UNITS) * 1_000_000.0).round();
    // In range by the clamp above, so the cast neither wraps nor saturates.
    (micros >= 1.0).then_some(micros as u64)
}

impl Consent {
    /// Takes over `old`'s decisions, for a replacement session. One-time answers were about the
    /// old project's runs, and stay with it.
    pub(super) fn carry_from(&mut self, mut old: std::cell::RefMut<'_, Self>) {
        self.decisions = std::mem::take(&mut old.decisions);
        self.dirty = old.dirty;
    }

    fn decision(&self, client: AutomationClient) -> (RunConsentDecision, BTreeMap<String, u64>) {
        self.decisions.get(client.key()).map_or((RunConsentDecision::Ask, BTreeMap::new()), |setting| (setting.decision(), setting.ceiling.clone()))
    }

    /// What `automation.run_consent_list` answers.
    fn list(&self) -> Json {
        let clients: Vec<Json> = AutomationClient::ALL
            .into_iter()
            .map(|client| {
                let (decision, ceiling) = self.decision(client);
                let ceiling = if decision == RunConsentDecision::Always { ceiling } else { BTreeMap::new() };
                json!({"client": client.key(), "decision": decision.name(), "ceiling": ceiling})
            })
            .collect();
        let asking = self.asking.as_ref().map(|ask| json!({"client": ask.client.key(), "token": ask.token}));
        json!({"clients": clients, "asking": asking})
    }

    fn set(&mut self, client: AutomationClient, decision: RunConsentDecision, ceiling: BTreeMap<String, u64>) {
        match decision {
            RunConsentDecision::Ask => {
                self.decisions.remove(client.key());
                self.once.remove(&client);
                self.declined.remove(&client);
            }
            RunConsentDecision::Always => {
                self.decisions.insert(client.key().into(), RunConsentSetting::always(ceiling));
            }
            RunConsentDecision::Never => {
                self.decisions.insert(client.key().into(), RunConsentSetting::never());
                self.once.remove(&client);
            }
        }
        self.dirty = true;
    }

    fn answer(&mut self, answer: &str, ceiling: BTreeMap<String, u64>) -> Result<(), Error> {
        let ask = self.asking.as_ref().ok_or_else(|| Error::new(ErrorKind::NotFound, "no paid run from automation is waiting for an answer"))?;
        let (client, token, accept) = (ask.client, ask.token.clone(), ask.accept.clone());
        match answer {
            "once" => {
                self.once.insert(client, accept);
            }
            "always" => {
                // It must cover the run being asked about, or that run would ask again at once.
                let short = ask.amounts.iter().find(|(code, micros)| ceiling.get(*code).is_none_or(|max| max < *micros));
                if let Some((code, micros)) = short {
                    return Err(invalid(&format!("the ceiling must be at least what this run may spend, {}", money(code, *micros))));
                }
                let unknown = ask.unknown;
                self.set(client, RunConsentDecision::Always, ceiling);
                // A run with a part of unknown price is never covered by a ceiling; this one
                // was allowed, so it may start.
                if unknown {
                    self.once.insert(client, accept);
                }
            }
            "never" => self.set(client, RunConsentDecision::Never, BTreeMap::new()),
            "decline" => {
                self.declined.insert(client, token);
            }
            _ => return Err(invalid("`answer` is `once`, `always`, `never` or `decline`")),
        }
        self.asking = None;
        Ok(())
    }
}

/// Registers the consent commands' host service on `session`, sharing `consent`.
pub(super) fn register(session: &mut Session, consent: &SharedConsent) -> Result<(), Error> {
    let shared = Rc::clone(consent);
    session.register_host_service(SERVICE, move |request: &Json| {
        let mut consent = shared.try_borrow_mut().map_err(|_| busy())?;
        let ceiling = || -> BTreeMap<String, u64> {
            request.get("ceiling").and_then(Json::as_object).into_iter().flatten().filter_map(|(code, micros)| Some((code.clone(), micros.as_u64()?))).collect()
        };
        match request.get("op").and_then(Json::as_str) {
            Some("list") => {}
            Some("set") => {
                let client = request.get("client").and_then(Json::as_str).and_then(AutomationClient::from_key).ok_or_else(|| invalid("unknown client"))?;
                let decision = request.get("decision").and_then(Json::as_str).and_then(RunConsentDecision::parse).ok_or_else(|| invalid("unknown decision"))?;
                consent.set(client, decision, ceiling());
            }
            Some("answer") => {
                let answer = request.get("answer").and_then(Json::as_str).ok_or_else(|| invalid("malformed answer"))?;
                consent.answer(answer, ceiling())?;
            }
            _ => return Err(invalid("malformed consent request")),
        }
        Ok(consent.list())
    })
}

/// What a `run.start`'s `accept` says the run may spend.
struct Accepted<'a> {
    token: &'a str,
    amounts: BTreeMap<String, u64>,
    unknown: bool,
}

impl<'a> Accepted<'a> {
    /// Read strictly: what can't be read here is refused rather than let past the gate.
    fn read(params: &'a Json) -> Result<Self, Error> {
        let accept = params.get("accept").and_then(Json::as_object).ok_or_else(|| invalid("run.start needs `accept` from run.quote"))?;
        let token = accept.get("token").and_then(Json::as_str).ok_or_else(|| invalid("`accept.token` must be the quote's token"))?;
        let given = accept.get("amounts").and_then(Json::as_object).ok_or_else(|| invalid("`accept.amounts` must map currencies to micros"))?;
        if given.len() > MAX_ACCEPTED_CURRENCIES {
            return Err(invalid("`accept.amounts` names too many currencies"));
        }
        let mut amounts = BTreeMap::new();
        for (code, micros) in given {
            let micros = micros.as_u64().ok_or_else(|| invalid("`accept.amounts` must be whole micros"))?;
            amounts.insert(code.clone(), micros);
        }
        // Anything other than a plain "no" counts as a part of unknown price.
        let partial = accept.get("partial").is_some_and(|partial| partial.as_bool() != Some(false));
        let unpriced = accept.get("unpriced_jobs").is_some_and(|jobs| jobs.as_u64() != Some(0));
        Ok(Self { token, amounts, unknown: partial || unpriced })
    }

    /// Accepted entirely at no charge: it can't spend anything.
    fn free(&self) -> bool {
        !self.unknown && self.amounts.values().all(|micros| *micros == 0)
    }
}

impl NodeCraftApp {
    /// The consent state and its commands, for a new session.
    pub(super) fn new_consent(session: &mut Session) -> Result<SharedConsent, Error> {
        let consent = SharedConsent::default();
        register(session, &consent)?;
        Ok(consent)
    }

    /// Takes the decisions from settings, unless a command changed them and they aren't written
    /// yet.
    pub(super) fn load_consent(&mut self, decisions: &BTreeMap<String, RunConsentSetting>) {
        if let Ok(mut consent) = self.consent.try_borrow_mut()
            && !consent.dirty
        {
            consent.decisions = decisions.clone();
        }
    }

    /// Writes decisions a command changed to settings.
    pub(super) fn sync_consent_settings(&mut self) {
        let Ok(mut consent) = self.consent.try_borrow_mut() else { return };
        if !consent.dirty {
            return;
        }
        consent.dirty = false;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store {
            let decisions = consent.decisions.clone();
            store.update(|settings| settings.automation.run_consent = decisions);
        }
    }

    /// Whether `id` is a run command an automation client's call is gated on.
    pub fn is_gated_run_command(id: &str) -> bool {
        RUN_COMMANDS.contains(&id)
    }

    /// Before an automation client's run command reaches the session (#1079): routes the cards it
    /// names to their providers, and before a `run.start` that may spend money checks the
    /// person's consent for `client`, asking them the first time. `approved` is a run the person
    /// approved on its own (the agent's Approve). Any other command is [`RunGate::Ready`].
    ///
    /// Calling it again with the same call is how a waiting client goes on: nothing is started
    /// here, and an Allow once is used up only by [`Self::automation_run_started`].
    pub fn automation_run_gate(&mut self, client: AutomationClient, id: &str, params: &Json, approved: bool) -> Result<RunGate, Error> {
        if !Self::is_gated_run_command(id) {
            return Ok(RunGate::Ready);
        }
        if let RunGate::Waiting(why) = self.route_for_automation(id, params)? {
            return Ok(RunGate::Waiting(why));
        }
        if id != "run.start" || approved {
            return Ok(RunGate::Ready);
        }
        self.consent_gate(client, params)
    }

    /// After a gated `run.start` started: the Allow once it used (by its quote token) is gone.
    pub fn automation_run_started(&mut self, client: AutomationClient, id: &str, params: &Json) {
        if id != "run.start" {
            return;
        }
        let token = params.get("accept").and_then(|accept| accept.get("token")).and_then(Json::as_str);
        if let (Some(token), Ok(mut consent)) = (token, self.consent.try_borrow_mut())
            && consent.once.get(&client).is_some_and(|once| once.get("token").and_then(Json::as_str) == Some(token))
        {
            consent.once.remove(&client);
        }
    }

    /// Routes the cards a run command names, as Generate does. Parameters that can't be read
    /// are left for the engine to refuse.
    fn route_for_automation(&mut self, id: &str, params: &Json) -> Result<RunGate, Error> {
        if self.runs.is_none() {
            return Ok(RunGate::Ready);
        }
        let Some(canvas) = params.get("canvas").and_then(|canvas| serde_json::from_value::<CanvasId>(canvas.clone()).ok()) else {
            return Ok(RunGate::Ready);
        };
        let mode = match id {
            "run.changed" => nodecraft_engine::run_cmds::CHANGED_MODE,
            "run.workflow" => nodecraft_engine::run_cmds::WORKFLOW_MODE,
            _ => params.get("mode").and_then(Json::as_str).unwrap_or_default(),
        };
        let kind = match mode {
            "selected" => RunKind::Generate,
            "with_upstream" | "field" => RunKind::Refresh,
            "all_stale" | nodecraft_engine::run_cmds::CHANGED_MODE => RunKind::Changed,
            nodecraft_engine::run_cmds::WORKFLOW_MODE => RunKind::Workflow,
            _ => return Ok(RunGate::Ready),
        };
        if canvas == self.canvas && kind != RunKind::Generate {
            self.refresh_stale_badges();
        }
        let stale = if canvas == self.canvas { self.stale_badges.as_ref().map(|badges| badges.nodes.clone()).unwrap_or_default() } else { BTreeSet::new() };
        let nodes: Vec<NodeId> = if kind.whole() {
            super::runs::whole_roots(&self.session, canvas, kind == RunKind::Workflow, &stale)
        } else if mode == "field" {
            let field = params.get("field").and_then(|field| serde_json::from_value::<FieldId>(field.clone()).ok());
            let on = self.session.document().canvases().get(&canvas);
            on.into_iter().flat_map(|on| on.nodes().values()).filter(|node| field.is_some() && node.field == field).map(|node| node.id).collect()
        } else {
            params.get("nodes").and_then(|nodes| serde_json::from_value(nodes.clone()).ok()).unwrap_or_default()
        };
        let settings = self.provider_settings();
        let shown = self.canvas;
        let Some(runs) = &mut self.runs else { return Ok(RunGate::Ready) };
        Ok(match runs.route_automation(&mut self.session, canvas, &nodes, kind, &stale, &settings, shown)? {
            super::runs::Route::Waiting => RunGate::Waiting(SETTING_UP),
            super::runs::Route::Ready => RunGate::Ready,
        })
    }

    fn consent_gate(&mut self, client: AutomationClient, params: &Json) -> Result<RunGate, Error> {
        let accepted = Accepted::read(params)?;
        if accepted.free() {
            return Ok(RunGate::Ready);
        }
        let again = {
            let mut consent = self.consent.try_borrow_mut().map_err(|_| busy())?;
            if consent.declined.get(&client).is_some_and(|token| token == accepted.token) {
                consent.declined.remove(&client);
                return Err(Error::new(ErrorKind::Forbidden, DECLINED));
            }
            if consent.once.get(&client).is_some_and(|once| Some(once) == params.get("accept")) {
                return Ok(RunGate::Ready);
            }
            let (decision, ceiling) = consent.decision(client);
            let over = accepted.amounts.iter().filter(|(_, micros)| **micros > 0).find(|(code, micros)| ceiling.get(*code).is_none_or(|max| max < *micros));
            let again = match decision {
                RunConsentDecision::Never => return Err(Error::new(ErrorKind::Forbidden, NEVER)),
                RunConsentDecision::Always if accepted.unknown => Some("Part of this run's price isn't known yet, so it asks each time.".to_owned()),
                RunConsentDecision::Always => match over {
                    None => return Ok(RunGate::Ready),
                    Some((code, _)) => Some(match ceiling.get(code) {
                        Some(max) => format!("This run may cost more than the {} per run you allowed.", money(code, *max)),
                        None => format!("You allowed runs priced in other currencies, not {code}."),
                    }),
                },
                RunConsentDecision::Ask => None,
            };
            if let Some(ask) = &consent.asking {
                return Ok(RunGate::Waiting(if ask.client == client && ask.token == accepted.token { ASKING } else { ASKING_OTHER }));
            }
            again
        };
        let ask = self.consent_question(client, params, &accepted, again);
        self.consent.try_borrow_mut().map_err(|_| busy())?.asking = Some(ask);
        Ok(RunGate::Waiting(ASKING))
    }

    /// What the window shows about the run: its price as quoted, else what it accepts.
    fn consent_question(&mut self, client: AutomationClient, params: &Json, accepted: &Accepted<'_>, again: Option<String>) -> ConsentAsk {
        let mut quote_params = params.clone();
        if let Some(object) = quote_params.as_object_mut() {
            object.remove("accept");
        }
        // Never waiting on a background save: without a price the dialog shows what it accepts.
        let quote = self.session.dispatch_without_waiting("run.quote", &quote_params).ok().as_ref().and_then(Quote::read);
        let price = match &quote {
            Some(quote) if quote.token() == accepted.token => quote.price(),
            _ if accepted.unknown => format!("Up to {}, and a part of unknown price", amounts_text(&accepted.amounts)),
            _ => format!("Up to {}", amounts_text(&accepted.amounts)),
        };
        let steps = quote.as_ref().filter(|quote| quote.token() == accepted.token).map(Quote::steps_total);
        let canvas = params
            .get("canvas")
            .and_then(|canvas| serde_json::from_value::<CanvasId>(canvas.clone()).ok())
            .and_then(|canvas| self.session.document().canvases().get(&canvas).map(|canvas| canvas.title().chars().take(80).collect()))
            .unwrap_or_default();
        let ceiling = accepted.amounts.iter().filter(|(_, micros)| **micros > 0).map(|(code, micros)| (code.clone(), *micros as f64 / 1_000_000.0)).collect();
        ConsentAsk {
            client,
            token: accepted.token.to_owned(),
            price,
            steps,
            canvas,
            amounts: accepted.amounts.iter().filter(|(_, micros)| **micros > 0).map(|(code, micros)| (code.clone(), *micros)).collect(),
            unknown: accepted.unknown,
            unpriced_jobs: params.get("accept").and_then(|accept| accept.get("unpriced_jobs")).and_then(Json::as_u64).filter(|jobs| *jobs > 0),
            accept: params.get("accept").cloned().unwrap_or(Json::Null),
            again,
            ceiling,
            error: None,
        }
    }

    /// Whether the window is asking about a paid run from automation.
    pub fn consent_asking(&self) -> Option<AutomationClient> {
        self.consent.try_borrow().ok().and_then(|consent| consent.asking.as_ref().map(|ask| ask.client))
    }

    /// The question about an automation client's paid run. Returns whether it owns this frame's
    /// input.
    pub(super) fn consent_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        self.sync_consent_settings();
        let Some(mut ask) = self.consent.try_borrow().ok().and_then(|consent| consent.asking.clone()) else {
            return false;
        };
        let t = *self.theme.tokens();
        let mut answer: Option<Json> = None;
        egui::Modal::new(egui::Id::new("nodecraft.run_consent")).show(ui.ctx(), |ui| {
            ui.set_width(t.dialog_width);
            ui.heading("Allow a paid run?");
            ui.label(format!("{} wants to start a run that costs money.", ask.client.name()));
            ui.label(egui::RichText::new(ask.client.detail()).color(t.text_muted));
            ui.add_space(t.control_gap);
            ui.label(egui::RichText::new(&ask.price).color(t.text).strong());
            let mut what = Vec::new();
            if let Some(steps) = ask.steps {
                what.push(if steps == 1 { "1 step".to_owned() } else { format!("{steps} steps") });
            }
            if !ask.canvas.is_empty() {
                what.push(format!("on {}", ask.canvas));
            }
            if !what.is_empty() {
                ui.label(what.join(" "));
            }
            // What the client accepted is what it may spend, whatever the quote said.
            let limit = if ask.amounts.is_empty() { "nothing of known price".to_owned() } else { amounts_text(&ask.amounts) };
            let jobs = ask.unpriced_jobs.map_or_else(String::new, |jobs| format!(", and {jobs} job(s) of unknown price"));
            ui.label(format!("It may spend up to {limit}{jobs}."));
            if ask.unknown {
                ui.label("Part of the price is only known while it runs. The run stops at what it accepted, and its budget caps still apply.");
            }
            if let Some(again) = &ask.again {
                ui.label(again);
            }
            ui.add_space(t.control_gap);
            if !ask.ceiling.is_empty() {
                ui.label(egui::RichText::new("Always allow runs from it costing up to, per run:").color(t.text_muted));
                for (code, units) in ask.ceiling.iter_mut() {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(units).range(0.000001..=MAX_CEILING_UNITS).speed(0.01).max_decimals(6));
                        ui.label(code.as_str());
                    });
                }
            }
            if let Some(error) = &ask.error {
                ui.colored_label(t.danger, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Allow once").on_hover_text("Start this run, and ask again next time").clicked() {
                    answer = Some(json!({"answer": "once"}));
                }
                let ceiling: Option<BTreeMap<String, u64>> = ask.ceiling.iter().map(|(code, units)| Some((code.clone(), to_micros(*units)?))).collect();
                let always = ui
                    .add_enabled(ceiling.as_ref().is_some_and(|ceiling| !ceiling.is_empty()), egui::Button::new("Always allow"))
                    .on_hover_text("Remember: runs from it up to the amount above start without asking. Revoke it in the Automation panel.");
                if always.clicked()
                    && let Some(ceiling) = ceiling
                {
                    answer = Some(json!({"answer": "always", "ceiling": ceiling}));
                }
                if ui.button("Never").on_hover_text("Remember: refuse paid runs from it. Change it in the Automation panel.").clicked() {
                    answer = Some(json!({"answer": "never"}));
                }
                if ui.button("Not now").on_hover_text("Refuse this run only").clicked() {
                    answer = Some(json!({"answer": "decline"}));
                }
            });
        });
        match answer {
            Some(answer) => {
                if let Err(error) = self.session.dispatch("automation.run_consent_answer", &answer) {
                    ask.error = Some(error.message);
                    self.keep_asking(ask);
                }
            }
            None => self.keep_asking(ask),
        }
        true
    }

    /// Keeps the person's edits to the open question, if it is still the one asked.
    fn keep_asking(&mut self, ask: ConsentAsk) {
        if let Ok(mut consent) = self.consent.try_borrow_mut()
            && let Some(open) = &mut consent.asking
            && open.client == ask.client
            && open.token == ask.token
        {
            *open = ask;
        }
    }

    /// Settings › Automation › Paid runs: what each channel may do, and a way to take it back.
    pub(super) fn consent_section(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        ui.add_space(t.control_gap);
        ui.label(egui::RichText::new("Paid runs").color(t.text).strong());
        ui.label(egui::RichText::new("Whether automation may start runs that cost money. Quotes and budget caps apply either way.").color(t.text_muted));
        let decisions: Vec<(AutomationClient, RunConsentDecision, BTreeMap<String, u64>)> = match self.consent.try_borrow() {
            Ok(consent) => AutomationClient::ALL.into_iter().map(|client| (client, consent.decision(client))).map(|(c, (d, m))| (c, d, m)).collect(),
            Err(_) => return,
        };
        let mut set: Option<(AutomationClient, &'static str)> = None;
        for (client, decision, ceiling) in decisions {
            ui.horizontal(|ui| {
                let state = match decision {
                    RunConsentDecision::Ask => "asks first".to_owned(),
                    RunConsentDecision::Never => "never".to_owned(),
                    RunConsentDecision::Always => format!("allowed up to {} per run", amounts_text(&ceiling)),
                };
                ui.label(format!("{}: {state}", client.name()));
                if decision != RunConsentDecision::Ask && ui.button(format!("Ask again ({})", client.key())).on_hover_text("Forget this decision").clicked() {
                    set = Some((client, "ask"));
                }
                if decision != RunConsentDecision::Never && ui.button(format!("Never ({})", client.key())).clicked() {
                    set = Some((client, "never"));
                }
            });
        }
        if let Some((client, decision)) = set {
            let refused = self.session.dispatch("automation.run_consent_set", &json!({"client": client.key(), "decision": decision})).err().map(|e| e.message);
            self.automation_status(refused);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_and_ceilings_read_as_people_write_them() {
        assert_eq!(money("USD", 40_000), "0.04 USD");
        assert_eq!(money("USD", 1_500_000), "1.50 USD");
        assert_eq!(money("EUR", 1_234_567), "1.234567 EUR");
        assert_eq!(money("USD", 0), "0.00 USD");
        assert_eq!(money("USD", u64::MAX), "18446744073709.551615 USD");
        assert_eq!(to_micros(0.04), Some(40_000));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-9] {
            assert_eq!(to_micros(bad), None, "{bad}");
        }
        assert_eq!(to_micros(1e30), Some(1_000_000_000_000_000));
    }

    #[test]
    fn the_gate_reads_accept_strictly() {
        let read = |accept: Json| Accepted::read(&json!({"accept": accept})).map(|a| (a.free(), a.unknown));
        assert_eq!(read(json!({"token": "t", "amounts": {}})).unwrap(), (true, false));
        assert_eq!(read(json!({"token": "t", "amounts": {"USD": 0}})).unwrap(), (true, false));
        assert_eq!(read(json!({"token": "t", "amounts": {"USD": 1}})).unwrap(), (false, false));
        assert_eq!(read(json!({"token": "t", "amounts": {}, "partial": true})).unwrap(), (false, true));
        assert_eq!(read(json!({"token": "t", "amounts": {}, "partial": "yes"})).unwrap(), (false, true));
        assert_eq!(read(json!({"token": "t", "amounts": {}, "unpriced_jobs": 2})).unwrap(), (false, true));
        assert_eq!(read(json!({"token": "t", "amounts": {}, "unpriced_jobs": 0, "partial": false})).unwrap(), (true, false));
        for bad in [
            json!({"amounts": {}}),
            json!({"token": "t"}),
            json!({"token": "t", "amounts": {"USD": 0.5}}),
            json!({"token": "t", "amounts": {"USD": -1}}),
            json!(null),
        ] {
            assert_eq!(read(bad.clone()).unwrap_err().kind, ErrorKind::InvalidInput, "{bad}");
        }
        assert!(Accepted::read(&json!([])).is_err());
    }

    #[test]
    fn decisions_are_listed_set_and_forgotten() {
        let mut consent = Consent::default();
        assert_eq!(consent.list()["clients"][0], json!({"client": "control", "decision": "ask", "ceiling": {}}));
        consent.set(AutomationClient::Control, RunConsentDecision::Always, BTreeMap::from([("USD".into(), 5)]));
        consent.once.insert(AutomationClient::Control, "t".into());
        assert!(consent.dirty);
        assert_eq!(consent.list()["clients"][0], json!({"client": "control", "decision": "always", "ceiling": {"USD": 5}}));
        consent.set(AutomationClient::Control, RunConsentDecision::Ask, BTreeMap::new());
        assert!(consent.once.is_empty(), "asking again forgets an unused Allow once");
        assert_eq!(consent.list()["clients"][0]["decision"], "ask");
        assert_eq!(consent.answer("once", BTreeMap::new()).unwrap_err().kind, ErrorKind::NotFound, "nothing is asked");
        assert_eq!(nodecraft_engine::settings::RUN_CONSENT_CLIENTS, AutomationClient::ALL.map(AutomationClient::key));
    }
}
