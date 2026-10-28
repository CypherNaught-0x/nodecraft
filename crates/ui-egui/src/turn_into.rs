//! "Turn into" (#60): dropping a new noodle on empty canvas opens a menu of the node kinds that
//! can take it. A kind's quick-add key, or ↑/↓ and ↵, or a click picks one; the app then runs
//! `node.create_connected`, so the node and its connection are one undo step.
use crate::noodle::Side;
use crate::theme::Tokens;
use egui::{Pos2, Vec2};
use nodecraft_graph::Endpoint;
use nodecraft_nodes::Registry;
use nodecraft_types::PortType;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: String,
    pub label: String,
    /// The kind's single-key quick-add shortcut, if it has one.
    pub key: Option<char>,
}

/// What a noodle carrying `output` can turn into, with each kind's single-key shortcut.
pub fn entries(kinds: &Registry, output: PortType) -> Vec<Entry> {
    nodecraft_engine::turn_into_cmds::kinds_accepting(kinds, output)
        .into_iter()
        .map(|d| Entry { kind: d.id.clone(), label: d.label.clone(), key: single_key(d.quick_add.as_deref()) })
        .collect()
}

/// Kinds that can provide a compatible output for an existing input.
pub fn feeding_entries(kinds: &Registry, input: PortType) -> Vec<Entry> {
    nodecraft_engine::turn_into_cmds::kinds_feeding(kinds, input)
        .into_iter()
        .map(|d| Entry { kind: d.id.clone(), label: d.label.clone(), key: single_key(d.quick_add.as_deref()) })
        .collect()
}

/// A shortcut that is one letter or digit with no modifiers.
fn single_key(shortcut: Option<&str>) -> Option<char> {
    let mut chars = shortcut?.chars();
    let key = chars.next()?;
    (chars.next().is_none() && key.is_ascii_alphanumeric()).then(|| key.to_ascii_lowercase())
}

/// The entry a typed character picks, if any.
pub fn by_key(entries: &[Entry], typed: char) -> Option<&Entry> {
    let typed = typed.to_ascii_lowercase();
    entries.iter().find(|entry| entry.key == Some(typed))
}

pub enum Outcome {
    Open,
    Closed,
    Chosen(String),
}

/// An open menu: the noodle it continues and where the new node goes.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnInto {
    pub anchor: Endpoint,
    pub side: Side,
    pub port_type: PortType,
    /// World position the new node goes to.
    pub world: Pos2,
    /// Screen position of the menu's top-left corner.
    pub screen: Pos2,
    pub highlighted: usize,
    /// What it offers, computed once when it opens (the registry can't change meanwhile).
    pub entries: Vec<Entry>,
    /// The release that opened the menu must not count as a click elsewhere.
    opened: bool,
}

impl TurnInto {
    pub fn new(kinds: &Registry, from: Endpoint, output: PortType, world: Pos2, screen: Pos2) -> Self {
        Self::for_port(kinds, from, Side::Output, output, world, screen)
    }

    pub fn feeding(kinds: &Registry, to: Endpoint, input: PortType, world: Pos2, screen: Pos2) -> Self {
        Self::for_port(kinds, to, Side::Input, input, world, screen)
    }

    fn for_port(kinds: &Registry, anchor: Endpoint, side: Side, port_type: PortType, world: Pos2, screen: Pos2) -> Self {
        let entries = match side {
            Side::Output => entries(kinds, port_type),
            Side::Input => feeding_entries(kinds, port_type),
        };
        Self { anchor, side, port_type, world, screen, highlighted: 0, entries, opened: true }
    }

    /// One frame: a kind's key, or ↑/↓ and ↵, or a click picks; Escape or a click elsewhere closes.
    pub fn show(&mut self, ctx: &egui::Context, t: &Tokens) -> Outcome {
        let entries = std::mem::take(&mut self.entries);
        let outcome = self.frame(ctx, &entries, t);
        self.entries = entries;
        outcome
    }

    fn frame(&mut self, ctx: &egui::Context, entries: &[Entry], t: &Tokens) -> Outcome {
        let title = match self.side {
            Side::Output => "Turn into",
            Side::Input => "Connect from",
        };
        // Letters typed into a focused text field (search, a card) are that field's, not picks.
        let field_focused = crate::canvas::text_input_focused(ctx);
        let (up, down, enter, escape, typed) = ctx.input_mut(|i| {
            let typed = i.events.iter().find_map(|event| match event {
                egui::Event::Text(text) => text.chars().next(),
                _ => None,
            });
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                typed,
            )
        });
        if escape {
            return Outcome::Closed;
        }
        if let Some(entry) = typed.filter(|_| !field_focused).and_then(|typed| by_key(entries, typed)) {
            return Outcome::Chosen(entry.kind.clone());
        }
        let last = entries.len().saturating_sub(1);
        if down {
            self.highlighted = self.highlighted.saturating_add(1).min(last);
        }
        if up {
            self.highlighted = self.highlighted.saturating_sub(1);
        }
        self.highlighted = self.highlighted.min(last);
        if enter && let Some(entry) = entries.get(self.highlighted) {
            return Outcome::Chosen(entry.kind.clone());
        }
        let first_frame = std::mem::replace(&mut self.opened, false);
        let mut chosen = None;
        let area = egui::Area::new(egui::Id::new("turn_into_menu")).order(egui::Order::Foreground).fixed_pos(self.screen).show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.panel_fill)
                .stroke(egui::Stroke::new(1.0, t.panel_stroke))
                .corner_radius(t.panel_radius)
                .inner_margin(t.control_gap * 2.0)
                .show(ui, |ui| {
                    ui.set_width(t.card_width * 0.75);
                    ui.label(egui::RichText::new(title).color(t.text_muted));
                    if entries.is_empty() {
                        let message = match self.side {
                            Side::Output => "No node takes this connection yet",
                            Side::Input => "No node has a compatible output",
                        };
                        ui.label(egui::RichText::new(message).color(t.text_muted));
                    }
                    let row_height = ui.spacing().interact_size.y;
                    let stride = row_height + ui.spacing().item_spacing.y;
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt("entries")
                        .max_height((stride * crate::palette::MAX_ROWS as f32 - ui.spacing().item_spacing.y).min(ui.ctx().content_rect().height() * 0.5));
                    if first_frame || up || down {
                        scroll = scroll.vertical_scroll_offset(self.highlighted as f32 * stride);
                    }
                    scroll.show_rows(ui, row_height, entries.len(), |ui, range| {
                        for row in range {
                            let Some(entry) = entries.get(row) else { continue };
                            let text = match entry.key {
                                Some(key) => format!("{}    {}", entry.label, key.to_ascii_uppercase()),
                                None => entry.label.clone(),
                            };
                            let response = ui
                                .add(egui::Button::selectable(row == self.highlighted, text).truncate().min_size(Vec2::new(ui.available_width(), row_height)));
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(egui::WidgetType::Button, true, row == self.highlighted, format!("{title} {}", entry.label))
                            });
                            if response.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                                self.highlighted = row;
                            }
                            if response.clicked() {
                                chosen = Some(entry.kind.clone());
                            }
                        }
                    });
                });
        });
        if let Some(kind) = chosen {
            return Outcome::Chosen(kind);
        }
        // Any press outside closes it, not only a click: starting a drag elsewhere (a new noodle,
        // a box select) must give the canvas its keys back at once.
        let pressed_outside = ctx.input(|i| i.pointer.primary_pressed() && i.pointer.press_origin().is_some_and(|p| !area.response.rect.contains(p)));
        if !first_frame && (pressed_outside || area.response.clicked_elsewhere()) {
            return Outcome::Closed;
        }
        Outcome::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, key: Option<char>) -> Entry {
        Entry { kind: kind.into(), label: kind.into(), key }
    }

    #[test]
    fn single_keys_are_one_plain_letter_or_digit_matched_case_insensitively() {
        assert_eq!(single_key(Some("R")), Some('r'));
        assert_eq!(single_key(Some("7")), Some('7'));
        for none in [None, Some(""), Some("Shift+R"), Some("RR"), Some("é"), Some("/")] {
            assert_eq!(single_key(none), None, "{none:?}");
        }
        let entries = [entry("structure.router", Some('r')), entry("gen.image", None)];
        assert_eq!(by_key(&entries, 'R').map(|e| e.kind.as_str()), Some("structure.router"));
        assert!(by_key(&entries, 'x').is_none());
    }

    #[test]
    fn builtins_offer_the_router_for_every_noodle() {
        let kinds = Registry::builtins().unwrap();
        let offered = entries(&kinds, PortType { modality: nodecraft_types::Modality::Image, many: false });
        assert!(offered.contains(&Entry { kind: nodecraft_nodes::router::ID.into(), label: "Router".into(), key: Some('r') }));
        // And the image actions (#410), by name.
        assert!(offered.iter().any(|e| e.kind == "image.blur" && e.label == "Blur"));
        assert!(!offered.iter().any(|e| e.kind.starts_with("text.")));
    }
}
