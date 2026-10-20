//! The mask editor (#102): paint where an image should change, widen its canvas for
//! outpainting, and wire the edit card. It edits the Mask and Expand cards `mask.create` adds,
//! only through commands: each finished stroke is one `node.mask_set`, so one undo step.
use super::NodeCraftApp;
use crate::{card, ids, theme::Tokens};
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use nodecraft_actions::image::{ImageLimits, Raster};
use nodecraft_actions::mask::{self, MaskOptions, Stroke, Strokes};
use nodecraft_engine::mask_cmds;
use nodecraft_graph::{Canvas, Endpoint, Node};
use nodecraft_types::{AssetRef, CanvasId, Modality, NodeId, Unsigned, Value};
use serde_json::json;

const SIDES: [&str; 4] = ["left", "top", "right", "bottom"];
const SIDE_LABELS: [&str; 4] = ["Left", "Top", "Right", "Bottom"];
/// Longest side of the overlay raster, in pixels: enough to see, cheap to redraw per stroke.
const OVERLAY_EDGE: u32 = 384;
/// Brush sizes offered, as a fraction of the longer side.
const BRUSH: std::ops::RangeInclusive<f64> = 0.005..=0.5;
/// How far right of a card the cards it adds go, in world points.
const BESIDE: f32 = 360.0;

pub(super) struct MaskEditor {
    canvas: CanvasId,
    pub(super) mask: NodeId,
    erase: bool,
    size: f64,
    hardness: f64,
    /// The stroke being drawn; stored when the pointer is released.
    stroke: Option<Stroke>,
    /// Margins being typed or dragged, applied when the control lets go.
    margins: Option<[u32; 4]>,
    pub(super) prompt: String,
    model: Option<String>,
    pub(super) overlay: Option<(OverlayKey, egui::TextureHandle)>,
}

impl MaskEditor {
    pub(super) fn new(canvas: CanvasId, mask: NodeId) -> Self {
        Self { canvas, mask, erase: false, size: 0.08, hardness: 0.7, stroke: None, margins: None, prompt: String::new(), model: None, overlay: None }
    }
}

#[derive(PartialEq)]
pub(super) struct OverlayKey {
    strokes: String,
    options: MaskOptions,
    size: [u32; 2],
    image: [u32; 4],
    tint: Color32,
}

/// What the editor works on, read from the document each frame.
struct Target {
    strokes: Strokes,
    options: MaskOptions,
    /// The Expand card feeding the mask, if any, and its margins (left, top, right, bottom).
    expand: Option<NodeId>,
    margins: [u32; 4],
    /// The image under the mask (before expansion), once it exists.
    image: Option<AssetRef>,
    pos: nodecraft_graph::Position,
}

fn flag(node: &Node, name: &str) -> bool {
    matches!(node.params.get(name), Some(Value::Bool(true)))
}

fn input<'a>(canvas: &'a Canvas, node: NodeId, port: &str) -> Option<&'a Node> {
    let edge = canvas.edges().values().find(|e| e.to.node == node && e.to.port == port)?;
    canvas.nodes().get(&edge.from.node)
}

fn target(canvas: &Canvas, id: NodeId) -> Option<Target> {
    let node = canvas.nodes().get(&id).filter(|n| n.kind == nodecraft_nodes::mask::ID)?;
    let strokes = mask_cmds::strokes(node).ok()?;
    let options = MaskOptions { transparent: flag(node, "transparent"), invert: flag(node, "invert") };
    let upstream = input(canvas, id, "image");
    let (expand, margins, image) = match upstream {
        Some(n) if n.kind == mask_cmds::EXPAND => {
            let margins = SIDES.map(|side| match n.params.get(side) {
                Some(Value::Unsigned(v)) => u32::try_from(v.0).unwrap_or(mask::MAX_MARGIN).min(mask::MAX_MARGIN),
                _ => 0,
            });
            (Some(n.id), margins, input(canvas, n.id, "image").and_then(nodecraft_engine::split::image_of).cloned())
        }
        Some(n) => (None, [0; 4], nodecraft_engine::split::image_of(n).cloned()),
        None => (None, [0; 4], None),
    };
    Some(Target { strokes, options, expand, margins, image, pos: node.pos })
}

/// The first output of `node` that carries one image, to paint a mask over.
pub(super) fn image_output(node: &Node) -> Option<&str> {
    node.outputs.iter().find(|p| p.port_type.modality == Modality::Image && !p.port_type.many).map(|p| p.name.as_str())
}

/// The mask over a downscaled frame, tinted for drawing over the image. Margins (outside
/// `image`) are transparent, as the expanded canvas will be.
fn overlay_image(key: &OverlayKey, strokes: &Strokes) -> Option<egui::ColorImage> {
    let [w, h] = key.size;
    let [x0, y0, x1, y1] = key.image;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact((w as usize).checked_mul(h as usize)?.checked_mul(4)?).ok()?;
    for y in 0..h {
        for x in 0..w {
            let inside = (x0..x1).contains(&x) && (y0..y1).contains(&y);
            pixels.extend_from_slice(&[0, 0, 0, if inside { 255 } else { 0 }]);
        }
    }
    let limits = ImageLimits::default();
    let frame = Raster::new(w, h, pixels, limits).ok()?;
    let rendered = mask::render(strokes, key.options, &frame, limits).ok()?;
    let [r, g, b, a] = key.tint.to_srgba_unmultiplied();
    let mut tinted = Vec::new();
    tinted.try_reserve_exact(rendered.pixels().len()).ok()?;
    for p in rendered.pixels().as_chunks::<4>().0 {
        let alpha = (u16::from(a) * u16::from(p[0]) / 255) as u8;
        tinted.extend_from_slice(&[r, g, b, alpha]);
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &tinted))
}

fn param_json(value: Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

impl NodeCraftApp {
    /// Adds an Expand and a Mask card behind `node`'s image and opens the editor on the mask.
    pub(super) fn open_mask_editor_for(&mut self, node: NodeId) {
        let document = self.session.document();
        let Some(source) = document.canvases().get(&self.canvas).and_then(|c| c.nodes().get(&node)) else {
            return;
        };
        let Some(port) = image_output(source) else {
            self.notice = Some("This card has no single image to paint over.".into());
            return;
        };
        let revision = self.session.revision();
        let (Some(expand), Some(mask), Some(first), Some(second)) = (
            ids::node_id(document, ("mask-expand", node, revision)),
            ids::node_id(document, ("mask", node, revision)),
            ids::edge_id(document, ("mask-edge", node, revision, 0)),
            ids::edge_id(document, ("mask-edge", node, revision, 1)),
        ) else {
            self.notice = Some("Couldn't allocate node ids; try again.".into());
            return;
        };
        let pos = json!({"x": source.pos.x + BESIDE, "y": source.pos.y});
        let params =
            json!({"canvas": self.canvas, "from": Endpoint { node, port: port.into() }, "expand": expand, "mask": mask, "edges": [first, second], "pos": pos});
        match self.session.dispatch("mask.create", &params) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(mask);
                self.mask_editor = Some(MaskEditor::new(self.canvas, mask));
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    fn mask_dispatch(&mut self, command: &str, params: &serde_json::Value) -> bool {
        match self.session.dispatch(command, params) {
            Ok(_) => {
                self.notice = None;
                true
            }
            Err(error) => {
                self.notice = Some(error.message);
                false
            }
        }
    }

    /// The editor, while one is open. True when it owns the frame.
    pub(super) fn mask_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut editor) = self.mask_editor.take() else {
            return false;
        };
        let target =
            (editor.canvas == self.canvas).then(|| self.session.document().canvases().get(&self.canvas).and_then(|c| target(c, editor.mask))).flatten();
        let Some(target) = target else {
            // The mask went (undo, another canvas): nothing left to edit.
            return false;
        };
        let t = *self.theme.tokens();
        let ctx = ui.ctx().clone();
        let image_size = target.image.as_ref().and_then(|a| Some([a.meta.width?, a.meta.height?])).filter(|[w, h]| *w > 0 && *h > 0);
        let preview = target.image.as_ref().map(|asset| {
            let assets = self.session.assets();
            let edge = t.viewer_width * ctx.pixels_per_point();
            self.previews.request_asset(&ctx, asset.hash, edge, || assets.preview_read(asset))
        });
        let models = self.session.generation_catalog().map(mask_cmds::inpaint_models).unwrap_or_default();
        let mut commands: Vec<(&'static str, serde_json::Value)> = Vec::new();
        let mut close = false;
        let mut create = false;
        let canvas = self.canvas;
        let id = editor.mask;
        let modal = egui::Modal::new(egui::Id::new("mask_editor")).show(&ctx, |ui| {
            ui.set_width(t.viewer_width);
            ui.heading("Mask");
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut editor.erase, false, "Brush");
                ui.selectable_value(&mut editor.erase, true, "Eraser");
                ui.add(egui::Slider::new(&mut editor.size, BRUSH).logarithmic(true).text("Size"));
                ui.add(egui::Slider::new(&mut editor.hardness, 0.0..=1.0).text("Hardness"));
                if ui.add_enabled(!target.strokes.strokes.is_empty(), egui::Button::new("Undo stroke")).clicked() {
                    let mut fewer = target.strokes.strokes.clone();
                    fewer.pop();
                    commands.push(("node.mask_set", json!({"canvas": canvas, "id": id, "strokes": fewer})));
                }
                if ui.add_enabled(!target.strokes.strokes.is_empty(), egui::Button::new("Clear")).clicked() {
                    commands.push(("node.mask_set", json!({"canvas": canvas, "id": id, "strokes": []})));
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (name, label, on) in [("transparent", "Include transparent areas", target.options.transparent), ("invert", "Invert", target.options.invert)]
                {
                    let mut value = on;
                    if ui.checkbox(&mut value, label).changed() {
                        commands.push(("node.param_set", json!({"canvas": canvas, "id": id, "name": name, "value": param_json(Value::Bool(value))})));
                    }
                }
            });
            if let Some(expand) = target.expand {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Expand canvas").color(t.text_muted));
                    // Held while a control is in use; the document's margins otherwise.
                    let mut margins = editor.margins.unwrap_or(target.margins);
                    let (mut apply, mut held) = (false, false);
                    for (side, value) in SIDE_LABELS.iter().zip(margins.iter_mut()) {
                        let label = ui.label(*side);
                        let r = ui.add(egui::DragValue::new(value).range(0..=mask::MAX_MARGIN).speed(4.0).suffix(" px")).labelled_by(label.id);
                        apply |= r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
                        held |= r.dragged() || r.has_focus();
                    }
                    if apply {
                        for ((side, value), was) in SIDES.iter().zip(margins).zip(target.margins) {
                            if value != was {
                                let value = param_json(Value::Unsigned(Unsigned(u64::from(value))));
                                commands.push(("node.param_set", json!({"canvas": canvas, "id": expand, "name": side, "value": value})));
                            }
                        }
                    }
                    editor.margins = (held && !apply).then_some(margins);
                });
            }
            let shown = editor.margins.unwrap_or(target.margins);
            match image_size {
                Some(size) => paint_area(ui, &mut editor, &target, size, shown, preview.as_ref(), &t, &mut commands),
                None => {
                    ui.label("Run the image card first: the mask is painted over its result.");
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut editor.prompt).hint_text("Describe the change").desired_width(t.viewer_width * 0.5));
                let chosen = editor.model.clone().filter(|m| models.contains(m)).or_else(|| models.first().cloned());
                let label = ui.label(egui::RichText::new("Model").color(t.text_muted));
                egui::ComboBox::from_id_salt("mask_model")
                    .selected_text(chosen.as_deref().unwrap_or("No model takes a mask"))
                    .show_ui(ui, |ui| {
                        for model in &models {
                            if ui.selectable_label(chosen.as_ref() == Some(model), model).clicked() {
                                editor.model = Some(model.clone());
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
                let ready = !editor.prompt.trim().is_empty() && chosen.is_some();
                create = ui.add_enabled(ready, egui::Button::new("Create edit")).on_hover_text("Add a generation card that redraws the masked area").clicked();
                close |= ui.button("Close").clicked();
            });
        });
        close |= modal.should_close();
        for (command, params) in commands {
            if !self.mask_dispatch(command, &params) {
                break;
            }
        }
        if create && self.create_mask_edit(&editor, &models, target.pos) {
            close = true;
        }
        if !close {
            self.mask_editor = Some(editor);
        }
        true
    }

    fn create_mask_edit(&mut self, editor: &MaskEditor, models: &[String], at: nodecraft_graph::Position) -> bool {
        let document = self.session.document();
        let revision = self.session.revision();
        let (Some(id), Some(first), Some(second)) = (
            ids::node_id(document, ("mask-edit", editor.mask, revision)),
            ids::edge_id(document, ("mask-edit-edge", editor.mask, revision, 0)),
            ids::edge_id(document, ("mask-edit-edge", editor.mask, revision, 1)),
        ) else {
            self.notice = Some("Couldn't allocate node ids; try again.".into());
            return false;
        };
        let model = editor.model.clone().filter(|m| models.contains(m)).or_else(|| models.first().cloned());
        let params = json!({"canvas": editor.canvas, "mask": editor.mask, "id": id, "edges": [first, second], "pos": {"x": at.x + BESIDE, "y": at.y}, "prompt": editor.prompt.trim(), "model": model});
        let created = self.mask_dispatch("mask.edit", &params);
        if created {
            self.selection.only(id);
        }
        created
    }
}

/// The image in its expanded frame, the mask over it, and the brush. A finished stroke queues
/// one `node.mask_set` with every stroke.
#[allow(clippy::too_many_arguments)]
fn paint_area(
    ui: &mut egui::Ui,
    editor: &mut MaskEditor,
    target: &Target,
    [w, h]: [u32; 2],
    [left, top, right, bottom]: [u32; 4],
    preview: Option<&crate::preview::Preview>,
    t: &Tokens,
    commands: &mut Vec<(&'static str, serde_json::Value)>,
) {
    let frame_w = f64::from(w) + f64::from(left) + f64::from(right);
    let frame_h = f64::from(h) + f64::from(top) + f64::from(bottom);
    let width = ui.available_width();
    let tallest = (ui.ctx().content_rect().height() * 0.6).max(1.0);
    let scale = (f64::from(width) / frame_w).min(f64::from(tallest) / frame_h);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let size = Vec2::new((frame_w * scale) as f32, (frame_h * scale) as f32);
    let (area, _) = ui.allocate_exact_size(Vec2::new(width, size.y), Sense::hover());
    let frame = Rect::from_center_size(area.center(), size);
    let at = |px: f64, py: f64| Pos2::new(frame.min.x + (px * scale) as f32, frame.min.y + (py * scale) as f32);
    let image_rect = Rect::from_min_max(at(f64::from(left), f64::from(top)), at(f64::from(left) + f64::from(w), f64::from(top) + f64::from(h)));
    ui.painter().rect_filled(frame, 0.0, t.canvas_bg);
    ui.painter().rect_stroke(frame, 0.0, egui::Stroke::new(t.handle_stroke, t.panel_stroke), egui::StrokeKind::Outside);
    if let Some(preview) = preview {
        card::show_image(ui, image_rect, preview, None, t);
    }

    // The stored strokes, rasterized as the action renders them.
    let shrink = (f64::from(OVERLAY_EDGE) / frame_w.max(frame_h)).min(1.0);
    let px = |v: f64| ((v * shrink).round() as u32).max(1);
    let key = OverlayKey {
        strokes: target.strokes.encode().unwrap_or_default(),
        options: target.options,
        size: [px(frame_w), px(frame_h)],
        image: [f64::from(left), f64::from(top), f64::from(left) + f64::from(w), f64::from(top) + f64::from(h)].map(|v| (v * shrink).round() as u32),
        tint: t.mask_overlay,
    };
    if editor.overlay.as_ref().is_none_or(|(k, _)| *k != key) {
        editor.overlay = overlay_image(&key, &target.strokes).map(|image| (key, ui.ctx().load_texture("mask_overlay", image, egui::TextureOptions::LINEAR)));
    }
    if let Some((_, texture)) = &editor.overlay {
        ui.painter().image(texture.id(), frame, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }

    let response = ui.interact(frame, ui.id().with("mask_paint"), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Mask painting area"));
    let radius = (editor.size * frame_w.max(frame_h) / 2.0 * scale) as f32;
    let normal =
        |p: Pos2| [(f64::from(p.x - frame.min.x) / (frame_w * scale)).clamp(-0.5, 1.5), (f64::from(p.y - frame.min.y) / (frame_h * scale)).clamp(-0.5, 1.5)];
    if response.is_pointer_button_down_on()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let point = normal(pointer);
        let used: usize = target.strokes.strokes.iter().map(|s| s.points.len()).sum();
        let stroke = editor.stroke.get_or_insert_with(|| Stroke { size: editor.size, hardness: editor.hardness, erase: editor.erase, points: Vec::new() });
        let spacing = f64::from((radius / 4.0).max(1.0));
        let far = stroke.points.last().is_none_or(|last| {
            let (dx, dy) = ((point[0] - last[0]) * frame_w * scale, (point[1] - last[1]) * frame_h * scale);
            dx.hypot(dy) >= spacing
        });
        if far && used.saturating_add(stroke.points.len()) < mask::MAX_POINTS {
            stroke.points.push(point);
        }
    } else if let Some(stroke) = editor.stroke.take()
        && !stroke.points.is_empty()
        && target.strokes.strokes.len() < mask::MAX_STROKES
    {
        let mut strokes = target.strokes.strokes.clone();
        strokes.push(stroke);
        commands.push(("node.mask_set", json!({"canvas": editor.canvas, "id": editor.mask, "strokes": strokes})));
    }

    // The stroke in progress, and the brush under the pointer.
    let painter = ui.painter().with_clip_rect(frame);
    if let Some(stroke) = &editor.stroke {
        let colour = if stroke.erase { t.canvas_bg.gamma_multiply(0.6) } else { t.mask_overlay };
        let points: Vec<Pos2> = stroke.points.iter().map(|p| at(p[0] * frame_w, p[1] * frame_h)).collect();
        for pair in points.windows(2) {
            if let [a, b] = pair {
                painter.line_segment([*a, *b], egui::Stroke::new(radius * 2.0, colour));
            }
        }
        for p in &points {
            painter.circle_filled(*p, radius, colour);
        }
    }
    if let Some(hover) = response.hover_pos() {
        let ring = if editor.erase { t.text_muted } else { t.accent };
        painter.circle_stroke(hover, radius, egui::Stroke::new(t.handle_stroke, ring));
    }
}
