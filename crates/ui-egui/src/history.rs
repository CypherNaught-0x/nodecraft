//! The history strip on a card with results (#81): step through them, pick the active one
//! (what downstream nodes use), and delete, extract or clear them. It only reports intents; the
//! app turns them into `history.*` commands.
use crate::theme::Tokens;
use egui::{RichText, Sense};
use nodecraft_graph::{Generation, Node};
use nodecraft_types::{GenerationId, Value};

/// Rows of controls the strip draws: the step controls, then the results.
pub const ROWS: usize = 2;
/// Card rows (`control_row_height` each) it reserves at the bottom of a card; one fits both of
/// its rows of controls.
pub const RESERVED_ROWS: usize = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryIntent {
    Select(GenerationId),
    Previous,
    Next,
    Delete(GenerationId),
    Extract(GenerationId),
    Clear,
}

/// The strip's drawn height: two rows of controls.
pub fn height(spacing: &egui::Spacing) -> f32 {
    spacing.interact_size.y * ROWS as f32 + spacing.item_spacing.y
}

/// Rows a node's card needs for its history strip.
pub fn rows(node: &Node) -> usize {
    if node.history.is_empty() { 0 } else { RESERVED_ROWS }
}

/// A short, single-line description of a result for its tooltip.
pub fn preview(generation: &Generation, t: &Tokens) -> String {
    let first = generation.outputs.first();
    let text = match first {
        Some(Value::Text(text)) => text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim().to_owned(),
        Some(Value::Media(asset)) => match (asset.meta.width, asset.meta.height) {
            (Some(w), Some(h)) => format!("{} {w}×{h}", asset.mime.essence()),
            _ => asset.mime.essence().to_owned(),
        },
        Some(Value::List(items)) => format!("{} items", items.len()),
        Some(_) => "A value".to_owned(),
        None => "No output".to_owned(),
    };
    let mut chars = text.chars();
    let mut clipped: String = chars.by_ref().take(t.preview_chars).collect();
    if chars.next().is_some() {
        clipped.push('…');
    }
    if clipped.is_empty() { "Empty text".to_owned() } else { clipped }
}

/// Most result chips drawn at once, centred on the active result; the position label and ‹ ›
/// reach the rest. Keeps a node with hundreds of results as cheap per frame as one with a few.
pub const MAX_CHIPS: usize = 12;

/// The range of results that get a chip.
pub fn window(count: usize, active: Option<usize>) -> std::ops::Range<usize> {
    let centre = active.unwrap_or(count.saturating_sub(1));
    let start = centre.saturating_sub(MAX_CHIPS / 2).min(count.saturating_sub(MAX_CHIPS));
    start..start.saturating_add(MAX_CHIPS).min(count)
}

/// Gives a glyph button a spoken name.
fn named(response: egui::Response, name: &str) -> egui::Response {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), name));
    response
}

/// A pointer click doesn't focus an egui button; take focus so ←/→ keep stepping from here.
fn focus_on_click(response: &egui::Response) -> bool {
    if response.clicked() {
        response.request_focus();
    }
    response.has_focus()
}

/// Draws the strip. ←/→ step through results while the card (`card_focused`) or any control
/// of the strip has keyboard focus.
pub fn strip(ui: &mut egui::Ui, node: &Node, card_focused: bool, t: &Tokens) -> Option<HistoryIntent> {
    let count = node.history.len();
    if count == 0 {
        return None;
    }
    let active = node.active.and_then(|id| node.history.iter().position(|g| g.id == id));
    let older = active.is_none_or(|index| index > 0);
    let newer = active.is_none_or(|index| index + 1 < count);
    let mut intent = None;
    let mut focused = card_focused;
    ui.horizontal(|ui| {
        let previous = named(ui.add_enabled(older, egui::Button::new("‹")), "Previous result");
        focused |= focus_on_click(&previous);
        if previous.on_hover_text("Previous result (←)").clicked() {
            intent = Some(HistoryIntent::Previous);
        }
        let position = active.map_or_else(|| format!("– / {count}"), |index| format!("{} / {count}", index + 1));
        ui.label(RichText::new(position).color(t.text_muted));
        let next = named(ui.add_enabled(newer, egui::Button::new("›")), "Next result");
        focused |= focus_on_click(&next);
        if next.on_hover_text("Next result (→)").clicked() {
            intent = Some(HistoryIntent::Next);
        }
        let clear = named(ui.button("Clear"), "Clear results");
        focused |= clear.has_focus();
        if clear.on_hover_text("Remove every result of this node (undoable)").clicked() {
            intent = Some(HistoryIntent::Clear);
        }
    });
    ui.horizontal(|ui| {
        let shown = window(count, active);
        for (index, generation) in node.history.iter().enumerate().skip(shown.start).take(shown.len()) {
            let is_active = active == Some(index);
            let chip = ui.add(egui::Button::selectable(is_active, format!("{}", index + 1)).sense(Sense::click()));
            let state = if is_active { ", active" } else { "" };
            chip.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, is_active, format!("Result {} of {count}{state}", index + 1)));
            focused |= focus_on_click(&chip);
            if chip.clicked() && !is_active {
                intent = Some(HistoryIntent::Select(generation.id));
            }
            // Built only while hovered: a preview walks the result's text.
            let chip = if chip.hovered() { chip.on_hover_text(preview(generation, t)) } else { chip };
            chip.context_menu(|ui| {
                if ui.add_enabled(!is_active, egui::Button::new("Use this result")).clicked() {
                    intent = Some(HistoryIntent::Select(generation.id));
                    ui.close();
                }
                if ui.button("Extract to canvas").clicked() {
                    intent = Some(HistoryIntent::Extract(generation.id));
                    ui.close();
                }
                if ui.button("Delete result").clicked() {
                    intent = Some(HistoryIntent::Delete(generation.id));
                    ui.close();
                }
            });
        }
    });
    if focused && intent.is_none() {
        let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
        if left && older {
            intent = Some(HistoryIntent::Previous);
        } else if right && newer {
            intent = Some(HistoryIntent::Next);
        }
    }
    intent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chip_window_is_bounded_and_follows_the_active_result() {
        assert_eq!(window(0, None), 0..0);
        assert_eq!(window(5, Some(2)), 0..5);
        assert_eq!(window(1000, None), 988..1000, "the newest when none is active");
        assert_eq!(window(1000, Some(0)), 0..12);
        assert_eq!(window(1000, Some(500)), 494..506);
        assert_eq!(window(1000, Some(999)), 988..1000);
        for active in 0..40 {
            let shown = window(40, Some(active));
            assert!(shown.contains(&active) && shown.len() == MAX_CHIPS);
        }
    }
}
