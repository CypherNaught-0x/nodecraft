//! The Budgets panel (#201): the most runs may spend per call, per session and per project, in
//! US dollars. An empty field means no cap. The caps are the person's settings (`[budgets]`), not
//! a command: no agent or script can raise them. A run a cap blocks says which one, and its
//! confirmation opens this panel.

use nodecraft_engine::settings::BudgetSettings;
use nodecraft_exec::cost::{Spent, format_money};

use crate::theme::Tokens;

/// Longest amount read from a field.
const MAX_FIELD_CHARS: usize = 16;

/// Each cap's field: its label, and what it limits.
const CAPS: [(&str, &str); 3] = [
    ("Per call", "The most one provider job may cost."),
    ("This session", "The most runs may spend until NodeCraft closes."),
    ("This project", "The most runs in this project may spend."),
];

#[derive(Default)]
pub struct BudgetsPanel {
    pub open: bool,
    /// What the fields show, filled from the settings each time the panel opens.
    fields: Option<[String; 3]>,
    /// Why the typed amounts weren't saved.
    pub notice: Option<String>,
    /// Said after a save, until the next edit.
    pub status: Option<String>,
}

impl BudgetsPanel {
    /// Opens the panel on the caps `settings` hold now.
    pub fn open(&mut self) {
        self.open = true;
        self.fields = None;
    }

    /// Draws the panel when open; returns the caps to save when the person saves them.
    pub fn show(&mut self, ctx: &egui::Context, settings: &BudgetSettings, spent: Option<&Spent>, t: &Tokens) -> Option<BudgetSettings> {
        if !self.open {
            self.fields = None;
            return None;
        }
        let fields = self.fields.get_or_insert_with(|| {
            [settings.per_call_usd_cents, settings.session_usd_cents, settings.project_usd_cents].map(|cents| cents.map(dollars).unwrap_or_default())
        });
        let mut save = None;
        let mut open = self.open;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Budgets").id(egui::Id::new("nodecraft.budgets")).open(&mut open).collapsible(false).resizable(false).frame(frame).show(ctx, |ui| {
            ui.label(
                egui::RichText::new("Runs that would go past a cap don't start. Leave a field empty for no cap. Amounts are in US dollars.")
                    .color(t.text_muted),
            );
            ui.add_space(t.control_gap);
            let mut edited = false;
            egui::Grid::new("nodecraft.budgets.caps").num_columns(2).spacing([t.control_gap * 2.0, t.control_gap]).show(ui, |ui| {
                for ((label, hint), field) in CAPS.iter().zip(fields.iter_mut()) {
                    let name = ui.label(egui::RichText::new(*label).color(t.text)).on_hover_text(*hint);
                    edited |= ui
                        .add(egui::TextEdit::singleline(field).char_limit(MAX_FIELD_CHARS).hint_text("no cap").desired_width(t.card_width * 0.6))
                        .labelled_by(name.id)
                        .on_hover_text(*hint)
                        .changed();
                    ui.end_row();
                }
            });
            if edited {
                self.status = None;
            }
            if let Some(spent) = spent {
                let text = |amounts: &std::collections::BTreeMap<_, _>| {
                    let listed: Vec<String> = amounts.values().copied().map(format_money).collect();
                    if listed.is_empty() { "nothing".to_owned() } else { listed.join(", ") }
                };
                ui.label(
                    egui::RichText::new(format!("Spent this session: {}. In this project: {}.", text(&spent.session), text(&spent.project)))
                        .color(t.text_muted),
                );
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match parse(fields) {
                        Ok(caps) => {
                            self.notice = None;
                            self.status = Some("Saved. Runs are checked against these caps from now on.".into());
                            save = Some(caps);
                        }
                        Err(why) => self.notice = Some(why),
                    }
                }
            });
            if let Some(notice) = &self.notice {
                ui.label(egui::RichText::new(notice).color(t.danger));
            }
            if let Some(status) = &self.status {
                ui.label(egui::RichText::new(status).color(t.text_muted));
            }
        });
        self.open = open;
        save
    }
}

/// The three fields as caps, or what is wrong with one of them.
fn parse(fields: &[String; 3]) -> Result<BudgetSettings, String> {
    let mut cents = [None; 3];
    for ((slot, field), (label, _)) in cents.iter_mut().zip(fields).zip(CAPS) {
        *slot = cents_of(field).map_err(|why| format!("{label}: {why}"))?;
    }
    let [per_call_usd_cents, session_usd_cents, project_usd_cents] = cents;
    Ok(BudgetSettings { per_call_usd_cents, session_usd_cents, project_usd_cents })
}

/// An amount in dollars, such as `5`, `0.25` or `$12.50`, as cents; empty is no cap.
pub fn cents_of(text: &str) -> Result<Option<u64>, String> {
    let text = text.trim();
    let text = text.strip_prefix('$').unwrap_or(text).trim();
    if text.is_empty() {
        return Ok(None);
    }
    let bad = || "type an amount in dollars, such as 5 or 0.25, or leave it empty".to_owned();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) || fraction.len() > 2 || whole.len() > 9 {
        return Err(bad());
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| bad())? };
    let fraction: u64 = if fraction.is_empty() { 0 } else { format!("{fraction:0<2}").parse().map_err(|_| bad())? };
    let cents = whole.checked_mul(100).and_then(|c| c.checked_add(fraction)).ok_or_else(bad)?;
    if cents > BudgetSettings::MAX_USD_CENTS {
        return Err(format!("the most a cap can be is {}", dollars(BudgetSettings::MAX_USD_CENTS)));
    }
    Ok(Some(cents))
}

/// Cents as a dollar amount for a field: `5`, `0.25`, `12.5`.
pub fn dollars(cents: u64) -> String {
    let (whole, fraction) = (cents / 100, cents % 100);
    if fraction == 0 { whole.to_string() } else { format!("{whole}.{}", format!("{fraction:02}").trim_end_matches('0')) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_read_in_dollars_and_hostile_ones_refused() {
        assert_eq!(cents_of(""), Ok(None));
        assert_eq!(cents_of("  "), Ok(None));
        assert_eq!(cents_of("5"), Ok(Some(500)));
        assert_eq!(cents_of("$0.25"), Ok(Some(25)));
        assert_eq!(cents_of(".5"), Ok(Some(50)));
        assert_eq!(cents_of("12.50"), Ok(Some(1250)));
        assert_eq!(cents_of("100000"), Ok(Some(10_000_000)));
        for bad in ["-1", "1.234", "1e3", "abc", ".", "1.2.3", "99999999999999", "100000.01", "５", "NaN"] {
            assert!(cents_of(bad).is_err(), "{bad}");
        }
        for cents in [0, 5, 25, 50, 1250, 10_000_000] {
            assert_eq!(cents_of(&dollars(cents)), Ok(Some(cents)));
        }
        let fields = ["0.5".to_owned(), String::new(), "x".to_owned()];
        assert!(parse(&fields).is_err_and(|why| why.starts_with("This project:")));
    }
}
