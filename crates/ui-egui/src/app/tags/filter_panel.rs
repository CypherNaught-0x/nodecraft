//! Presentation for the floating tag filter. Intents still dispatch through the scoped host.
use super::*;
use crate::{icons::Icon, tags::TagFilter};
use nodecraft_graph::Tag;

pub(super) struct Interaction {
    pub action: Option<&'static str>,
    pub filter: Option<(BTreeSet<TagId>, bool)>,
    pub definition: Option<(Tag, bool)>,
    pub focus: Vec<egui::Id>,
    pub buttons: Vec<egui::Id>,
}

pub(super) fn header(ui: &mut egui::Ui, allowed: bool, focus_pending: &mut bool, t: &theme::Tokens) -> (Rect, (egui::Response, egui::Response)) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), t.navigation_toolbar_height), Sense::hover());
    let close_rect = Rect::from_min_max(Pos2::new(rect.right() - rect.height().min(rect.width()), rect.top()), rect.max);
    let handle = rect.with_max_x(close_rect.left());
    let title = ui.place(handle, egui::Label::new(egui::RichText::new("Tag filter").heading()).sense(Sense::focusable_noninteractive()).truncate());
    if allowed && *focus_pending && ui.is_visible() {
        title.request_focus();
        *focus_pending = false;
    }
    let close = ui
        .add_enabled_ui(allowed, |ui| ui.place(close_rect, egui::Button::image(Icon::Close.image(ui.ctx(), t.icon_size, t)).frame(false)))
        .inner
        .on_hover_text("Close tags");
    close.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, allowed, "Close tags"));
    (handle, (title, close))
}

pub(super) fn body(ui: &mut egui::Ui, canvas: Option<&Canvas>, state: &mut TagFilter, allowed: bool, t: &theme::Tokens) -> Interaction {
    crate::comments::checkbox_visuals(ui.visuals_mut(), t);
    let search = ui.add_enabled(
        allowed,
        egui::TextEdit::singleline(&mut state.query_draft).hint_text("Find a tag…").char_limit(crate::tags::QUERY_CHARS).desired_width(ui.available_width()),
    );
    search.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, allowed, "Find a tag"));
    if search.changed() {
        state.update_query();
        ui.ctx().request_repaint();
    }

    let mut result = Interaction { action: None, filter: None, definition: None, focus: vec![search.id], buttons: Vec::new() };
    ui.separator();
    if let Some(canvas) = canvas {
        list(ui, canvas, state, allowed, t, &mut result);
    }

    let mut untagged = state.untagged;
    let untagged_response = counted_row(ui, None, state.count(None), None, allowed, |_| checkbox(&mut untagged, "Untagged"), t);
    result.focus.push(untagged_response.id);
    if untagged_response.changed() {
        result.filter = Some((state.tags.clone(), untagged));
    }
    ui.separator();

    let count = state.results.len();
    let status = if state.scanning {
        "Finding matching cards…".to_owned()
    } else if let Some(index) = state.current_index() {
        format!("{} of {count} matches", index + 1)
    } else if state.active() {
        let noun = if count == 1 { "match" } else { "matches" };
        format!("{count} {noun}")
    } else {
        "0 matches · choose tags".to_owned()
    };
    let status = ui.add(egui::Label::new(egui::RichText::new(status).color(t.text_muted)).truncate());
    ui.ctx().accesskit_node_builder(status.id, |node| {
        node.set_live(egui::accesskit::Live::Polite);
        node.set_live_atomic();
    });

    let steps = allowed && state.active() && (state.scanning || count > 0);
    ui.horizontal_wrapped(|ui| {
        let labels = ui.available_width() >= t.tag_panel_width - 2.0 * t.card_padding - t.navigation_gap;
        for (icon, label, accessible, enabled, command) in [
            (Icon::Previous, "Previous", "Previous match", steps, Some("canvas.tag_filter_previous")),
            (Icon::Next, "Next", "Next match", steps, Some("canvas.tag_filter_next")),
            (Icon::Clear, "Clear", "Clear tag filter", allowed && state.active(), None),
        ] {
            let button = if labels { icon.button(ui, label, t) } else { egui::Button::image(icon.image(ui.ctx(), t.icon_size, t)) };
            let response = ui.add_enabled(enabled, button.frame(false)).on_hover_text(accessible);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, accessible));
            result.focus.push(response.id);
            result.buttons.push(response.id);
            if response.clicked() {
                if let Some(command) = command {
                    result.action = Some(command);
                } else {
                    result.filter = Some((BTreeSet::new(), false));
                }
            }
        }
    });
    result
}

fn list(ui: &mut egui::Ui, canvas: &Canvas, state: &TagFilter, allowed: bool, t: &theme::Tokens, result: &mut Interaction) {
    let ids = state.visible_tags();
    if canvas.tags().is_empty() {
        ui.label("No tags yet. Add a colour tag to a card to filter by it.");
    } else if ids.is_empty() {
        ui.label(if state.indexing {
            "Loading tags…"
        } else if state.query_pending() {
            "Finding tags…"
        } else {
            "No tags match this search."
        });
    } else {
        // Keep the Untagged row, footer and dividers outside the virtualized list.
        let footer = t.tag_definition_row_height + ui.spacing().interact_size.y * 2.0 + t.navigation_gap * 3.0;
        let height = (ui.available_height() - footer).max(t.tag_definition_row_height);
        egui::ScrollArea::vertical().id_salt("filter_tags").max_height(height).show_rows(ui, t.tag_definition_row_height, ids.len(), |ui, range| {
            for id in ids.get(range).unwrap_or_default() {
                let Some(tag) = canvas.tags().get(id) else {
                    continue;
                };
                let mut selected = state.tags.contains(id);
                let row = counted_row(
                    ui,
                    Some(*id),
                    state.count(Some(*id)),
                    Some(t.tag_color(tag.color.as_deref())),
                    allowed,
                    |ui| checkbox(&mut selected, tag_definitions::row_text(ui, tag, "", true, t)),
                    t,
                );
                let identity = format!("{}\n{}", tag.id, crate::tags::label(&tag.title));
                row.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, allowed, selected, &identity));
                let row = row.on_hover_text(format!("{identity}\nRight-click or Shift+F10 to rename or delete."));
                let keyboard_menu = allowed && row.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F10));
                if keyboard_menu {
                    egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&row));
                }
                egui::Popup::context_menu(&row).anchor(&row).show(|ui| {
                    let rename = ui.button("Rename tag…");
                    if keyboard_menu {
                        rename.request_focus();
                    }
                    if rename.clicked() {
                        result.definition = Some((tag.clone(), false));
                        ui.close();
                    }
                    if ui.button("Delete tag…").clicked() {
                        result.definition = Some((tag.clone(), true));
                        ui.close();
                    }
                });
                result.focus.push(row.id);
                if row.changed() {
                    let mut tags = state.tags.clone();
                    if selected {
                        tags.insert(*id);
                    } else {
                        tags.remove(id);
                    }
                    result.filter = Some((tags, state.untagged));
                }
            }
        });
    }
}

fn counted_row<'widget>(
    ui: &mut egui::Ui,
    tag: Option<TagId>,
    count: Option<usize>,
    color: Option<egui::Color32>,
    allowed: bool,
    widget: impl FnOnce(&mut egui::Ui) -> egui::Checkbox<'widget>,
    t: &theme::Tokens,
) -> egui::Response {
    ui.push_id(("tag_filter_row", tag), |ui| {
        let size = Vec2::new(ui.available_width(), t.tag_definition_row_height);
        ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = count.map_or_else(|| "…".into(), |count| count.to_string());
            ui.add(egui::Label::new(egui::RichText::new(label).color(t.text_muted)).selectable(false));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(t.tag_marker_width), Sense::hover());
                if let Some(color) = color {
                    ui.painter().circle_filled(rect.center(), rect.width() / 2.0, color);
                }
                let checkbox = widget(ui);
                let response = tag_definitions::row(ui, allowed, checkbox, t);
                ui.ctx().accesskit_node_builder(response.id, |node| {
                    node.set_description(match count {
                        None => "Counting cards".into(),
                        Some(1) => "1 card".into(),
                        Some(count) => format!("{count} cards"),
                    });
                });
                response
            })
            .inner
        })
        .inner
    })
    .inner
}

fn checkbox<'a>(selected: &'a mut bool, label: impl egui::IntoAtoms<'a>) -> egui::Checkbox<'a> {
    let mut atoms = label.into_atoms();
    atoms.push_right(egui::Atom::grow());
    egui::Checkbox::new(selected, atoms)
}
