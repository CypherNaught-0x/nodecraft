//! The quick-add palette: double-click empty canvas to search node kinds and commands.
//! Choosing a kind creates it where the palette opened (`node.create`); choosing a command
//! dispatches it. Entries and filtering are pure so they can be tested without a UI.
use crate::theme::Tokens;
use egui::{Pos2, Vec2};

/// Rows shown at once; typing narrows the list.
pub const MAX_ROWS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A node kind id, created with `node.create`.
    Kind(String),
    /// A command id, dispatched with `{}`.
    Command(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub target: Target,
    pub label: String,
    /// The key that does the same from the canvas, if any.
    pub shortcut: Option<String>,
}

/// Entries matching `query` (case-insensitive, on the label or id), best first: label prefix,
/// then a word in the label starting with it, then anywhere; ties keep `entries` order.
pub fn filter<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let query = query.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize, &Entry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if query.is_empty() {
                return Some((0, index, entry));
            }
            let label = entry.label.to_lowercase();
            let id = match &entry.target {
                Target::Kind(id) => id.to_lowercase(),
                Target::Command(id) => id.to_lowercase(),
            };
            let rank = if label.starts_with(&query) {
                0
            } else if label.split_whitespace().any(|word| word.starts_with(&query)) {
                1
            } else if label.contains(&query) || id.contains(&query) {
                2
            } else {
                return None;
            };
            Some((rank, index, entry))
        })
        .collect();
    ranked.sort_by_key(|(rank, index, _)| (*rank, *index));
    ranked.into_iter().map(|(_, _, entry)| entry).collect()
}

/// An open palette: where it was opened and what is typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// World position new nodes go to.
    pub world: Pos2,
    /// Screen position of the popup's top-left corner.
    pub screen: Pos2,
    pub query: String,
    pub highlighted: usize,
    focus: bool,
}

pub enum Outcome {
    Open,
    Closed,
    Chosen(Target),
}

impl Palette {
    pub fn new(world: Pos2, screen: Pos2) -> Self {
        Self { world, screen, query: String::new(), highlighted: 0, focus: true }
    }

    /// One frame of the popup: ↑/↓ move, ↵ chooses, Escape or a click elsewhere closes.
    pub fn show(&mut self, ctx: &egui::Context, entries: &[Entry], t: &Tokens) -> Outcome {
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if escape {
            return Outcome::Closed;
        }
        let matches = filter(entries, &self.query);
        let shown = matches.len().min(MAX_ROWS);
        if down {
            self.highlighted = (self.highlighted + 1).min(shown.saturating_sub(1));
        }
        if up {
            self.highlighted = self.highlighted.saturating_sub(1);
        }
        self.highlighted = self.highlighted.min(shown.saturating_sub(1));
        if enter && let Some(entry) = matches.get(self.highlighted) {
            return Outcome::Chosen(entry.target.clone());
        }
        // The double-click that opened the palette must not count as a click elsewhere.
        let first_frame = self.focus;
        let mut chosen = None;
        let area = egui::Area::new(egui::Id::new("quick_add_palette")).order(egui::Order::Foreground).fixed_pos(self.screen).show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.panel_fill)
                .stroke(egui::Stroke::new(1.0, t.panel_stroke))
                .corner_radius(t.panel_radius)
                .inner_margin(t.control_gap * 2.0)
                .show(ui, |ui| {
                    ui.set_width(t.card_width);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .id_salt("query")
                            .hint_text("Add a node or run a command…")
                            .desired_width(f32::INFINITY)
                            .char_limit(128),
                    );
                    if self.focus {
                        edit.request_focus();
                        self.focus = false;
                    }
                    if edit.changed() {
                        self.highlighted = 0;
                    }
                    if matches.is_empty() {
                        ui.label(egui::RichText::new("Nothing matches").color(t.text_muted));
                    }
                    for (row, entry) in matches.iter().take(MAX_ROWS).enumerate() {
                        let text = match &entry.shortcut {
                            Some(key) => format!("{}    {key}", entry.label),
                            None => entry.label.clone(),
                        };
                        let response = ui.add(egui::Button::selectable(row == self.highlighted, text).min_size(Vec2::new(ui.available_width(), 0.0)));
                        if response.hovered() {
                            self.highlighted = row;
                        }
                        if response.clicked() {
                            chosen = Some(entry.target.clone());
                        }
                    }
                });
        });
        if let Some(target) = chosen {
            return Outcome::Chosen(target);
        }
        if !first_frame && area.response.clicked_elsewhere() {
            return Outcome::Closed;
        }
        Outcome::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(id: &str, label: &str, key: Option<&str>) -> Entry {
        Entry { target: Target::Kind(id.into()), label: label.into(), shortcut: key.map(str::to_owned) }
    }

    #[test]
    fn filtering_ranks_prefixes_then_words_then_anywhere() {
        let entries = vec![
            kind("source.text", "Text", Some("T")),
            kind("source.image", "Image", Some("I")),
            kind("test.context", "Image context", None),
            Entry { target: Target::Command("edit.undo"), label: "Undo".into(), shortcut: Some("Command+Z".into()) },
        ];
        let labels = |q: &str| filter(&entries, q).into_iter().map(|e| e.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(""), vec!["Text", "Image", "Image context", "Undo"]);
        assert_eq!(labels("ima"), vec!["Image", "Image context"]);
        assert_eq!(labels("CONTEXT"), vec!["Image context"]);
        assert_eq!(labels("ext"), vec!["Text", "Image context"]);
        assert_eq!(labels("edit.u"), vec!["Undo"]);
        assert!(labels("zzz").is_empty());
        // Unicode and odd input never panic.
        assert!(labels("ß̶\u{0}").is_empty());
    }
}
