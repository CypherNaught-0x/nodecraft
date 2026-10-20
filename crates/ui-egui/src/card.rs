//! Graph-backed cards. Widgets keep only drafts; all accepted edits become engine commands.
use crate::{
    canvas::CanvasView,
    history::{self, HistoryIntent},
    noodle::{self, Side},
    theme::Tokens,
};
use egui::{FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use nodecraft_graph::{Node, Position};
use nodecraft_nodes::NodeDescriptor;
use nodecraft_types::{Integer, Number, ParamControl, Unsigned, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashSet},
};

pub(crate) mod generate;
pub use generate::Roles;

pub const BODY_MIN_ZOOM: f32 = 0.55;
pub const MIN_TEXT_PX: f32 = 5.0;

#[derive(Default)]
pub struct EditorState {
    title: Option<Draft>,
    text: BTreeMap<String, Draft>,
    sliders: BTreeMap<String, SliderDraft>,
    error: Option<(EditTarget, String)>,
    pub hidden_dirty: bool,
    choices: HashSet<egui::Id>,
    draft_limit: Option<usize>,
    focus_request: Option<egui::Id>,
    text_ids: BTreeMap<String, egui::Id>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum EditTarget {
    Title,
    Parameter(String),
}
struct SliderDraft {
    original: f64,
    value: f64,
}
struct Draft {
    original: String,
    text: String,
    focus: bool,
    was_focused: bool,
}

pub enum Intent {
    Move(Position),
    Title(String),
    Parameter {
        name: String,
        value: Option<Value>,
    },
    Model,
    History(HistoryIntent),
    /// Quote a run of this card and ask before starting it (#83).
    Generate,
    /// Record the answer a cancelled or failed run left on this text card (#504).
    KeepPartial,
    /// Let that answer go.
    DiscardPartial,
}
pub struct CardResponse {
    pub response: egui::Response,
    pub intents: Vec<Intent>,
    /// Port handles by side and port name, for starting and rerouting noodles.
    pub ports: Vec<(Side, String, egui::Response)>,
}

pub fn text_px(size: f32, zoom: f32) -> Option<f32> {
    let px = (size * zoom).round();
    (px.is_finite() && px >= MIN_TEXT_PX).then_some(px)
}
/// Pointer slop around a port, in screen points, so small dots stay easy to grab when zoomed out.
pub const PORT_HIT: f32 = 10.0;

/// Automatic titles are derived view data, never written back during rendering.
pub fn title(node: &Node, descriptor: Option<&NodeDescriptor>) -> String {
    let fallback = || descriptor.map_or("Unknown node", |d| d.label.as_str());
    let text = if node.title.trim().is_empty() {
        ["prompt", "text"]
            .into_iter()
            .find_map(|name| match node.params.get(name) {
                Some(Value::Text(text)) => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or_else(fallback)
    } else {
        &node.title
    };
    let prefix: String = text.chars().take(256).collect();
    let line = prefix.lines().find(|line| !line.trim().is_empty()).unwrap_or_else(fallback).trim();
    let mut chars = line.chars();
    let mut clipped: String = chars.by_ref().take(64).collect();
    if chars.next().is_some() {
        clipped.push('…');
    }
    clipped
}

/// Position-independent logical metrics, including the node's history strip.
/// Never recover these by subtracting large coordinates.
pub fn world_size(node: &Node, descriptor: Option<&NodeDescriptor>, t: &Tokens) -> Vec2 {
    let count = descriptor.map_or(1, |d| d.params.params.len().min(12));
    let params = (t.card_min_height + count.saturating_sub(1) as f32 * t.control_row_height).min(t.card_max_height);
    // The history strip sits under the parameters and is never squeezed out by them; an image
    // preview sits above them.
    let preview = if nodecraft_engine::split::image_of(node).is_some() { t.card_preview_height } else { 0.0 };
    // A text card always has room for its answer, so the card doesn't jump when one starts
    // streaming in (#504).
    let answer = if node.kind == nodecraft_nodes::generation::TEXT_KIND { t.card_preview_height } else { 0.0 };
    let height = params + preview + answer + history::rows(node) as f32 * t.control_row_height;
    Vec2::new(t.card_width, height)
}
pub fn world_rect(node: &Node, descriptor: Option<&NodeDescriptor>, t: &Tokens) -> Rect {
    Rect::from_min_size(Pos2::new(node.pos.x, node.pos.y), world_size(node, descriptor, t))
}

pub struct CardView<'a> {
    pub descriptor: Option<&'a NodeDescriptor>,
    pub visible: Option<&'a BTreeSet<String>>,
    /// Resolved editability; disabling retains field drafts.
    pub enabled: Option<&'a BTreeSet<String>>,
    pub preview_pos: Option<Position>,
    /// The node's images, when it has any (#386): one row per batch item of a fanned result
    /// (#519), else one row; and how many images the result holds, shown or not.
    pub images: (Vec<Vec<crate::preview::Preview>>, usize),
    /// Rows and columns to draw over the image while a split is being picked (#337), with the
    /// image's full size when known (so lines fall where the split will cut).
    pub grid: Option<(u32, u32, Option<[u32; 2]>)>,
    /// The parameters a generation card's composer draws itself (#83).
    pub roles: Option<&'a Roles>,
    /// A wire feeds the card's prompt port (#84).
    pub prompt_wired: bool,
    /// A text card's answer still arriving, or cut short (#504).
    pub partial: Option<generate::Partial<'a>>,
}

/// Positive distances outside the body, including the clipped title/UNSAVED notice and
/// minimum screen-sized output handle. Called only with a validated canvas zoom.
pub fn outer_insets(t: &Tokens, zoom: f32, output: bool) -> (f32, f32, f32, f32) {
    let top = t.label_size.max(t.title_min_screen_px / zoom) + t.label_gap + t.control_gap;
    let handle = if output { t.handle_radius.max(t.handle_min_screen_radius / zoom) + t.handle_stroke / (2.0 * zoom) } else { 0.0 };
    // The handle is centered on the right edge. Conservatively include its vertical extent.
    (0.0, top.max(handle), handle, handle)
}
pub fn cull_margin(t: &Tokens, zoom: f32) -> f32 {
    let (_, top, right, bottom) = outer_insets(t, zoom, true);
    top.max(right).max(bottom) * zoom
}

pub fn show(
    parent: &mut egui::Ui,
    view: &CanvasView,
    node: &Node,
    card: CardView<'_>,
    editor: &mut EditorState,
    selected: bool,
    t: &Tokens,
) -> Option<CardResponse> {
    let CardView { descriptor, visible, enabled, preview_pos, images, grid, roles, prompt_wired, partial } = card;
    let composer = descriptor.filter(|d| generate::has_composer(node, d));
    let origin = parent.max_rect().min;
    let mut rect = world_rect(node, descriptor, t);
    if let Some(pos) = preview_pos {
        rect = rect.translate(Vec2::new(pos.x - node.pos.x, pos.y - node.pos.y));
    }
    let screen = view.rect_to_screen(origin, rect);
    let extent = screen.expand(cull_margin(t, view.zoom));
    if !parent.clip_rect().intersects(extent) {
        editor.mark_hidden();
        return None;
    }
    let layer = egui::LayerId::new(parent.layer_id().order, parent.id().with(("node_layer", node.id)));
    parent.ctx().set_sublayer(parent.layer_id(), layer);
    let transform = egui::emath::TSTransform::new(origin.to_vec2() + view.pan, view.zoom);
    parent.ctx().set_transform_layer(layer, transform);
    let label_rect = Rect::from_min_size(
        rect.left_top() - Vec2::new(0.0, outer_insets(t, view.zoom, false).1),
        Vec2::new(rect.width(), t.label_size.max(t.title_min_screen_px / view.zoom) + t.control_gap),
    );
    let mut ui = parent.new_child(egui::UiBuilder::new().id_salt(("node", node.id)).layer_id(layer).max_rect(rect.union(label_rect)));
    // Egui layer transforms affect painting and pointer input, but AccessKit bounds remain
    // in world coordinates. Transform the card's accessibility subtree as well.
    ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
        node.set_transform(egui::accesskit::Affine::new([
            view.zoom as f64,
            0.0,
            0.0,
            view.zoom as f64,
            transform.translation.x as f64,
            transform.translation.y as f64,
        ]));
    });
    ui.set_clip_rect(transform.inverse() * parent.clip_rect());
    ui.style_mut().override_font_id = Some(FontId::proportional(t.body_size));
    ui.style_mut().spacing.item_spacing = Vec2::splat(t.control_gap);
    let response = ui.interact(rect, ui.id().with("card"), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), format!("Node {}", title(node, descriptor))));
    if response.clicked() {
        response.request_focus();
    }
    let mut intents = Vec::new();
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta = response.drag_delta();
        let pos = Position { x: preview_pos.unwrap_or(node.pos).x + delta.x, y: preview_pos.unwrap_or(node.pos).y + delta.y };
        if delta != Vec2::ZERO && pos.x.is_finite() && pos.y.is_finite() {
            intents.push(Intent::Move(pos));
        }
    }
    let stroke = if selected {
        t.card_stroke_selected
    } else if response.hovered() {
        t.card_stroke_hover
    } else {
        t.card_stroke
    };
    ui.painter().rect(rect, t.card_radius, t.card_fill, Stroke::new(1.0 / view.zoom, stroke), StrokeKind::Inside);

    {
        let px = (t.label_size * view.zoom).round().max(t.title_min_screen_px);
        if editor.title.as_ref().is_some_and(|draft| draft.original != node.title && draft.text == draft.original) {
            editor.title = None;
        }
        if let Some(draft) = &mut editor.title {
            let r = ui.put(
                label_rect,
                egui::TextEdit::singleline(&mut draft.text)
                    .id(ui.id().with("title_edit"))
                    .font(FontId::monospace(px / view.zoom))
                    .char_limit(4096)
                    .hint_text("Automatic title"),
            );
            if draft.focus {
                r.request_focus();
                draft.focus = false;
            }
            let owned_focus = r.has_focus() || r.lost_focus() || draft.was_focused;
            let cancel = owned_focus && ui.input(|i| i.key_pressed(egui::Key::Escape));
            let submit = owned_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));
            draft.was_focused = r.has_focus();
            if cancel {
                editor.title = None;
                r.surrender_focus();
            } else if (r.lost_focus() || submit) && draft.text != draft.original {
                intents.push(Intent::Title(draft.text.clone()));
                r.surrender_focus();
            } else if r.lost_focus() {
                editor.title = None;
            }
        } else {
            let label = title(node, descriptor).to_uppercase();
            let label = if editor.hidden_dirty { format!("{label} • UNSAVED") } else { label };
            let r = ui.interact(label_rect, ui.id().with("title"), Sense::click());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), format!("Rename {label}")));
            let mut job = egui::text::LayoutJob::simple_singleline(label, FontId::monospace(px / view.zoom), t.text_muted);
            job.wrap = egui::text::TextWrapping { max_width: label_rect.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            ui.painter().with_clip_rect(label_rect.intersect(ui.clip_rect())).galley(label_rect.left_top(), galley, t.text_muted);
            // Enter on a focused generation card generates; elsewhere it renames, as F2 does.
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let focused = r.has_focus() || response.has_focus();
            if composer.is_some() && focused && enter {
                intents.push(Intent::Generate);
            } else if r.double_clicked() || focused && (enter || ui.input(|i| i.key_pressed(egui::Key::F2))) {
                editor.title = Some(Draft { original: node.title.clone(), text: node.title.clone(), focus: true, was_focused: false });
            }
            r.on_hover_text("Double-click or press F2 to rename. Clear the title to use the prompt.");
        }
    }
    editor.hide_fields(visible);
    if view.zoom >= BODY_MIN_ZOOM {
        let mut inner = rect.shrink(t.card_padding);
        let strip = history::rows(node) as f32 * t.control_row_height;
        if strip > 0.0 {
            // The card reserves `strip`; the strip itself sits on the card's bottom edge.
            let (params, reserved) = inner.split_top_bottom_at_y(inner.bottom() - strip);
            inner = params;
            let height = (history::height(ui.spacing())).min(reserved.height());
            let results = Rect::from_min_max(Pos2::new(reserved.left(), reserved.bottom() - height), reserved.max);
            let mut strip_ui = ui.new_child(egui::UiBuilder::new().id_salt("history").max_rect(results));
            strip_ui.set_clip_rect(results.intersect(ui.clip_rect()));
            if let Some(intent) = history::strip(&mut strip_ui, node, response.has_focus(), t) {
                intents.push(Intent::History(intent));
            }
        }
        if !images.0.is_empty() {
            let (area, rest) = inner.split_top_bottom_at_y(inner.top() + t.card_preview_height);
            inner = rest;
            let area = area.shrink2(Vec2::new(0.0, t.control_gap));
            match images.0.as_slice() {
                [row] if row.len() == 1 && images.1 == 1 => {
                    if let Some(image) = row.first() {
                        show_image(&mut ui, area, image, grid, t);
                    }
                }
                // Picking a split shows the image it cuts, with its grid.
                rows if grid.is_some() => {
                    if let Some(image) = rows.first().and_then(|row| row.first()) {
                        show_image(&mut ui, area, image, grid, t);
                    }
                }
                rows => gallery(&mut ui, area, rows, images.1, t),
            }
        }
        if node.kind == nodecraft_nodes::generation::TEXT_KIND {
            let (area, rest) = inner.split_top_bottom_at_y(inner.top() + t.card_preview_height);
            inner = rest;
            generate::answer(&mut ui, area.shrink2(Vec2::new(0.0, t.control_gap)), generate::answer_of(node), partial, &mut intents, t);
        }
        if let Some(d) = composer {
            inner = generate::show(
                &mut ui,
                inner,
                generate::Composer { node, wired: prompt_wired, descriptor: d, roles, visible, enabled },
                editor,
                &mut intents,
                t,
            );
        }
        let own = |name: &str| composer.is_some() && (name == nodecraft_nodes::generation::PROMPT || roles.and_then(|r| r.count.as_deref()) == Some(name));
        let mut body = ui.new_child(egui::UiBuilder::new().id_salt("body").max_rect(inner));
        body.set_clip_rect(inner.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().id_salt("parameters").max_height(inner.height()).auto_shrink([false, false]).show(&mut body, |ui| {
            ui.set_width(inner.width());
            match descriptor {
                None => {
                    ui.label("This node kind or its controls are unavailable. Check its schema and view cache budget.");
                    if nodecraft_engine::generation_catalog::GenerationCatalog::is_model_selector(&node.kind, nodecraft_nodes::generation::MODEL)
                        && ui.button("Choose supported model…").clicked()
                    {
                        intents.push(Intent::Model);
                    }
                }
                Some(d) => {
                    for spec in &d.params.params {
                        if !visible.is_some_and(|visible| visible.contains(&spec.name)) || own(&spec.name) {
                            continue;
                        }
                        ui.push_id(&spec.name, |ui| {
                            let label = ui.label(egui::RichText::new(&spec.label).color(t.text_muted));
                            let label = if let Some(description) = &spec.description { label.on_hover_text(description) } else { label };
                            let value = node.params.get(&spec.name).or(spec.default.as_ref());
                            let editable = enabled.is_none_or(|names| names.contains(&spec.name));
                            if nodecraft_engine::generation_catalog::GenerationCatalog::is_model_selector(&node.kind, &spec.name) {
                                let current = match value {
                                    Some(Value::Text(id)) => id.as_str(),
                                    _ => "Unavailable saved model",
                                };
                                let preview: String = current.chars().take(t.preview_chars).collect();
                                ui.label(&preview);
                                if ui.add_enabled(editable, egui::Button::new("Change model…")).labelled_by(label.id).on_hover_text(&preview).clicked() {
                                    intents.push(Intent::Model);
                                }
                            } else {
                                let changed = if editable {
                                    control(ui, &spec.control, value, &spec.name, editor, label.id, t)
                                } else {
                                    ui.add_enabled_ui(false, |ui| control(ui, &spec.control, value, &spec.name, editor, label.id, t)).inner
                                };
                                if editable && let Some(value) = changed {
                                    intents.push(Intent::Parameter { name: spec.name.clone(), value });
                                }
                            }
                            ui.add_space(t.control_gap);
                        });
                    }
                }
            }

            if let Some((_, error)) = &editor.error {
                ui.label(error);
            }
        });
    }
    if view.zoom < BODY_MIN_ZOOM {
        editor.mark_hidden();
    }
    let mut ports = Vec::new();
    let hit = PORT_HIT / view.zoom;
    for (index, port) in node.inputs.iter().enumerate() {
        let c = noodle::anchor(rect, Side::Input, index, node.inputs.len());
        let radius = t.port_radius.max(t.port_min_radius / view.zoom);
        ui.painter().circle(c, radius, t.card_fill, Stroke::new(t.port_stroke / view.zoom, t.wire(port.port_type.modality)));
        let r = ui.interact(Rect::from_center_size(c, Vec2::splat(radius + hit) * 2.0), ui.id().with(("in", &port.name)), Sense::drag());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("{} input", port.name)));
        ports.push((Side::Input, port.name.clone(), r.on_hover_text(&port.name)));
    }
    let radius = t.handle_radius.max(t.handle_min_screen_radius / view.zoom);
    for (index, port) in node.outputs.iter().enumerate() {
        let c = noodle::anchor(rect, Side::Output, index, node.outputs.len());
        ui.painter().circle(c, radius, t.handle_fill, Stroke::new(t.handle_stroke / view.zoom, t.wire(port.port_type.modality)));
        for axis in [Vec2::X, Vec2::Y] {
            let arm = axis * radius * 0.5;
            ui.painter().line_segment([c - arm, c + arm], Stroke::new(t.handle_stroke / view.zoom, t.handle_glyph));
        }
        let r = ui.interact(Rect::from_center_size(c, Vec2::splat(radius + hit) * 2.0), ui.id().with(("out", &port.name)), Sense::drag());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("{} output", port.name)));
        ports.push((Side::Output, port.name.clone(), r.on_hover_text(format!("Drag to connect {}", port.name))));
    }
    Some(CardResponse { response, intents, ports })
}

/// Several images in `area`, a row each (one per batch item, #519), every tile the same size;
/// a note says how many more the result holds than fit.
fn gallery(ui: &mut egui::Ui, area: Rect, rows: &[Vec<crate::preview::Preview>], count: usize, t: &Tokens) {
    let shown: usize = rows.iter().map(Vec::len).sum();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let gap = t.control_gap;
    // Bounded by the gallery's row and column caps, so the casts are exact.
    let tile = Vec2::new(
        ((area.width() - gap * (columns - 1) as f32) / columns as f32).max(1.0),
        ((area.height() - gap * (rows.len().max(1) - 1) as f32) / rows.len().max(1) as f32).max(1.0),
    );
    for (r, row) in rows.iter().enumerate() {
        for (c, image) in row.iter().enumerate() {
            let min = area.min + Vec2::new(c as f32 * (tile.x + gap), r as f32 * (tile.y + gap));
            show_image_with(ui, Rect::from_min_size(min, tile), image, None, (r, c), t);
        }
    }
    if count > shown {
        let more = format!("+{} more", count - shown);
        let at = area.right_bottom() - Vec2::splat(gap);
        let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
        let text = painter.layout_no_wrap(more.clone(), FontId::proportional(t.label_size), t.text);
        let backdrop = egui::Align2::RIGHT_BOTTOM.anchor_size(at, text.size()).expand(gap / 2.0);
        painter.rect_filled(backdrop, t.card_radius, t.card_fill);
        painter.galley(backdrop.min + Vec2::splat(gap / 2.0), text, t.text);
        let response = ui.interact(backdrop, ui.id().with("gallery_more"), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{} more images", count - shown)));
    }
}

/// An image preview fitted in `area`, keeping its aspect; or a placeholder saying why not.
pub(crate) fn show_image(ui: &mut egui::Ui, area: Rect, image: &crate::preview::Preview, grid: Option<(u32, u32, Option<[u32; 2]>)>, t: &Tokens) {
    show_image_with(ui, area, image, grid, (0, 0), t);
}

/// [`show_image`] for one of several images on a card, told apart by `tile` (row, column).
fn show_image_with(
    ui: &mut egui::Ui,
    area: Rect,
    image: &crate::preview::Preview,
    grid: Option<(u32, u32, Option<[u32; 2]>)>,
    tile: (usize, usize),
    t: &Tokens,
) {
    use crate::preview::Preview;
    let response = ui.interact(area, ui.id().with("image").with(tile), Sense::hover());
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    match image {
        Preview::Ready { texture, size, source_size } => {
            let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
            let scale = (area.width() / w).min(area.height() / h);
            let fitted = Rect::from_center_size(area.center(), Vec2::new(w * scale, h * scale));
            painter.image(*texture, fitted, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
            if let Some((rows, columns, full)) = grid {
                draw_grid(&painter, fitted, rows, columns, full.unwrap_or(*source_size), t);
                let label = format!("Image {}×{}, split preview {rows} rows by {columns} columns", source_size[0], source_size[1]);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, label.clone()));
            } else {
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, format!("Image {}×{}", source_size[0], source_size[1])));
            }
        }
        Preview::Pending | Preview::Unavailable(_) => {
            painter.rect_filled(area, t.card_radius, t.canvas_bg);
            let text = match image {
                Preview::Unavailable(why) => *why,
                _ => "Loading preview…",
            };
            painter.text(area.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(t.body_size), t.text_muted);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, text));
        }
    }
}

/// Where the split's cuts fall on `fitted`, the image drawn on screen: the same edges
/// `split_grid` cuts at, scaled from the image's `full` size.
pub fn grid_lines(fitted: Rect, rows: u32, columns: u32, full: [u32; 2]) -> (Vec<f32>, Vec<f32>) {
    let at = |length: u32, count: u32, index: u32, start: f32, span: f32| {
        start + span * nodecraft_engine::split::edge(length, count, index) as f32 / length.max(1) as f32
    };
    let xs = (1..columns).map(|i| at(full[0], columns, i, fitted.left(), fitted.width())).collect();
    let ys = (1..rows).map(|i| at(full[1], rows, i, fitted.top(), fitted.height())).collect();
    (xs, ys)
}

fn draw_grid(painter: &egui::Painter, fitted: Rect, rows: u32, columns: u32, full: [u32; 2], t: &Tokens) {
    let (xs, ys) = grid_lines(fitted, rows, columns, full);
    let stroke = Stroke::new(t.port_stroke, t.accent);
    for x in xs {
        painter.line_segment([Pos2::new(x, fitted.top()), Pos2::new(x, fitted.bottom())], stroke);
    }
    for y in ys {
        painter.line_segment([Pos2::new(fitted.left(), y), Pos2::new(fitted.right(), y)], stroke);
    }
    painter.rect_stroke(fitted, 0.0, stroke, StrokeKind::Inside);
}

fn control(
    ui: &mut egui::Ui,
    control: &ParamControl,
    value: Option<&Value>,
    name: &str,
    editor: &mut EditorState,
    label: egui::Id,
    t: &Tokens,
) -> Option<Option<Value>> {
    control_with_mode(ui, control, value, name, editor, label, t, false)
}

/// Bulk Mixed numeric values have no meaningful slider position: enter an exact value instead.
#[allow(clippy::too_many_arguments)]
pub(crate) fn control_with_mode(
    ui: &mut egui::Ui,
    control: &ParamControl,
    value: Option<&Value>,
    name: &str,
    editor: &mut EditorState,
    label: egui::Id,
    t: &Tokens,
    numeric_text: bool,
) -> Option<Option<Value>> {
    match control {
        ParamControl::Toggle => {
            let mut checked = matches!(value, Some(Value::Bool(true)));
            let response = ui.add(egui::Checkbox::without_text(&mut checked)).labelled_by(label);
            // An empty intrinsic checkbox label shadows its labelled-by relation.
            ui.ctx().accesskit_node_builder(response.id, |node| node.clear_label());
            if response.changed() {
                return Some(Some(Value::Bool(checked)));
            }
        }
        ParamControl::Choice { options } => {
            let current = match value {
                Some(Value::Text(s)) => s.as_str(),
                _ => "",
            };
            let mut selected = current.to_owned();
            let response = egui::ComboBox::from_id_salt("choice")
                .selected_text(options.iter().find(|o| o.value == current).map_or("Choose…", |o| o.label.as_str()))
                .show_ui(ui, |ui| {
                    for option in options {
                        ui.selectable_value(&mut selected, option.value.clone(), &option.label);
                    }
                })
                .response
                .labelled_by(label);
            if editor.draft_limit.is_some() {
                editor.choices.insert(response.id);
            }
            if selected != current {
                return Some(Some(Value::Text(selected)));
            }
        }
        ParamControl::Slider { min: Some(min), max: Some(max), step } if !numeric_text => {
            let original = match value {
                Some(Value::Number(n)) => n.get(),
                _ => min.get(),
            };
            if editor.sliders.get(name).is_some_and(|draft| draft.original != original) {
                editor.sliders.remove(name);
            }
            let mut number = editor.sliders.get(name).map_or(original, |draft| draft.value);
            let r = ui.add(egui::Slider::new(&mut number, min.get()..=max.get()).step_by(step.get()).update_while_editing(false)).labelled_by(label);
            if r.dragged() {
                editor.sliders.insert(name.to_owned(), SliderDraft { original, value: number });
            } else if r.changed() || r.drag_stopped() {
                editor.sliders.remove(name);
                if number != original
                    && let Ok(n) = Number::new(number)
                {
                    return Some(Some(Value::Number(n)));
                }
            }
        }
        ParamControl::Asset { modality } => {
            ui.label(match value {
                Some(Value::Media(asset)) => format!("{} asset", asset.mime.essence()),
                _ => format!(
                    "No {} selected",
                    match modality {
                        nodecraft_types::Modality::Text => "text",
                        nodecraft_types::Modality::Image => "image",
                        nodecraft_types::Modality::Video => "video",
                        nodecraft_types::Modality::Audio => "audio",
                        nodecraft_types::Modality::Mesh => "3D asset",
                        nodecraft_types::Modality::Document => "document",
                    }
                ),
            });
            if value.is_some() && ui.small_button("Remove").clicked() {
                return Some(None);
            }
        }
        _ => {
            let original: Cow<'_, str> = match value {
                Some(Value::Text(s)) => Cow::Borrowed(s),
                Some(Value::Integer(n)) => Cow::Owned(n.0.to_string()),
                Some(Value::Unsigned(n)) => Cow::Owned(n.0.to_string()),
                Some(Value::Number(n)) => Cow::Owned(n.get().to_string()),
                _ => Cow::Borrowed(""),
            };
            if !editor.text.contains_key(name) {
                let mut preview: String = original.chars().take(t.preview_chars).collect();
                if original.len() > preview.len() {
                    preview.push('…');
                }
                let response = ui
                    .add_sized(
                        Vec2::new(
                            ui.available_width(),
                            if matches!(control, ParamControl::Text { multiline: true, .. }) {
                                t.body_size * t.text_rows as f32
                            } else {
                                t.control_row_height / 2.0
                            },
                        ),
                        egui::Button::new(preview).frame(false).wrap(),
                    )
                    .labelled_by(label);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, ui.is_enabled(), ""));
                ui.ctx().accesskit_node_builder(response.id, |node| node.clear_label());
                if response.clicked() || response.has_focus() {
                    editor.text.insert(name.to_owned(), Draft { original: original.to_string(), text: original.to_string(), focus: true, was_focused: false });
                    ui.ctx().request_repaint();
                }
                return None;
            }
            let remaining_draft_chars = editor.draft_limit.map(|limit| {
                let others =
                    editor.text.iter().filter(|(field, _)| field.as_str() != name).map(|(_, draft)| draft.text.len()).fold(0usize, usize::saturating_add);
                limit.saturating_sub(others) / 4
            });
            let draft = editor.text.get_mut(name)?;
            if draft.original != original && draft.text == draft.original {
                *draft = Draft { original: original.to_string(), text: original.to_string(), focus: false, was_focused: false };
                if editor.error.as_ref().is_some_and(|(field, _)| *field == EditTarget::Parameter(name.to_owned())) {
                    editor.error = None;
                }
            }
            let multiline = matches!(control, ParamControl::Text { multiline: true, .. });
            let max = match control {
                ParamControl::Text { max_bytes, .. } => *max_bytes,
                ParamControl::Integer { .. } | ParamControl::Unsigned { .. } | ParamControl::Seed { .. } => 20,
                ParamControl::Slider { .. } => 32,
                ParamControl::Color => 9,
                _ => 512,
            };
            let char_limit = remaining_draft_chars.map_or(max, |limit| max.min(limit));
            let widget =
                if multiline { egui::TextEdit::multiline(&mut draft.text).desired_rows(t.text_rows) } else { egui::TextEdit::singleline(&mut draft.text) };
            let r = ui.add(widget.id_salt("value").desired_width(f32::INFINITY).char_limit(char_limit)).labelled_by(label);
            if editor.draft_limit.is_some() {
                editor.text_ids.insert(name.to_owned(), r.id);
            }
            if draft.focus {
                r.request_focus();
                if editor.draft_limit.is_some() {
                    editor.focus_request = Some(r.id);
                }
                draft.focus = false;
            }
            let owned_focus = r.has_focus() || r.lost_focus() || draft.was_focused;
            let cancel = owned_focus && ui.input(|i| i.key_pressed(egui::Key::Escape));
            let submit = owned_focus && ui.input(|i| i.key_pressed(egui::Key::Enter) && (!multiline || i.modifiers.command));
            draft.was_focused = r.has_focus();
            if cancel {
                draft.text = draft.original.clone();
                r.surrender_focus();
                if editor.error.as_ref().is_some_and(|(field, _)| *field == EditTarget::Parameter(name.to_owned())) {
                    editor.error = None;
                }
                editor.text.remove(name);
                editor.hidden_dirty = false;
            } else if (r.lost_focus() || submit) && draft.text != draft.original {
                let parsed = match control {
                    ParamControl::Integer { .. } => draft.text.parse::<i64>().ok().map(|n| Value::Integer(Integer(n))),
                    ParamControl::Unsigned { .. } | ParamControl::Seed { .. } => draft.text.parse::<u64>().ok().map(|n| Value::Unsigned(Unsigned(n))),
                    ParamControl::Slider { .. } => draft.text.parse::<f64>().ok().and_then(|n| Number::new(n).ok()).map(Value::Number),
                    _ => Some(Value::Text(draft.text.clone())),
                };
                if let Some(value) = parsed.filter(|v| control.validate_value(v).is_ok()) {
                    r.surrender_focus();
                    return Some(Some(value));
                }
                let message = if draft.text.len() > max {
                    format!("This value exceeds the {max}-byte limit.")
                } else {
                    "Enter a value within this parameter's allowed range and step.".into()
                };
                editor.failed(EditTarget::Parameter(name.to_owned()), message);
            } else if r.lost_focus() {
                editor.text.remove(name);
            }
        }
    }
    None
}

impl EditorState {
    pub(crate) fn field_owns_focus(&self, name: &str, id: egui::Id) -> bool {
        self.text_ids.get(name) == Some(&id)
    }
    pub(crate) fn owns_text_focus(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|memory| memory.focused()).is_some_and(|id| self.text_ids.values().any(|field| *field == id))
    }
    pub(crate) fn take_focus_request(&mut self) -> Option<egui::Id> {
        self.focus_request.take()
    }
    pub(crate) fn set_draft_limit(&mut self, limit: usize) {
        self.draft_limit = Some(limit);
    }
    pub(crate) fn owns_popup(&self, ctx: &egui::Context) -> bool {
        self.choices.iter().any(|id| egui::ComboBox::is_open(ctx, *id))
    }
    pub(crate) fn begin_empty(&mut self, name: &str) {
        self.text.insert(name.to_owned(), Draft { original: String::new(), text: String::new(), focus: true, was_focused: false });
    }
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_ref().map(|(_, message)| message.as_str())
    }
    /// Drafts remain recoverable view state while a model preview is open. Applying a model
    /// cannot silently discard them or copy them into a semantically different parameter.
    pub(crate) fn has_uncommitted(&self) -> bool {
        self.title.as_ref().is_some_and(|draft| draft.text != draft.original)
            || self.text.values().any(|draft| draft.text != draft.original)
            || !self.sliders.is_empty()
    }

    fn hide_fields(&mut self, visible: Option<&BTreeSet<String>>) {
        if self.text.iter().any(|(name, draft)| !visible.is_some_and(|fields| fields.contains(name)) && draft.text != draft.original) {
            self.hidden_dirty = true;
        }
        if self.error.as_ref().is_some_and(|(target, _)| matches!(target, EditTarget::Parameter(name) if !visible.is_some_and(|fields| fields.contains(name))))
        {
            self.error = None;
        }
    }

    pub(crate) fn mark_hidden(&mut self) {
        self.hidden_dirty = self.title.as_ref().is_some_and(|draft| draft.text != draft.original)
            || self.text.values().any(|draft| draft.text != draft.original)
            || !self.sliders.is_empty();
        for draft in self.text.values_mut().chain(self.title.iter_mut()) {
            draft.was_focused = false;
        }
    }
    #[cfg(test)]
    pub fn draft_count(&self) -> usize {
        self.text.len() + usize::from(self.title.is_some())
    }

    pub fn accepted(&mut self, target: &EditTarget) {
        match target {
            EditTarget::Title => self.title = None,
            EditTarget::Parameter(name) => {
                self.text.remove(name);
                self.sliders.remove(name);
            }
        }
        self.hidden_dirty = false;
        if self.error.as_ref().is_some_and(|(field, _)| field == target) {
            self.error = None;
        }
    }
    pub fn failed(&mut self, target: EditTarget, message: String) {
        self.error = Some((target, message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_lines_fall_where_the_split_cuts() {
        let fitted = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(100.0, 70.0));
        // A 10×7 image split 2 rows × 3 columns cuts at x = 3, 6 and y = 3 (as split_grid does).
        let (xs, ys) = grid_lines(fitted, 2, 3, [10, 7]);
        assert_eq!(xs, [40.0, 70.0]);
        assert_eq!(ys, [50.0]);
        let (none_x, none_y) = grid_lines(fitted, 1, 1, [10, 7]);
        assert!(none_x.is_empty() && none_y.is_empty(), "1×1 has no cuts");
        let (zero_x, _) = grid_lines(fitted, 1, 2, [0, 0]);
        assert!(zero_x.iter().all(|x| x.is_finite()), "a zero-size image never divides by zero");
    }
    #[test]
    fn text_sizes_are_whole_points_and_vanish_when_tiny() {
        assert_eq!(text_px(13.0, 1.07), Some(14.0));
        assert_eq!(text_px(13.0, 0.3), None);
        assert_eq!(text_px(13.0, f32::NAN), None);
    }
    #[test]
    fn titles_are_unicode_safe_and_explicit_titles_win() {
        let mut node = Node::new(nodecraft_types::NodeId::from_bytes([1; 16]), "source.text");
        node.params.insert("text".into(), Value::Text(format!("\n{}", "é".repeat(80))));
        assert_eq!(title(&node, None), format!("{}…", "é".repeat(64)));
        node.title = "My title".into();
        assert_eq!(title(&node, None), "My title");
    }
}

#[cfg(test)]
mod layout_metrics_tests {
    use super::*;
    #[test]
    fn minimum_zoom_outer_metrics_cover_painted_handle_stroke_and_clipped_notice() {
        let t = &crate::theme::DARK;
        let zoom = crate::canvas::MIN_ZOOM;
        let (_, top, right, bottom) = outer_insets(t, zoom, true);
        let painted = t.handle_radius.max(t.handle_min_screen_radius / zoom) + t.handle_stroke / (2.0 * zoom);
        assert_eq!(right, painted);
        assert_eq!(bottom, painted);
        assert!(top >= t.label_size.max(t.title_min_screen_px / zoom) + t.label_gap + t.control_gap);
        assert!(cull_margin(t, zoom) >= painted * zoom);
    }
}
