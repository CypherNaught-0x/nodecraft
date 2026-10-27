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
//! A request the keychain hasn't answered within [`KEYCHAIN_TIMEOUT_S`] (an unlock prompt left
//! open) makes its row say the outcome is unknown, and the row can be used again. The keychain call
//! itself is not cancelled and may still succeed, so the panel keeps following the request (up to
//! [`MAX_PENDING`] at once) and applies its answer when it comes: a late save is remembered, a late
//! removal forgotten. A row shows only its newest request's answer; an older one still updates the
//! remembered accounts, which stays right because the keychain worker answers in request order.
//!
//! At most [`KeySettings::MAX_ACCOUNTS`] accounts are remembered. Saving a key for a new account
//! counts the new accounts with a save still in flight (or unknown) as taken, and is refused before
//! anything is written when the list is full; replacing a listed account's key is always allowed.
//! A save that fails releases its place.
//!
//! "Test" (desktop only) asks the provider whether it accepts the stored key, with a request that
//! costs nothing (`keys.test`, #237): the row says it works, can't be verified (and why), or the
//! error, such as the provider rejecting it.

use std::collections::{BTreeMap, BTreeSet};

use nodecraft_engine::Session;
use nodecraft_engine::secrets::Account;
use nodecraft_engine::settings::KeySettings;
use nodecraft_types::Error;
use serde_json::{Value, json};

use crate::theme::Tokens;

/// How long a request may wait for the keychain (an unanswered unlock prompt) before the row
/// says its outcome is unknown and can be used again. A late answer is still applied.
pub const KEYCHAIN_TIMEOUT_S: f64 = 120.0;

/// Most keychain requests the panel follows at once, answered or timed out; more are refused
/// until some are answered, so no answer is ever lost. The keychain worker itself holds far fewer.
pub const MAX_PENDING: usize = 256;

/// Where keys are kept, shown at the top of the panel.
#[cfg(not(target_arch = "wasm32"))]
pub const STORED_WHERE: &str = "Keys are kept in your system keychain, never in projects or settings.";
/// The browser has no keychain (#118): say plainly who else can read the keys.
#[cfg(target_arch = "wasm32")]
pub const STORED_WHERE: &str = "Keys are kept in this browser's storage for this site only, never in projects or settings. \
    Anyone who uses this browser profile can read them, so remove them when you finish on a shared computer.";

/// Common providers, offered as quick picks.
const PRESETS: &[&str] = &[
    "openai",
    "azure-openai",
    "anthropic",
    "openrouter",
    "litellm",
    "fal",
    "stability",
    "replicate",
    "runway",
    "seedream",
    "tripo",
    "meshy",
    "higgsfield",
    "kling",
    "elevenlabs",
];

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
    /// The keychain didn't answer in time, so the request may still finish (its answer is applied
    /// when it comes); what the user can do meanwhile.
    Unknown(String),
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
    /// A key was saved for the account.
    Remember(Account),
    /// The account's key was removed.
    Forget(Account),
    /// A check found a key stored for the account: list it (no key changed).
    Found(Account),
}

/// A request the keychain hasn't answered yet.
#[derive(Debug, Clone)]
struct Request {
    account: Account,
    op: Op,
    /// When it was sent, in egui seconds.
    since: f64,
    /// The panel's own order of requests, to tell newer from older answers.
    seq: u64,
    /// Past the timeout: the row says the outcome is unknown, but the answer is still awaited.
    overdue: bool,
}

pub struct KeysPanel {
    pub open: bool,
    /// Seconds a request may wait for the keychain ([`KEYCHAIN_TIMEOUT_S`]).
    pub timeout_s: f64,
    rows: Vec<(Account, KeyStatus)>,
    loaded: bool,
    account: String,
    key: String,
    /// Requests the keychain hasn't answered, by `pending` id (at most [`MAX_PENDING`]).
    pending: BTreeMap<u64, Request>,
    /// Each listed account's newest request: only its answer may change the row.
    latest: BTreeMap<Account, u64>,
    next_seq: u64,
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
            latest: BTreeMap::new(),
            next_seq: 1,
            now: 0.0,
            was_open: false,
            confirm_remove: None,
            notice: None,
        }
    }
}

impl KeysPanel {
    /// Whether a request is waiting for the keychain within its timeout.
    pub fn is_waiting(&self) -> bool {
        self.pending.values().any(|request| !request.overdue)
    }

    /// Whether any request is still awaited, including ones past the timeout whose outcome is
    /// unknown (the host keeps polling for their answers, more slowly).
    pub fn is_following(&self) -> bool {
        !self.pending.is_empty()
    }

    /// New accounts with a save in flight or of unknown outcome: they keep a place in the
    /// remembered list until answered.
    fn reserved(&self) -> Vec<Account> {
        let accounts: BTreeSet<&Account> = self.pending.values().filter(|request| request.op == Op::Set).map(|request| &request.account).collect();
        accounts.into_iter().cloned().collect()
    }

    /// Keeps the rows bounded: rows for accounts that are neither remembered nor awaiting an
    /// answer (a failed save, a key the full list couldn't take) are dropped, oldest first, once
    /// there are more than [`KeySettings::MAX_ACCOUNTS`] rows.
    fn prune(&mut self, settings: &KeySettings) {
        let remembered = settings.accounts();
        let busy: BTreeSet<Account> = self.pending.values().map(|request| request.account.clone()).collect();
        let mut extra = self.rows.len().saturating_sub(KeySettings::MAX_ACCOUNTS);
        if extra > 0 {
            self.rows.retain(|(account, _)| {
                if extra == 0 || remembered.contains(account) || busy.contains(account) {
                    true
                } else {
                    extra -= 1;
                    false
                }
            });
        }
        let rows = &self.rows;
        self.latest.retain(|account, _| busy.contains(account) || rows.iter().any(|(a, _)| a == account));
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
        if self.pending.len() >= MAX_PENDING {
            self.notice = Some("Too many keychain requests are still waiting for an answer. Try again once some have finished.".into());
            return None;
        }
        match session.dispatch(id, &params) {
            Ok(answer) => match answer.get("pending").and_then(Value::as_u64) {
                Some(request) => {
                    self.notice = None;
                    self.track(request, account, op);
                    None
                }
                None => {
                    self.notice = None;
                    let seq = self.next();
                    self.latest.insert(account.clone(), seq);
                    self.apply(account, op, seq, Ok(answer))
                }
            },
            Err(e) => {
                self.notice = Some(e.message);
                None
            }
        }
    }

    fn next(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        seq
    }

    /// Follows the keychain request `id` until it is answered; it is now the account's newest.
    fn track(&mut self, id: u64, account: &Account, op: Op) {
        let seq = self.next();
        self.latest.insert(account.clone(), seq);
        self.pending.insert(id, Request { account: account.clone(), op, since: self.now, seq, overdue: false });
        self.set_status(account, if op == Op::Test { KeyStatus::Testing } else { KeyStatus::Waiting });
    }

    /// Applies the keychain's answer to request `seq`. The remembered accounts follow every
    /// answer, in the order the keychain gave them; the row only follows the account's newest
    /// request, so an older answer never overwrites what a newer one is doing or said.
    fn apply(&mut self, account: &Account, op: Op, seq: u64, result: Result<Value, Error>) -> Option<AccountChange> {
        let newest = self.latest.get(account) == Some(&seq);
        let answer = match result {
            Ok(answer) => answer,
            Err(e) => {
                if newest {
                    self.set_status(account, KeyStatus::Failed(e.message));
                }
                return None;
            }
        };
        let masked = || answer.get("masked").and_then(Value::as_str).unwrap_or("…").to_owned();
        match op {
            Op::Set => {
                if newest {
                    self.set_status(account, KeyStatus::Stored(masked()));
                }
                Some(AccountChange::Remember(account.clone()))
            }
            Op::Remove => {
                if newest {
                    self.rows.retain(|(a, _)| a != account);
                }
                Some(AccountChange::Forget(account.clone()))
            }
            Op::Status => {
                let stored = answer.get("stored").and_then(Value::as_bool).unwrap_or(false);
                if newest {
                    self.set_status(account, if stored { KeyStatus::Stored(masked()) } else { KeyStatus::Missing });
                }
                // A key found for an account the list lost (or never got) is listed again.
                stored.then(|| AccountChange::Found(account.clone()))
            }
            Op::Test => {
                if newest {
                    let status = match answer.get("result").and_then(Value::as_str) {
                        Some("verified") => KeyStatus::Verified(masked()),
                        _ => KeyStatus::Unverifiable(masked(), answer.get("reason").and_then(Value::as_str).unwrap_or("no free check").to_owned()),
                    };
                    self.set_status(account, status);
                }
                None
            }
        }
    }

    /// Saves `key` for the account named `name`, unless the remembered list (counting the new
    /// accounts already being saved) has no room for it: then nothing is written and the notice
    /// says why. Replacing a listed account's key is always allowed.
    pub(crate) fn save(&mut self, session: &mut Session, settings: &KeySettings, name: &str, key: String) -> Option<AccountChange> {
        let account = match Account::new(name.trim()) {
            Ok(account) => account,
            Err(e) => {
                self.notice = Some(e.to_string());
                return None;
            }
        };
        if !settings.admits(&account, &self.reserved()) {
            self.notice =
                Some(format!("NodeCraft can keep keys for at most {} accounts. Remove one you no longer use, then save again.", KeySettings::MAX_ACCOUNTS));
            return None;
        }
        self.send(session, Op::Set, &account, json!({"account": account.as_str(), "key": key}))
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
                Some(request) => changes.extend(self.apply(&request.account, request.op, request.seq, outcome.result)),
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
        self.prune(settings);
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
                        KeyStatus::Unknown(what) => (what.clone(), t.text),
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
            if self.account == "runway" {
                ui.hyperlink_to("Powered by Runway", "https://runwayml.com");
            }
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
                        let name = self.account.clone();
                        changes.extend(self.save(session, settings, &name, key));
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

    /// Advances the panel's clock to `now` (egui seconds) and marks requests the keychain hasn't
    /// answered in time as of unknown outcome, so the row can be used again. The host calls it
    /// every frame, open or not.
    pub fn tick(&mut self, now: f64) {
        self.now = now;
        self.expire();
    }

    /// Requests past the timeout stay followed (their answer is still applied), but their row
    /// says the outcome is unknown and what to do.
    fn expire(&mut self) {
        let (now, limit) = (self.now, self.timeout_s);
        let mut late = Vec::new();
        for request in self.pending.values_mut() {
            if !request.overdue && now - request.since > limit {
                request.overdue = true;
                late.push((request.account.clone(), request.op, request.seq));
            }
        }
        for (account, op, seq) in late {
            if self.latest.get(&account) == Some(&seq) {
                self.set_status(&account, KeyStatus::Unknown(unknown_outcome(op).into()));
            }
        }
    }
}

/// What a row says when the keychain hasn't answered `op` in time.
fn unknown_outcome(op: Op) -> &'static str {
    match op {
        Op::Set => "No answer from the keychain yet (is an unlock prompt open?). The key may still be saved; this updates when it answers, or Check later.",
        Op::Remove => {
            "No answer from the keychain yet (is an unlock prompt open?). The key may still be removed; this updates when it answers, or Check later."
        }
        Op::Status | Op::Test => "No answer from the keychain yet (is an unlock prompt open?). Try again.",
    }
}

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
