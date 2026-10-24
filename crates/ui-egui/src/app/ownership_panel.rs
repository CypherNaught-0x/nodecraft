//! Reviewing quarantined charges (#362, #384): charges recorded before NodeCraft kept which
//! session's total held each one. The panel lists the groups `usage.ownership` reports; a group
//! shows what was known at import (its evidence), the totals it may move between, and its charges
//! a page at a time. The person picks the total that holds them (marking any that belong to the
//! other), previews the outcome with a dry run, and only then can confirm it. Confirming
//! dispatches `usage.reconcile_ownership`, which only a person may run; it settles nothing.
//!
//! A reconciled charge no run is left to settle is listed below the groups (#332, from
//! `usage.unsettled`). The person states what their provider's records show, the billed amount
//! or that it was never billed, previews the totals it leaves, and confirms
//! `usage.resolve_charge`, which is also a person's alone.
use super::*;
use serde_json::Value;
use std::collections::BTreeSet;

/// Charges one page of the panel shows.
const PAGE: usize = 50;

/// Which total holds a group's charges, apart from those marked as the other's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holder {
    Named,
    Earlier,
}

impl Holder {
    fn param(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Earlier => "earlier",
        }
    }
}

/// One group under review.
struct Group {
    session: String,
    currency: String,
    /// The `usage.ownership` page: the group's evidence, totals and token, and its charges.
    detail: Value,
    offset: usize,
    holder: Holder,
    /// Charges (provider, key) held by the other total.
    except: BTreeSet<(String, String)>,
    /// The dry run of the decision as it stands; any change clears it.
    preview: Option<Value>,
    /// Why the last preview or confirmation was refused.
    refused: Option<String>,
}

/// One reconciled open charge being resolved.
struct Resolving {
    /// Its `usage.unsettled` row, with the token the decision carries.
    charge: Value,
    /// Billed (at `amount`), or never billed.
    billed: bool,
    /// The billed amount as typed, in the charge's currency.
    amount: String,
    /// The dry run of the decision as it stands; any change clears it.
    preview: Option<Value>,
    refused: Option<String>,
}

/// The open panel.
#[derive(Default)]
pub(super) struct OwnershipReview {
    /// The `usage.ownership` listing of groups.
    groups: Value,
    /// The first `usage.unsettled` page.
    unsettled: Value,
    group: Option<Group>,
    resolving: Option<Resolving>,
}

/// What a click asked for, applied after the panel is drawn.
enum Action {
    Review(String, String),
    Page(usize),
    Back,
    Holder(Holder),
    Toggle(String, String),
    Preview,
    Confirm,
    Resolve(usize),
    Billed(bool),
    Amount(String),
    ResolvePreview,
    ResolveConfirm,
}

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn count(value: &Value, pointer: &str) -> u64 {
    value.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

/// "1 charge", "2 charges".
fn charges(n: u64) -> String {
    if n == 1 { "1 charge".into() } else { format!("{n} charges") }
}

/// Micros as a decimal amount: 7 -> "0.000007".
fn decimal(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// A typed decimal amount as micros, at most six decimals; `None` for anything else.
fn micros(text: &str) -> Option<u64> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if (whole.is_empty() && fraction.is_empty()) || fraction.len() > 6 || !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let fraction: u64 = if fraction.is_empty() { 0 } else { format!("{fraction:0<6}").parse().ok()? };
    whole.checked_mul(1_000_000)?.checked_add(fraction)
}

/// A session id short enough for a heading.
fn short(session: &str) -> String {
    let head: String = session.chars().take(8).collect();
    if head.len() < session.len() { format!("{head}…") } else { head }
}

impl Group {
    fn decision(&self, dry_run: bool) -> Value {
        let except: Vec<Value> = self.except.iter().map(|(provider, key)| json!({"provider": provider, "key": key})).collect();
        json!({"session": self.session, "currency": self.currency, "token": self.detail.pointer("/group/token"),
            "holder": self.holder.param(), "except": except, "dry_run": dry_run})
    }
}

impl Resolving {
    fn decision(&self, dry_run: bool) -> Result<Value, String> {
        let c = &self.charge;
        let mut decision = json!({"provider": text(c, "/provider"), "key": text(c, "/key"), "token": text(c, "/token"), "dry_run": dry_run});
        if self.billed {
            let micros = micros(&self.amount).ok_or("Enter the billed amount as a number, with at most six decimals")?;
            decision["outcome"] = json!("settled");
            decision["currency"] = json!(text(c, "/amount/currency"));
            decision["micros"] = json!(micros);
        } else {
            decision["outcome"] = json!("released");
        }
        Ok(decision)
    }
}

impl NodeCraftApp {
    /// Opens the review on the list of groups and the reconciled charges still open.
    pub(super) fn open_ownership_review(&mut self) {
        match self.ownership_listings() {
            Ok((groups, unsettled)) => {
                self.notice = None;
                self.ownership = Some(OwnershipReview { groups, unsettled, group: None, resolving: None });
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    fn ownership_listings(&mut self) -> Result<(Value, Value), Error> {
        let groups = self.session.dispatch("usage.ownership", &json!({}))?;
        let unsettled = self.session.dispatch("usage.unsettled", &json!({"limit": PAGE}))?;
        Ok((groups, unsettled))
    }

    fn load_group(&mut self, session: String, currency: String, offset: usize) -> Result<Group, Error> {
        let detail = self.session.dispatch("usage.ownership", &json!({"session": session, "currency": currency, "offset": offset, "limit": PAGE}))?;
        Ok(Group { session, currency, detail, offset, holder: Holder::Named, except: BTreeSet::new(), preview: None, refused: None })
    }

    pub(super) fn ownership_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(review) = &self.ownership else { return };
        let mut open = true;
        let mut action = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Quarantined charges")
            .id(egui::Id::new("nodecraft.ownership"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::CENTER_TOP, [0.0, t.navigation_margin + t.navigation_toolbar_height + t.navigation_gap])
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_max_width((3.0 * t.card_width).min(self.viewport.width().max(0.0)));
                match (&review.group, &review.resolving) {
                    (Some(group), _) => group_view(ui, group, t, &mut action),
                    (None, Some(resolving)) => resolve_view(ui, resolving, t, &mut action),
                    (None, None) => {
                        groups(ui, &review.groups, t, &mut action);
                        unsettled(ui, &review.unsettled, t, &mut action);
                    }
                }
            });
        if !open {
            self.ownership = None;
            return;
        }
        if let Some(action) = action {
            self.apply_ownership(action);
        }
    }

    fn apply_ownership(&mut self, action: Action) {
        let Some(mut review) = self.ownership.take() else { return };
        match action {
            Action::Review(session, currency) => match self.load_group(session, currency, 0) {
                Ok(group) => review.group = Some(group),
                Err(error) => self.notice = Some(error.message),
            },
            Action::Back => {
                review.group = None;
                review.resolving = None;
            }
            Action::Page(offset) => {
                if let Some(group) = review.group.take() {
                    match self.load_group(group.session.clone(), group.currency.clone(), offset) {
                        // The decision carries over: the token is the group's, not the page's. If the
                        // group changed meanwhile, what was previewed no longer holds.
                        Ok(page) => {
                            let same = page.detail.pointer("/group/token") == group.detail.pointer("/group/token");
                            let preview = if same { group.preview } else { None };
                            review.group = Some(Group { detail: page.detail, offset, preview, ..group });
                        }
                        Err(error) => review.group = Some(Group { refused: Some(error.message), ..group }),
                    }
                }
            }
            Action::Holder(holder) => {
                if let Some(group) = &mut review.group {
                    group.holder = holder;
                    group.preview = None;
                }
            }
            Action::Toggle(provider, key) => {
                if let Some(group) = &mut review.group {
                    let charge = (provider, key);
                    if !group.except.remove(&charge) {
                        group.except.insert(charge);
                    }
                    group.preview = None;
                }
            }
            Action::Preview => {
                if let Some(group) = &mut review.group {
                    match self.session.dispatch("usage.reconcile_ownership", &group.decision(true)) {
                        Ok(preview) => {
                            group.preview = Some(preview);
                            group.refused = None;
                        }
                        Err(error) => {
                            group.preview = None;
                            group.refused = Some(error.message);
                        }
                    }
                }
            }
            Action::Confirm => {
                if let Some(mut group) = review.group.take() {
                    let decision = group.decision(false);
                    match self.session.dispatch("usage.reconcile_ownership", &decision) {
                        Ok(record) => {
                            if let Some(runs) = &self.runs {
                                runs.sync_usage(&mut self.session);
                            }
                            self.notice = Some(format!(
                                "Recorded which total holds {} of session {}. Nothing was settled or refunded.",
                                charges(count(&record, "/charges")),
                                short(&group.session)
                            ));
                            match self.ownership_listings() {
                                Ok((groups, unsettled)) => (review.groups, review.unsettled) = (groups, unsettled),
                                Err(error) => self.notice = Some(error.message),
                            }
                        }
                        Err(error) => {
                            group.preview = None;
                            group.refused = Some(error.message);
                            review.group = Some(group);
                        }
                    }
                }
            }
            Action::Resolve(index) => {
                if let Some(charge) = review.unsettled.get("charges").and_then(|rows| rows.get(index)) {
                    let amount = decimal(count(charge, "/amount/micros"));
                    review.resolving = Some(Resolving { charge: charge.clone(), billed: true, amount, preview: None, refused: None });
                }
            }
            Action::Billed(billed) => {
                if let Some(resolving) = &mut review.resolving {
                    resolving.billed = billed;
                    resolving.preview = None;
                }
            }
            Action::Amount(amount) => {
                if let Some(resolving) = &mut review.resolving {
                    resolving.amount = amount;
                    resolving.preview = None;
                }
            }
            Action::ResolvePreview => {
                if let Some(resolving) = &mut review.resolving {
                    let preview = resolving.decision(true).and_then(|d| self.session.dispatch("usage.resolve_charge", &d).map_err(|e| e.message));
                    (resolving.preview, resolving.refused) = match preview {
                        Ok(preview) => (Some(preview), None),
                        Err(refused) => (None, Some(refused)),
                    };
                }
            }
            Action::ResolveConfirm => {
                if let Some(mut resolving) = review.resolving.take() {
                    let confirmed = resolving.decision(false).and_then(|d| self.session.dispatch("usage.resolve_charge", &d).map_err(|e| e.message));
                    match confirmed {
                        Ok(record) => {
                            if let Some(runs) = &self.runs {
                                runs.sync_usage(&mut self.session);
                            }
                            self.notice = Some(match text(&record, "/outcome") {
                                "settled" => format!("Settled the charge {} at {}.", text(&record, "/key"), text(&record, "/cost/text")),
                                _ => format!("Released the charge {}; it no longer counts toward any total.", text(&record, "/key")),
                            });
                            match self.ownership_listings() {
                                Ok((groups, unsettled)) => (review.groups, review.unsettled) = (groups, unsettled),
                                Err(error) => self.notice = Some(error.message),
                            }
                        }
                        Err(refused) => {
                            resolving.preview = None;
                            resolving.refused = Some(refused);
                            review.resolving = Some(resolving);
                        }
                    }
                }
            }
        }
        self.ownership = Some(review);
    }
}

fn groups(ui: &mut egui::Ui, listing: &Value, t: &Tokens, action: &mut Option<Action>) {
    let rows = listing.get("groups").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        let reconciled = count(listing, "/reconciled");
        let done = match reconciled {
            0 => String::new(),
            1 => " (1 decision recorded)".into(),
            n => format!(" ({n} decisions recorded)"),
        };
        ui.label(egui::RichText::new(format!("No quarantined charges are waiting for review{done}.")).color(t.text_muted));
        return;
    }
    ui.label(
        "These charges were recorded before NodeCraft kept which session's total held each one, so they are set \
         apart until you decide. Compare them with your provider's billing records, then say which total they belong \
         to. Nothing is settled, released or refunded.",
    );
    ui.add_space(t.control_gap);
    egui::Grid::new("ownership_groups").num_columns(4).spacing([t.control_gap * 2.0, t.control_gap]).striped(true).show(ui, |ui| {
        for row in rows {
            let session = text(row, "/session");
            ui.label(format!("Session {}", short(session)));
            ui.label(charges(count(row, "/charges_total")));
            ui.label(format!("{} open", text(row, "/original_open/text")));
            if ui.button("Review").clicked() {
                *action = Some(Action::Review(session.to_owned(), text(row, "/currency").to_owned()));
            }
            ui.end_row();
        }
    });
    if listing.get("next_offset").is_some_and(|next| !next.is_null()) {
        ui.label(egui::RichText::new(format!("{} groups in all; review these first.", count(listing, "/total"))).color(t.text_muted));
    }
}

fn unsettled(ui: &mut egui::Ui, listing: &Value, t: &Tokens, action: &mut Option<Action>) {
    let rows = listing.get("charges").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        return;
    }
    ui.add_space(t.control_gap * 2.0);
    ui.heading("Reconciled charges still open");
    ui.label(
        "No run is left to settle these. Check each one against your provider's billing records, then record the \
         amount it billed, or that it never billed it.",
    );
    egui::ScrollArea::vertical().id_salt("unsettled_scroll").max_height(t.minimap_height * 2.0).show(ui, |ui| {
        egui::Grid::new("unsettled_charges").num_columns(4).spacing([t.control_gap * 2.0, t.control_gap]).striped(true).show(ui, |ui| {
            for (index, charge) in rows.iter().enumerate() {
                ui.label(format!("{} · {}", text(charge, "/provider"), text(charge, "/model"))).on_hover_text(format!(
                    "Session {}, node {}, recorded at {} ms since 1970",
                    short(text(charge, "/session")),
                    text(charge, "/node"),
                    count(charge, "/at_ms")
                ));
                ui.label(format!("{} {}", text(charge, "/amount/text"), text(charge, "/status")));
                ui.label(egui::RichText::new(text(charge, "/key")).color(t.text_muted))
                    .on_hover_text("The charge's idempotency key, as your provider's records show it");
                if ui.button("Resolve").clicked() {
                    *action = Some(Action::Resolve(index));
                }
                ui.end_row();
            }
        });
    });
    if listing.get("next_offset").is_some_and(|next| !next.is_null()) {
        ui.label(egui::RichText::new(format!("{} charges in all; resolve these first.", count(listing, "/total"))).color(t.text_muted));
    }
}

fn resolve_view(ui: &mut egui::Ui, resolving: &Resolving, t: &Tokens, action: &mut Option<Action>) {
    let c = &resolving.charge;
    if ui.button("All charges").clicked() {
        *action = Some(Action::Back);
    }
    ui.heading(format!("{} · {}", text(c, "/provider"), text(c, "/model")));
    egui::Grid::new("resolve_charge").num_columns(2).spacing([t.control_gap * 2.0, t.control_gap]).show(ui, |ui| {
        for (label, value) in [
            ("Idempotency key", text(c, "/key").to_owned()),
            ("Session", short(text(c, "/session"))),
            ("Open amount", format!("{} ({})", text(c, "/amount/text"), text(c, "/status"))),
        ] {
            ui.label(label);
            ui.label(value);
            ui.end_row();
        }
    });
    ui.add_space(t.control_gap);
    ui.label("Your provider's records show it:");
    ui.horizontal(|ui| {
        for (billed, label) in [(true, "Billed"), (false, "Never billed")] {
            if ui.radio(resolving.billed == billed, label).clicked() && resolving.billed != billed {
                *action = Some(Action::Billed(billed));
            }
        }
    });
    if resolving.billed {
        ui.horizontal(|ui| {
            ui.label(format!("Billed amount ({})", text(c, "/amount/currency")));
            let mut amount = resolving.amount.clone();
            if ui.add(egui::TextEdit::singleline(&mut amount).desired_width(t.card_width * 0.5)).changed() {
                *action = Some(Action::Amount(amount));
            }
        });
    }
    if let Some(preview) = &resolving.preview {
        let outcome = match text(preview, "/outcome") {
            "settled" => format!("Settles at {}", text(preview, "/cost/text")),
            _ => "Leaves the totals".to_owned(),
        };
        ui.label(format!(
            "{outcome}. Afterwards this session's total is {} and the project's {}.",
            text(preview, "/session_total/text"),
            text(preview, "/project_total/text")
        ));
    }
    if let Some(refused) = &resolving.refused {
        ui.label(egui::RichText::new(refused).color(t.danger));
    }
    ui.horizontal(|ui| {
        if ui.button("Preview").on_hover_text("Check the decision and show the totals it leaves, changing nothing").clicked() {
            *action = Some(Action::ResolvePreview);
        }
        let confirm = ui
            .add_enabled(resolving.preview.is_some(), egui::Button::new("Confirm"))
            .on_hover_text("Record this in the usage log")
            .on_disabled_hover_text("Preview the decision first");
        if confirm.clicked() {
            *action = Some(Action::ResolveConfirm);
        }
    });
}

fn group_view(ui: &mut egui::Ui, group: &Group, t: &Tokens, action: &mut Option<Action>) {
    let d = &group.detail;
    if ui.button("All groups").clicked() {
        *action = Some(Action::Back);
    }
    ui.heading(format!("Session {} · {}", short(&group.session), group.currency));
    ui.label(egui::RichText::new("When these charges were imported").color(t.text_muted));
    egui::Grid::new("ownership_evidence").num_columns(2).spacing([t.control_gap * 2.0, t.control_gap]).show(ui, |ui| {
        for (label, pointer) in [
            ("This session's total", "/group/evidence/original_named/text"),
            ("The earlier total", "/group/evidence/original_earlier/text"),
            ("Open charges", "/group/evidence/original_open/text"),
            ("Project total", "/group/evidence/original_project/text"),
            ("This session's total could hold up to", "/group/max_named/text"),
            ("The earlier total could hold up to", "/group/max_earlier/text"),
        ] {
            ui.label(label);
            ui.label(text(d, pointer));
            ui.end_row();
        }
    });
    ui.add_space(t.control_gap);
    let total = count(d, "/total");
    ui.label(format!("{}. Tick the ones that belong to the other total.", charges(total)));
    let rows = d.get("charges").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    egui::ScrollArea::vertical().max_height(t.minimap_height * 2.0).show(ui, |ui| {
        egui::Grid::new("ownership_charges").num_columns(5).spacing([t.control_gap * 2.0, t.control_gap]).striped(true).show(ui, |ui| {
            for charge in rows {
                let (provider, key) = (text(charge, "/provider").to_owned(), text(charge, "/key").to_owned());
                let mut other = group.except.contains(&(provider.clone(), key.clone()));
                if ui.checkbox(&mut other, "Other total").changed() {
                    *action = Some(Action::Toggle(provider.clone(), key.clone()));
                }
                ui.label(format!("{provider} · {}", text(charge, "/model"))).on_hover_text(format!(
                    "Node {}, recorded at {} ms since 1970",
                    text(charge, "/node"),
                    count(charge, "/at_ms")
                ));
                ui.label(text(charge, "/amount/text"));
                let kept = match text(charge, "/location") {
                    "pending" => "waiting to settle",
                    "evicted" => "only in the totals",
                    _ => "in the usage list",
                };
                ui.label(format!("{}, {kept}", text(charge, "/status")));
                ui.label(egui::RichText::new(&key).color(t.text_muted)).on_hover_text("The charge's idempotency key, as your provider's records show it");
                ui.end_row();
            }
        });
    });
    let next = d.get("next_offset").and_then(Value::as_u64).and_then(|next| usize::try_from(next).ok());
    if group.offset > 0 || next.is_some() {
        ui.horizontal(|ui| {
            if ui.add_enabled(group.offset > 0, egui::Button::new("Previous")).clicked() {
                *action = Some(Action::Page(group.offset.saturating_sub(PAGE)));
            }
            if let Some(next) = next
                && ui.button("Next").clicked()
            {
                *action = Some(Action::Page(next));
            }
        });
    }
    ui.add_space(t.control_gap);
    ui.label("The rest are held by:");
    ui.horizontal(|ui| {
        for (holder, label) in [(Holder::Named, "This session's total"), (Holder::Earlier, "The earlier total")] {
            if ui.radio(group.holder == holder, label).clicked() && group.holder != holder {
                *action = Some(Action::Holder(holder));
            }
        }
    });
    if let Some(preview) = &group.preview {
        ui.label(format!(
            "{}: {} held by this session's total, {} by the earlier one; {} moves from the earlier total. Afterwards this session's total is {} and the earlier one {}.",
            charges(count(preview, "/charges")),
            count(preview, "/named"),
            count(preview, "/earlier"),
            text(preview, "/moved_from_earlier/text"),
            text(preview, "/named_total/text"),
            text(preview, "/earlier_total/text"),
        ));
    }
    if let Some(refused) = &group.refused {
        ui.label(egui::RichText::new(refused).color(t.danger));
    }
    ui.horizontal(|ui| {
        if ui.button("Preview").on_hover_text("Check the decision and show what it changes, changing nothing").clicked() {
            *action = Some(Action::Preview);
        }
        let confirm = ui
            .add_enabled(group.preview.is_some(), egui::Button::new("Confirm"))
            .on_hover_text("Record this decision in the usage log")
            .on_disabled_hover_text("Preview the decision first");
        if confirm.clicked() {
            *action = Some(Action::Confirm);
        }
    });
}

#[cfg(test)]
mod tests;
