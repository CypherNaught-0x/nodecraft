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

/// Draws the strip; `focused` lets ←/→ step through results.
pub fn strip(ui: &mut egui::Ui, node: &Node, focused: bool, t: &Tokens) -> Option<HistoryIntent> {
    let count = node.history.len();
    if count == 0 {
        return None;
    }
    let active = node.active.and_then(|id| node.history.iter().position(|g| g.id == id));
    let mut intent = None;
    ui.horizontal(|ui| {
        let older = active.is_none_or(|index| index > 0);
        let newer = active.is_none_or(|index| index + 1 < count);
        if ui.add_enabled(older, egui::Button::new("‹")).on_hover_text("Previous result (←)").clicked() {
            intent = Some(HistoryIntent::Previous);
        }
        let position = active.map_or_else(|| format!("– / {count}"), |index| format!("{} / {count}", index + 1));
        ui.label(RichText::new(position).color(t.text_muted));
        if ui.add_enabled(newer, egui::Button::new("›")).on_hover_text("Next result (→)").clicked() {
            intent = Some(HistoryIntent::Next);
        }
        if ui.button("Clear").on_hover_text("Remove every result of this node (undoable)").clicked() {
            intent = Some(HistoryIntent::Clear);
        }
        if focused {
            let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
            if left && older {
                intent = Some(HistoryIntent::Previous);
            } else if right && newer {
                intent = Some(HistoryIntent::Next);
            }
        }
    });
    egui::ScrollArea::horizontal().id_salt("history").auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (index, generation) in node.history.iter().enumerate() {
                let is_active = active == Some(index);
                let label = format!("{}", index + 1);
                let chip = ui.add(egui::Button::selectable(is_active, label).sense(Sense::click()));
                let state = if is_active { ", active" } else { "" };
                chip.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, is_active, format!("Result {} of {count}{state}", index + 1)));
                if chip.clicked() && !is_active {
                    intent = Some(HistoryIntent::Select(generation.id));
                }
                let chip = chip.on_hover_text(preview(generation, t));
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
    });
    intent
}
