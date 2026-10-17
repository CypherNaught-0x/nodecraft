//! The API keys panel: list provider accounts, check whether each has a key, add and remove keys.
//!
//! Every action is a `keys.*` command dispatched through the app's session. In the app the
//! `keys.store` service runs on a keychain thread (`engine::secrets::KeyWorker`), so commands
//! answer `{pending: id}` and the outcome arrives later through `KeyResults`. The panel accepts
//! both that and an immediate answer (tests, the browser build). Keys are never shown: only
//! the masked tail the engine returns. A typed key is cleared from the field as soon as it is sent.
//!
//! Accounts are listed from settings (`[keys] accounts`) and never read at startup, since each
//! keychain read can show a prompt; "Check" reads one on request.

use std::collections::BTreeMap;

use nodecraft_engine::Session;
use nodecraft_engine::secrets::Account;
use nodecraft_engine::settings::KeySettings;
use nodecraft_types::Error;
use serde_json::{Value, json};

use crate::theme::Tokens;

/// Common providers, offered as quick picks.
const PRESETS: &[&str] = &["openai", "anthropic", "openrouter", "fal", "replicate"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    /// Not read yet (reading can prompt, so it waits for "Check").
    Unchecked,
    /// A request is with the keychain.
    Waiting,
    /// A key is stored; its masked tail (`…abcd` or `…`).
    Stored(String),
    Missing,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Set,
    Remove,
    Status,
}

/// A change to the remembered account list, for the host to save in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChange {
    Remember(Account),
    Forget(Account),
}

#[derive(Default)]
pub struct KeysPanel {
    pub open: bool,
    rows: Vec<(Account, KeyStatus)>,
    loaded: bool,
    account: String,
    key: String,
    /// Requests the keychain hasn't answered: id → (account, what was asked).
    pending: BTreeMap<u64, (Account, Op)>,
    confirm_remove: Option<Account>,
    pub notice: Option<String>,
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
        };
        match session.dispatch(id, &params) {
            Ok(answer) => match answer.get("pending").and_then(Value::as_u64) {
                Some(request) => {
                    self.pending.insert(request, (account.clone(), op));
                    self.set_status(account, KeyStatus::Waiting);
                    None
                }
                None => self.apply(account, op, Ok(answer)),
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
                Some((account, op)) => changes.extend(self.apply(&account, op, outcome.result)),
                None => others.push(outcome),
            }
        }
        (changes, others)
    }

    /// Draws the panel (when open) and returns account-list changes to save.
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session, settings: &KeySettings, t: &Tokens) -> Vec<AccountChange> {
        let mut changes = Vec::new();
        if !self.open {
            return changes;
        }
        self.load(settings);
        let mut open = self.open;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("API keys").id(egui::Id::new("nodecraft.keys")).open(&mut open).collapsible(false).resizable(false).frame(frame).show(ctx, |ui| {
            ui.label(egui::RichText::new("Keys are kept in your system keychain, never in projects or settings.").color(t.text_muted));
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
                        KeyStatus::Failed(why) => (why.clone(), t.accent),
                    };
                    ui.label(egui::RichText::new(text).color(color));
                    ui.horizontal(|ui| {
                        let waiting = *status == KeyStatus::Waiting;
                        if ui.add_enabled(!waiting, egui::Button::new("Check")).clicked() {
                            changes.extend(self.send(session, Op::Status, account, json!({"account": account.as_str()})));
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
                    ui.add(egui::TextEdit::singleline(&mut self.key).password(true).hint_text("paste the key").desired_width(t.card_width))
                        .labelled_by(label.id);
                    if ui.add_enabled(!self.key.trim().is_empty(), egui::Button::new("Save")).clicked() {
                        // The field is cleared whatever happens: the key lives only in the keychain.
                        let key = std::mem::take(&mut self.key);
                        match Account::new(self.account.trim()) {
                            Ok(account) => changes.extend(self.send(session, Op::Set, &account, json!({"account": account.as_str(), "key": key}))),
                            Err(e) => self.notice = Some(e.to_string()),
                        }
                    }
                });
                ui.end_row();
            });
            if let Some(notice) = &self.notice {
                ui.label(egui::RichText::new(notice).color(t.accent));
            }
        });
        self.open = open;
        changes
    }
}

#[cfg(test)]
mod tests;
