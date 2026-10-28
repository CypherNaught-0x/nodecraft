//! The history strip on a card with results (#81): step through them, pick the active one
//! (what downstream nodes use), and delete, extract, export or clear them. It only reports intents; the
//! app turns them into `history.*` commands.
use crate::theme::Tokens;
use egui::{RichText, Sense};
use nodecraft_graph::{Generation, Node};
use nodecraft_types::{GenerationId, Value};

/// Rows of controls the strip draws: the step controls, then the results.
pub const ROWS: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryIntent {
    Select(GenerationId),
    Previous,
    Next,
    Delete(GenerationId),
    /// Extract one output (`port`, `None` for the first) or one item of a list output.
    Extract {
        generation: GenerationId,
        port: Option<String>,
        item: Option<usize>,
    },
    /// Save one media output (or list item) to a file; `name` is the suggested file name.
    Export {
        generation: GenerationId,
        port: Option<String>,
        item: Option<usize>,
        name: String,
    },
    /// Save this exact historical media through the shared batch-save preview and worker.
    SaveToFolder {
        generation: GenerationId,
        port: Option<String>,
        item: Option<usize>,
    },
    /// Put one text or image output (or list item) on the OS clipboard.
    Copy {
        generation: GenerationId,
        port: Option<String>,
        item: Option<usize>,
    },
    Clear,
}

/// The strip's drawn height: two rows of controls.
pub fn height(t: &Tokens) -> f32 {
    (t.body_size + t.control_gap) * ROWS as f32 + t.control_gap
}

/// A short, single-line description of a result for its tooltip.
pub fn preview(generation: &Generation, t: &Tokens) -> String {
    let first = generation.outputs.first();
    // Text is clipped while borrowed: a result can be megabytes long.
    if let Some(Value::Text(text)) = first {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
        return if line.is_empty() { "Empty text".to_owned() } else { clip(line, t.preview_chars) };
    }
    let text = match first {
        Some(Value::Text(_)) => String::new(),
        Some(Value::Media(asset)) => match (asset.meta.width, asset.meta.height) {
            (Some(w), Some(h)) => format!("{} {w}×{h}", asset.mime.essence()),
            _ => asset.mime.essence().to_owned(),
        },
        Some(Value::List(items)) => format!("{} items", nodecraft_engine::history_cmds::list_items(items, !generation.items.is_empty()).count()),
        Some(_) => "A value".to_owned(),
        None => "No output".to_owned(),
    };
    clip(&text, t.preview_chars)
}

/// The first `chars` characters of `text`, with an ellipsis when there were more.
fn clip(text: &str, chars: usize) -> String {
    let mut rest = text.chars();
    let mut clipped: String = rest.by_ref().take(chars).collect();
    if rest.next().is_some() {
        clipped.push('…');
    }
    clipped
}

/// Most list items offered for extraction from one output.
pub const MAX_EXTRACT_ITEMS: usize = 12;

/// What the Extract menu offers for a result: each output, or each item of a list output (up to
/// [`MAX_EXTRACT_ITEMS`], a batch-fanned result's media counted one by one), labelled for the menu.
pub fn extract_targets(node: &Node, generation: &Generation) -> Vec<(String, Option<String>, Option<usize>)> {
    let mut targets = Vec::new();
    // Every modality has a source node to hold it (PDFs since #113).
    for (port, value) in node.outputs.iter().zip(&generation.outputs) {
        match value {
            Value::List(items) => {
                let count = nodecraft_engine::history_cmds::list_items(items, !generation.items.is_empty()).count();
                for item in 0..count.min(MAX_EXTRACT_ITEMS) {
                    targets.push((format!("{}: item {} of {}", port.name, item + 1, count), Some(port.name.clone()), Some(item)));
                }
            }
            _ => targets.push((port.name.clone(), Some(port.name.clone()), None)),
        }
    }
    targets
}

/// What the Copy menu offers for a result: each text or image output, or each such item of a
/// list output (up to [`MAX_EXTRACT_ITEMS`]), labelled for the menu.
pub fn copy_targets(node: &Node, generation: &Generation) -> Vec<(String, Option<String>, Option<usize>)> {
    let mut targets = Vec::new();
    for (port, value) in node.outputs.iter().zip(&generation.outputs) {
        // As `history.copy` decides: text, or media from an image output.
        let copies = |value: &Value| match value {
            Value::Text(_) => true,
            Value::Media(_) => port.port_type.modality == nodecraft_types::Modality::Image,
            _ => false,
        };
        match value {
            Value::List(items) => {
                let items = || nodecraft_engine::history_cmds::list_items(items, !generation.items.is_empty());
                let count = items().count();
                for (item, value) in items().enumerate().take(MAX_EXTRACT_ITEMS) {
                    if copies(value) {
                        targets.push((format!("{}: item {} of {}", port.name, item + 1, count), Some(port.name.clone()), Some(item)));
                    }
                }
            }
            value if copies(value) => targets.push((port.name.clone(), Some(port.name.clone()), None)),
            _ => {}
        }
    }
    targets
}

pub type ExportTarget = (String, Option<String>, Option<usize>, String);

/// First twelve media leaves per output, with stable export indices that also count non-media
/// leaves. Scan the entire bounded collection before exposing any targets; exhaustion is a refusal.
pub fn export_targets(node: &Node, generation: &Generation) -> Result<Vec<ExportTarget>, nodecraft_types::Error> {
    if node.outputs.len() != generation.outputs.len() {
        return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Conflict, "This result was made for different outputs than the node has now"));
    }
    let mut targets = Vec::new();
    let mut budget = nodecraft_engine::export_leaves::Budget::default();
    for (port, value) in node.outputs.iter().zip(&generation.outputs) {
        let mut offered = Vec::new();
        let mut count = 0usize;
        for leaf in nodecraft_engine::export_leaves::iter(value, !generation.items.is_empty(), &mut budget) {
            let leaf = leaf?;
            count = count.saturating_add(1);
            if let Value::Media(asset) = leaf.value
                && offered.len() < MAX_EXTRACT_ITEMS
            {
                offered.push((leaf.item, suggested_name(asset.mime.essence())));
            }
        }
        for (item, name) in offered {
            let label = item.map_or_else(|| port.name.clone(), |i| format!("{}: item {} of {}", port.name, i + 1, count));
            targets.push((label, Some(port.name.clone()), item, name));
        }
    }
    Ok(targets)
}

/// The file name the save dialog suggests for a result of type `mime`.
pub fn suggested_name(mime: &str) -> String {
    let extension = if mime == "audio/wave" { "wav".into() } else { nodecraft_format::export::extension_for(mime).unwrap_or_else(|| "bin".into()) };
    format!("result.{extension}")
}

/// Most result chips drawn at once, centred on the active result; the position label and ‹ ›
/// reach the rest. Keeps a node with hundreds of results as cheap per frame as one with a few.
pub const MAX_CHIPS: usize = 12;

/// The range of results that get a chip when `fit` chips fit the strip (at most
/// [`MAX_CHIPS`], at least one), centred on the active result (the newest when none is).
pub fn window(count: usize, active: Option<usize>, fit: usize) -> std::ops::Range<usize> {
    let shown = fit.clamp(1, MAX_CHIPS);
    let centre = active.unwrap_or(count.saturating_sub(1));
    let start = centre.saturating_sub(shown / 2).min(count.saturating_sub(shown));
    start..start.saturating_add(shown).min(count)
}

/// How many chips of the widest label (`count`'s digits) fit in `ui`'s row.
fn chips_that_fit(ui: &egui::Ui, count: usize) -> usize {
    let widest = egui::WidgetText::from("0".repeat(count.to_string().len())).into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);
    let spacing = ui.spacing();
    let chip = galley_width(&widest) + spacing.button_padding.x * 2.0 + spacing.item_spacing.x;
    let room = ui.available_width() + spacing.item_spacing.x;
    if !(chip.is_finite() && chip > 0.0 && room.is_finite() && room > 0.0) {
        return 1;
    }
    // Bounded by MAX_CHIPS before the cast, so it can't overflow.
    (room / chip).floor().clamp(1.0, MAX_CHIPS as f32) as usize
}

fn galley_width(galley: &std::sync::Arc<egui::Galley>) -> f32 {
    galley.size().x
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
    // After ←/→ from a focused chip, focus moves to the new active chip in the next frame (the
    // window may have moved). Only then: clicking elsewhere must be able to take focus away.
    let follow_key = ui.id().with(("history_chip_focus", node.id));
    let follow = ui.data(|d| d.get_temp::<bool>(follow_key)).unwrap_or(false);
    let mut button_focus = false;
    ui.horizontal(|ui| {
        let previous = named(ui.add_enabled(older, egui::Button::new("‹")), "Previous result");
        button_focus |= focus_on_click(&previous);
        if previous.on_hover_text("Previous result (←)").clicked() {
            intent = Some(HistoryIntent::Previous);
        }
        let position = active.map_or_else(|| format!("– / {count}"), |index| format!("{} / {count}", index + 1));
        ui.label(RichText::new(position).color(t.text_muted));
        let next = named(ui.add_enabled(newer, egui::Button::new("›")), "Next result");
        button_focus |= focus_on_click(&next);
        if next.on_hover_text("Next result (→)").clicked() {
            intent = Some(HistoryIntent::Next);
        }
        let clear = named(ui.button("Clear"), "Clear results");
        button_focus |= clear.has_focus();
        if clear.on_hover_text("Remove every result of this node (undoable)").clicked() {
            intent = Some(HistoryIntent::Clear);
        }
    });
    focused |= button_focus;
    let mut chip_focus = false;
    ui.horizontal(|ui| {
        let shown = window(count, active, chips_that_fit(ui, count));
        for (index, generation) in node.history.iter().enumerate().skip(shown.start).take(shown.len()) {
            let is_active = active == Some(index);
            // Keyed by the result, so focus stays with it when the window moves.
            let chip = ui.push_id(generation.id, |ui| ui.add(egui::Button::selectable(is_active, format!("{}", index + 1)).sense(Sense::click()))).inner;
            let state = if is_active { ", active" } else { "" };
            chip.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, is_active, format!("Result {} of {count}{state}", index + 1)));
            if is_active && follow && !button_focus && !chip.has_focus() {
                chip.request_focus();
            }
            chip_focus |= focus_on_click(&chip);
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
                let targets = extract_targets(node, generation);
                match targets.as_slice() {
                    [(_, port, item)] => {
                        if ui.button("Extract to canvas").clicked() {
                            intent = Some(HistoryIntent::Extract { generation: generation.id, port: port.clone(), item: *item });
                            ui.close();
                        }
                    }
                    [] => {
                        ui.add_enabled(false, egui::Button::new("Extract to canvas"));
                    }
                    _ => {
                        ui.menu_button("Extract to canvas", |ui| {
                            for (label, port, item) in &targets {
                                if ui.button(label).clicked() {
                                    intent = Some(HistoryIntent::Extract { generation: generation.id, port: port.clone(), item: *item });
                                    ui.close();
                                }
                            }
                        });
                    }
                }
                let result = export_targets(node, generation);
                let targets = result.as_deref().unwrap_or_default();
                match targets {
                    [(_, port, item, name)] => {
                        if ui.button("Export…").clicked() {
                            intent = Some(HistoryIntent::Export { generation: generation.id, port: port.clone(), item: *item, name: name.clone() });
                            ui.close();
                        }
                    }
                    [] => {
                        ui.add_enabled(false, egui::Button::new("Export…"))
                            .on_disabled_hover_text(result.as_ref().err().map_or("Only media results export to a file.", |e| e.message.as_str()));
                    }
                    _ => {
                        ui.menu_button("Export…", |ui| {
                            for (label, port, item, name) in targets {
                                if ui.button(label).clicked() {
                                    intent = Some(HistoryIntent::Export { generation: generation.id, port: port.clone(), item: *item, name: name.clone() });
                                    ui.close();
                                }
                            }
                        });
                    }
                }
                let targets = copy_targets(node, generation);
                match targets.as_slice() {
                    [(_, port, item)] => {
                        if ui.button("Copy").clicked() {
                            intent = Some(HistoryIntent::Copy { generation: generation.id, port: port.clone(), item: *item });
                            ui.close();
                        }
                    }
                    [] => {
                        ui.add_enabled(false, egui::Button::new("Copy")).on_disabled_hover_text("Only text and image results copy.");
                    }
                    _ => {
                        ui.menu_button("Copy", |ui| {
                            for (label, port, item) in &targets {
                                if ui.button(label).clicked() {
                                    intent = Some(HistoryIntent::Copy { generation: generation.id, port: port.clone(), item: *item });
                                    ui.close();
                                }
                            }
                        });
                    }
                }
                if ui.button("Delete result").clicked() {
                    intent = Some(HistoryIntent::Delete(generation.id));
                    ui.close();
                }
            });
        }
    });
    focused |= chip_focus;
    let mut stepped = false;
    if focused && intent.is_none() {
        let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
        if left && older {
            intent = Some(HistoryIntent::Previous);
        } else if right && newer {
            intent = Some(HistoryIntent::Next);
        }
        stepped = intent.is_some();
    }
    ui.data_mut(|d| d.insert_temp(follow_key, stepped && chip_focus));
    intent
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::Port;
    use nodecraft_types::{Modality, NodeId, PortType};

    fn result(outputs: Vec<Value>) -> Generation {
        Generation::new(GenerationId::from_bytes([1; 16]), outputs)
    }

    #[test]
    fn previews_skip_blank_lines_and_clip_unicode_by_character() {
        let t = crate::theme::DARK;
        assert_eq!(preview(&result(vec![Value::Text("\n  \n  Ünïcödé forest  \nsecond".into())]), &t), "Ünïcödé forest");
        assert_eq!(preview(&result(vec![Value::Text(" \n\t".into())]), &t), "Empty text");
        let long = "é".repeat(t.preview_chars + 5);
        let clipped = preview(&result(vec![Value::Text(long)]), &t);
        assert_eq!(clipped.chars().count(), t.preview_chars + 1);
        assert!(clipped.ends_with('…'));
        assert_eq!(preview(&result(vec![]), &t), "No output");
        assert_eq!(preview(&result(vec![Value::List(vec![Value::Bool(true); 3])]), &t), "3 items");
    }

    #[test]
    fn extract_offers_each_output_and_list_item() {
        let text = |many| PortType { modality: Modality::Text, many };
        let mut node = Node::new(NodeId::from_bytes([2; 16]), "test.gen");
        node.outputs = vec![Port { name: "caption".into(), port_type: text(false) }, Port { name: "takes".into(), port_type: text(true) }];
        let items = Value::List((0..20).map(|n| Value::Text(format!("{n}"))).collect());
        let targets = extract_targets(&node, &result(vec![Value::Text("c".into()), items]));
        assert_eq!(targets.len(), 1 + MAX_EXTRACT_ITEMS);
        assert_eq!(targets[0], ("caption".into(), Some("caption".into()), None));
        assert_eq!(targets[1], ("takes: item 1 of 20".into(), Some("takes".into()), Some(0)));
        node.outputs.push(Port { name: "report".into(), port_type: PortType { modality: Modality::Document, many: false } });
        let targets = extract_targets(&node, &result(vec![Value::Text("c".into()), Value::List(vec![]), Value::Text("m".into())]));
        assert_eq!(targets.len(), 2, "an empty list offers nothing; a document output is offered");
        assert_eq!(targets[1], ("report".into(), Some("report".into()), None));
        // A batch-fanned result offers its values one by one, not each item's list (#519).
        let texts = |t: &[&str]| t.iter().map(|t| Value::Text((*t).into())).collect::<Vec<_>>();
        let fanned = Value::List(vec![Value::List(texts(&["a", "b"])), Value::List(texts(&["c"]))]);
        let key = |n: u8| nodecraft_graph::ItemKey { batch: NodeId::from_bytes([30; 16]), item: nodecraft_types::ItemId::from_bytes([n; 16]) };
        let by_items = |outputs: Vec<Value>| Generation { items: vec![vec![key(1)], vec![key(2)]], ..result(outputs) };
        let targets = extract_targets(&node, &by_items(vec![Value::Text("c".into()), fanned.clone(), Value::Text("m".into())]));
        assert_eq!(targets.iter().rev().nth(1), Some(&("takes: item 3 of 3".into(), Some("takes".into()), Some(2))), "the last item, then the document");
        assert_eq!(preview(&by_items(vec![fanned.clone()]), &crate::theme::DARK), "3 items");
        // A list of lists no batch made is two values.
        assert_eq!(preview(&result(vec![fanned]), &crate::theme::DARK), "2 items");
    }

    #[test]
    fn the_chip_window_is_bounded_and_follows_the_active_result() {
        assert_eq!(window(0, None, 12), 0..0);
        assert_eq!(window(5, Some(2), 12), 0..5);
        assert_eq!(window(1000, None, 12), 988..1000, "the newest when none is active");
        assert_eq!(window(1000, Some(0), 12), 0..12);
        assert_eq!(window(1000, Some(500), 12), 494..506);
        assert_eq!(window(1000, Some(999), 12), 988..1000);
        assert_eq!(window(1000, Some(500), 7), 497..504, "only what fits");
        assert_eq!(window(1000, Some(500), 0), 500..501, "at least the active one");
        assert_eq!(window(1000, Some(500), 99), 494..506, "never more than MAX_CHIPS");
        for fit in 1..=MAX_CHIPS {
            for active in 0..40 {
                let shown = window(40, Some(active), fit);
                assert!(shown.contains(&active) && shown.len() == fit);
            }
        }
    }
}
