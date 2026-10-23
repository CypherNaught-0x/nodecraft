//! The API keys panel: list provider accounts, check whether each has a key, add and remove keys.
//!
//! Every action is a `keys.*` command dispatched through the app's session. In the app the
//! `keys.store` service runs on a keychain thread (`engine::secrets::KeyWorker`), so commands
//! answer `{pending: id}` and the outcome arrives later through `KeyResults`. The panel accepts
//! both that and an immediate answer (tests, and the browser build, which keeps keys in the
//! browser's storage for the site). Keys are never shown: only the masked tail the engine returns.
//! A typed key is cleared from the field as soon as it is sent.
//!
//! Accounts are listed from settings (`[keys] accounts`) and never read at startup, since each
//! keychain read can show a prompt; "Check" reads one on request. The browser build also lists
//! the accounts it finds in the site's storage at startup (names only; no key is read).
//!
//! "Test" (desktop only) asks the provider whether it accepts the stored key, with a request that
//! costs nothing (`keys.test`, #237): the row says it works, can't be verified (and why), or the
//! error, such as the provider rejecting it.

use std::collections::BTreeMap;

use nodecraft_engine::Session;
use nodecraft_engine::secrets::Account;
use nodecraft_engine::settings::KeySettings;
use nodecraft_types::Error;
use serde_json::{Value, json};

use crate::theme::Tokens;

/// How long a request may wait for the keychain (an unanswered unlock prompt) before the row
/// says so and can be used again. A late answer is then ignored.
pub const KEYCHAIN_TIMEOUT_S: f64 = 120.0;

/// Where keys are kept, shown at the top of the panel.
#[cfg(not(target_arch = "wasm32"))]
pub const STORED_WHERE: &str = "Keys are kept in your system keychain, never in projects or settings.";
/// The browser has no keychain (#118): say plainly who else can read the keys.
#[cfg(target_arch = "wasm32")]
pub const STORED_WHERE: &str = "Keys are kept in this browser's storage for this site only, never in projects or settings. \
    Anyone who uses this browser profile can read them, so remove them when you finish on a shared computer.";

/// Common providers, offered as quick picks.
const PRESETS: &[&str] =
    &["openai", "anthropic", "openrouter", "litellm", "fal", "replicate", "seedream", "tripo", "meshy", "higgsfield", "kling", "elevenlabs"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    /// Not read yet (reading can prompt, so it waits for "Check").
    Unchecked,
    /// A request is with the keychain.
    Waiting,
    /// A key is stored; its masked tail (`…abcd` or `…`).
    Stored(String),
    Missing,
    /// A `keys.test` check is with the provider.
    Testing,
    /// The provider accepted the key; its masked tail.
    Verified(String),
    /// The key (its masked tail) can't be checked for free; why.
    Unverifiable(String, String),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Set,
    Remove,
    Status,
    Test,
}

/// A change to the remembered account list, for the host to save in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChange {
    Remember(Account),
    Forget(Account),
}

pub struct KeysPanel {
    pub open: bool,
    /// Seconds a request may wait for the keychain ([`KEYCHAIN_TIMEOUT_S`]).
    pub timeout_s: f64,
    rows: Vec<(Account, KeyStatus)>,
    loaded: bool,
    account: String,
    key: String,
    /// Requests the keychain hasn't answered: id → (account, what was asked, when, in egui
    /// seconds).
    pending: BTreeMap<u64, (Account, Op, f64)>,
    /// egui time of the current frame.
    now: f64,
    /// Whether the panel was drawn last frame, to notice it closing.
    was_open: bool,
    confirm_remove: Option<Account>,
    pub notice: Option<String>,
}

impl Default for KeysPanel {
    fn default() -> Self {
        Self {
            open: false,
            timeout_s: KEYCHAIN_TIMEOUT_S,
            rows: Vec::new(),
            loaded: false,
            account: String::new(),
            key: String::new(),
            pending: BTreeMap::new(),
            now: 0.0,
            was_open: false,
            confirm_remove: None,
            notice: None,
        }
    }
}

impl KeysPanel {
    /// Whether a request is waiting for the keychain.
    pub fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The listed accounts and what is known about each.
    pub fn rows(&self) -> &[(Account, KeyStatus)] {
        &self.rows
    }

    /// Lists `settings`' accounts the first time the panel is shown.
    fn load(&mut self, settings: &KeySettings) {
        if !self.loaded {
            self.rows = settings.accounts().into_iter().map(|a| (a, KeyStatus::Unchecked)).collect();
            self.loaded = true;
        }
    }

    fn set_status(&mut self, account: &Account, status: KeyStatus) {
        match self.rows.iter_mut().find(|(a, _)| a == account) {
            Some((_, current)) => *current = status,
            None => self.rows.push((account.clone(), status)),
        }
    }

    /// Dispatches a `keys.*` command and records the request or its immediate answer.
    fn send(&mut self, session: &mut Session, op: Op, account: &Account, params: Value) -> Option<AccountChange> {
        let id = match op {
            Op::Set => "keys.set",
            Op::Remove => "keys.remove",
            Op::Status => "keys.status",
            Op::Test => "keys.test",
        };
        match session.dispatch(id, &params) {
            Ok(answer) => match answer.get("pending").and_then(Value::as_u64) {
                Some(request) => {
                    self.notice = None;
                    self.pending.insert(request, (account.clone(), op, self.now));
                    self.set_status(account, if op == Op::Test { KeyStatus::Testing } else { KeyStatus::Waiting });
                    None
                }
                None => {
                    self.notice = None;
                    self.apply(account, op, Ok(answer))
                }
            },
            Err(e) => {
                self.notice = Some(e.message);
                None
            }
        }
    }

    /// Applies a keychain answer to the rows.
    fn apply(&mut self, account: &Account, op: Op, result: Result<Value, Error>) -> Option<AccountChange> {
        let answer = match result {
            Ok(answer) => answer,
            Err(e) => {
                self.set_status(account, KeyStatus::Failed(e.message));
                return None;
            }
        };
        let masked = || answer.get("masked").and_then(Value::as_str).unwrap_or("…").to_owned();
        match op {
            Op::Set => {
                self.set_status(account, KeyStatus::Stored(masked()));
                Some(AccountChange::Remember(account.clone()))
            }
            Op::Remove => {
                self.rows.retain(|(a, _)| a != account);
                Some(AccountChange::Forget(account.clone()))
            }
            Op::Status => {
                let stored = answer.get("stored").and_then(Value::as_bool).unwrap_or(false);
                self.set_status(account, if stored { KeyStatus::Stored(masked()) } else { KeyStatus::Missing });
                None
            }
            Op::Test => {
                let status = match answer.get("result").and_then(Value::as_str) {
                    Some("verified") => KeyStatus::Verified(masked()),
                    _ => KeyStatus::Unverifiable(masked(), answer.get("reason").and_then(Value::as_str).unwrap_or("no free check").to_owned()),
                };
                self.set_status(account, status);
                None
            }
        }
    }

    /// Takes the keychain's answers (from the host's `KeyResults`), matched to this panel's
    /// requests by id. Answers to requests made elsewhere are returned untouched. (Native only:
    /// the browser build has no keychain thread.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn receive(&mut self, outcomes: Vec<nodecraft_engine::secrets::KeyOutcome>) -> (Vec<AccountChange>, Vec<nodecraft_engine::secrets::KeyOutcome>) {
        let mut changes = Vec::new();
        let mut others = Vec::new();
        for outcome in outcomes {
            match self.pending.remove(&outcome.id) {
                Some((account, op, _)) => changes.extend(self.apply(&account, op, outcome.result)),
                None => others.push(outcome),
            }
        }
        (changes, others)
    }

    /// Draws the panel (when open) and returns account-list changes to save.
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session, settings: &KeySettings, t: &Tokens) -> Vec<AccountChange> {
        let mut changes = Vec::new();
        if !self.open {
            // Closed some other way (the toolbar button): still forget anything typed.
            if self.was_open {
                self.forget_typed(ctx);
                self.confirm_remove = None;
                self.was_open = false;
            }
            return changes;
        }
        self.was_open = true;
        self.tick(ctx.input(|i| i.time));
        self.load(settings);
        let mut open = self.open;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("API keys").id(egui::Id::new("nodecraft.keys")).open(&mut open).collapsible(false).resizable(false).frame(frame).show(ctx, |ui| {
            ui.label(egui::RichText::new(STORED_WHERE).color(t.text_muted));
            ui.add_space(t.control_gap);
            let rows = self.rows.clone();
            if rows.is_empty() {
                ui.label(egui::RichText::new("No keys yet.").color(t.text_muted));
            }
            egui::Grid::new("nodecraft.keys.rows").num_columns(3).spacing([t.control_gap * 2.0, t.control_gap]).show(ui, |ui| {
                for (account, status) in &rows {
                    ui.label(egui::RichText::new(account.as_str()).color(t.text).monospace());
                    let (text, color) = match status {
                        KeyStatus::Unchecked => ("Not checked".to_owned(), t.text_muted),
                        KeyStatus::Waiting => ("Waiting for the keychain…".to_owned(), t.text_muted),
                        KeyStatus::Stored(masked) => (format!("Stored {masked}"), t.text),
                        KeyStatus::Missing => ("No key".to_owned(), t.text_muted),
                        KeyStatus::Testing => ("Asking the provider…".to_owned(), t.text_muted),
                        KeyStatus::Verified(masked) => (format!("Stored {masked}, works: the provider accepted it"), t.accent),
                        KeyStatus::Unverifiable(masked, why) => (format!("Stored {masked}, can't verify: {why}"), t.text_muted),
                        KeyStatus::Failed(why) => (why.clone(), t.danger),
                    };
                    ui.label(egui::RichText::new(text).color(color));
                    ui.horizontal(|ui| {
                        let waiting = matches!(status, KeyStatus::Waiting | KeyStatus::Testing);
                        if ui.add_enabled(!waiting, egui::Button::new("Check")).clicked() {
                            changes.extend(self.send(session, Op::Status, account, json!({"account": account.as_str()})));
                        }
                        // The browser can't send the check (keys.test is unsupported there).
                        if cfg!(not(target_arch = "wasm32"))
                            && ui
                                .add_enabled(!waiting, egui::Button::new("Test"))
                                .on_hover_text("Ask the provider whether it accepts this key (free)")
                                .clicked()
                        {
                            changes.extend(self.send(session, Op::Test, account, json!({"account": account.as_str()})));
                        }
                        let confirming = self.confirm_remove.as_ref() == Some(account);
                        let label = if confirming { "Confirm remove" } else { "Remove" };
                        if ui.add_enabled(!waiting, egui::Button::new(label)).clicked() {
                            if confirming {
                                self.confirm_remove = None;
                                changes.extend(self.send(session, Op::Remove, account, json!({"account": account.as_str()})));
                            } else {
                                self.confirm_remove = Some(account.clone());
                            }
                        }
                    });
                    ui.end_row();
                }
            });
            ui.separator();
            ui.label(egui::RichText::new("Add or replace a key").color(t.text));
            ui.horizontal_wrapped(|ui| {
                for preset in PRESETS {
                    if ui.selectable_label(self.account == *preset, *preset).clicked() {
                        self.account = (*preset).to_owned();
                    }
                }
            });
            egui::Grid::new("nodecraft.keys.add").num_columns(2).spacing([t.control_gap * 2.0, t.control_gap]).show(ui, |ui| {
                let label = ui.label(egui::RichText::new("Account").color(t.text_muted));
                ui.add(egui::TextEdit::singleline(&mut self.account).hint_text("e.g. openai").desired_width(t.card_width)).labelled_by(label.id);
                ui.end_row();
                let label = ui.label(egui::RichText::new("Key").color(t.text_muted));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.key).id(Self::key_field()).password(true).hint_text("paste the key").desired_width(t.card_width),
                    )
                    .labelled_by(label.id);
                    if ui.add_enabled(!self.key.trim().is_empty(), egui::Button::new("Save")).clicked() {
                        // The field (and its undo history) is cleared whatever happens: the key
                        // lives only in the keychain.
                        let key = std::mem::take(&mut self.key);
                        self.forget_typed(ui.ctx());
                        match Account::new(self.account.trim()) {
                            Ok(account) => changes.extend(self.send(session, Op::Set, &account, json!({"account": account.as_str(), "key": key}))),
                            Err(e) => self.notice = Some(e.to_string()),
                        }
                    }
                });
                ui.end_row();
            });
            if let Some(notice) = &self.notice {
                ui.label(egui::RichText::new(notice).color(t.danger));
            }
        });
        if !open {
            // Closing forgets anything typed: the key lives only in the keychain.
            self.forget_typed(ctx);
            self.confirm_remove = None;
        }
        self.open = open;
        changes
    }

    /// The key field's id, so its undo history can be cleared.
    fn key_field() -> egui::Id {
        egui::Id::new("nodecraft.keys.key")
    }

    /// Clears the typed key and the field's undo history, which would otherwise bring it back.
    fn forget_typed(&mut self, ctx: &egui::Context) {
        self.key.clear();
        if let Some(mut state) = egui::TextEdit::load_state(ctx, Self::key_field()) {
            state.clear_undoer();
            state.store(ctx, Self::key_field());
        }
    }

    /// Advances the panel's clock to `now` (egui seconds) and fails requests the keychain never
    /// answered, so they can be retried. The host calls it every frame, open or not.
    pub fn tick(&mut self, now: f64) {
        self.now = now;
        self.expire();
    }

    /// Requests the keychain never answered become failures the user can retry.
    fn expire(&mut self) {
        let (now, limit) = (self.now, self.timeout_s);
        let late: Vec<u64> = self.pending.iter().filter(|(_, (_, _, since))| now - since > limit).map(|(id, _)| *id).collect();
        for id in late {
            if let Some((account, _, _)) = self.pending.remove(&id) {
                self.set_status(&account, KeyStatus::Failed("No answer from the keychain (is an unlock prompt open?). Try again.".into()));
            }
        }
    }
}

#[cfg(test)]
mod tests;
