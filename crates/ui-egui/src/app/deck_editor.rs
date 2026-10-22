//! Editing a deck (#694): a full-window view of a Deck card with its slides down the left, as
//! thumbnails to pick, drag into order, add and remove, and the picked slide on the Layer Editor's
//! stage, layer list and fields. Every change is a `deck.*` command carrying the revision the
//! editor last listed, so an edit made against a deck that has changed since is refused rather
//! than undoing someone else's work. Layers this version can't read are kept where they are.
use super::NodeCraftApp;
use super::layer_editor::{self, Art, Commands, FrameParams, LayerEditor, Listing, Rendered, Stage, draw_layer, render_scale, text_field, vector_art};
use crate::theme::Tokens;
use egui::{Pos2, Rect, Sense, Vec2};
use nodecraft_actions::deck::{self, Drawable, Shown, Slide};
use nodecraft_actions::layers::{self, Content, Fonts, Frame};
use nodecraft_types::{AssetRef, CanvasId, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::HashMap;

pub(super) struct DeckEditor {
    pub(super) canvas: CanvasId,
    pub(super) node: NodeId,
    /// The picked slide's id, once the deck has one.
    slide: Option<String>,
    /// The picked slide's place, to pick its neighbour when it goes.
    index: usize,
    /// The picked slide's stage, layer list and fields.
    layers: Option<LayerEditor>,
    /// `deck.list` as of a document revision.
    listing: Option<(u64, Deck)>,
    /// Each slide thumbnail's rendered text and shape layers: by slide id, then layer id.
    thumbs: HashMap<String, HashMap<String, Rendered>>,
    /// The slide name being typed: (field key, text).
    name: Option<(String, String)>,
    /// A thumbnail being dragged: its index, and the gap it would drop into.
    pub(super) moving: Option<(String, usize)>,
}

impl DeckEditor {
    pub(super) fn new(canvas: CanvasId, node: NodeId) -> Self {
        Self { canvas, node, slide: None, index: 0, layers: None, listing: None, thumbs: HashMap::new(), name: None, moving: None }
    }

    /// The picked slide's id.
    #[cfg(test)]
    pub(super) fn slide(&self) -> Option<&str> {
        self.slide.as_deref()
    }

    /// The picked slide's stage, layer list and fields.
    #[cfg(test)]
    pub(super) fn layers(&self) -> Option<&LayerEditor> {
        self.layers.as_ref()
    }
}

/// A deck as `deck.list` returns it.
struct Deck {
    revision: u64,
    /// False for a deck from a newer NodeCraft, which lists with no slides.
    writable: bool,
    frame: Frame,
    background: String,
    slides: Vec<Entry>,
}

struct Entry {
    slide: Slide,
    /// Its layers as stored, bottom first, those this version can't read included.
    raw: Vec<Json>,
    sources: Vec<Source>,
    unsupported: usize,
}

#[derive(Deserialize)]
struct Listed {
    revision: u64,
    writable: bool,
    width: u32,
    height: u32,
    background: String,
    slides: Vec<Json>,
}

#[derive(Deserialize)]
struct Listed1 {
    #[serde(default)]
    layers: Vec<Json>,
    #[serde(default)]
    sources: Vec<Source>,
    #[serde(default)]
    unsupported: Vec<Json>,
}

#[derive(Deserialize)]
struct Source {
    origin: String,
    nth: u32,
    #[serde(default)]
    image: Option<AssetRef>,
    #[serde(default)]
    text: Option<String>,
}

fn listing(value: Json) -> Option<Deck> {
    let listed: Listed = serde_json::from_value(value).ok()?;
    let background = layers::parse_color(&listed.background).ok()?;
    let slides = listed
        .slides
        .into_iter()
        .filter_map(|raw| {
            let Listed1 { layers, sources, unsupported } = serde_json::from_value(raw.clone()).ok()?;
            Some(Entry { slide: serde_json::from_value(raw).ok()?, raw: layers, sources, unsupported: unsupported.len() })
        })
        .collect();
    Some(Deck {
        revision: listed.revision,
        writable: listed.writable,
        frame: Frame { width: listed.width, height: listed.height, background },
        background: listed.background,
        slides,
    })
}

/// What each layer of `entry` shows, as its sources say now.
fn drawn(entry: &Entry, frame: Frame) -> Drawable<AssetRef> {
    deck::drawable(&entry.slide, frame, |origin, nth| match entry.sources.iter().find(|s| s.origin == origin && s.nth == nth) {
        Some(Source { image: Some(image), .. }) => Shown::Image(image.clone()),
        Some(Source { text: Some(text), .. }) => Shown::Text(text.clone()),
        _ => Shown::Missing,
    })
}

/// A slide id the deck doesn't use: `slide-n`.
fn fresh_slide(deck: &Deck) -> String {
    let taken = |id: &str| deck.slides.iter().any(|e| e.slide.id == id);
    (1..=deck.slides.len().saturating_add(1)).map(|n| format!("slide-{n}")).find(|id| !taken(id)).unwrap_or_else(|| "slide".into())
}

/// The gap between thumbnails a pointer at `y` is nearest: 0 above the first, `count` below the
/// last.
fn gap_at(rows: &[Rect], y: f32) -> usize {
    rows.iter().position(|r| y < r.center().y).unwrap_or(rows.len())
}

impl NodeCraftApp {
    pub(super) fn open_deck_editor(&mut self, node: NodeId) {
        self.notice = None;
        self.selection.only(node);
        self.deck_editor = Some(DeckEditor::new(self.canvas, node));
    }

    /// Runs the editor's commands in order until one fails; each `deck.*` edit is checked
    /// against the revision before it, the listed one for the first.
    pub(super) fn deck_run(&mut self, listed: u64, commands: Commands) {
        let mut expected = listed;
        for (command, mut params) in commands {
            if command.starts_with("deck.")
                && let Some(params) = params.as_object_mut()
            {
                params.insert("expected_revision".into(), json!(expected));
            }
            match self.session.dispatch(command, &params) {
                Ok(_) => self.notice = None,
                Err(error) => {
                    // The engine's wording tells an agent to list again; here the next frame does.
                    let conflict = error.kind == nodecraft_types::ErrorKind::Conflict && command.starts_with("deck.");
                    self.notice = Some(if conflict { "The deck changed meanwhile; try again.".into() } else { error.message });
                    break;
                }
            }
            expected = self.session.revision();
        }
    }

    /// Each layer's art in `drawn`: image previews, and text and shapes rendered into `textures`.
    fn deck_art(
        &mut self,
        ctx: &egui::Context,
        drawn: &Drawable<AssetRef>,
        textures: &mut HashMap<String, Rendered>,
        scale: f64,
        dragged: &[String],
    ) -> HashMap<String, Art> {
        let edge = (ctx.content_rect().size().max_elem() * ctx.pixels_per_point()).min(2048.0);
        let render_at = render_scale(scale * f64::from(ctx.pixels_per_point()));
        let fonts = Fonts::default();
        let mut art = HashMap::new();
        for layer in &drawn.layers {
            let shown = match &layer.content {
                Content::Image { input, fit } => {
                    let image = drawn.images.get(*input as usize);
                    let texture = image.and_then(|asset| {
                        let assets = self.session.assets();
                        match self.previews.request_asset(ctx, asset.hash, edge, || assets.preview_read(asset)) {
                            crate::preview::Preview::Ready { texture, source_size, .. } => Some((texture, source_size)),
                            _ => None,
                        }
                    });
                    Art::Image { texture, fit: *fit }
                }
                Content::Source { fit, .. } => Art::Image { texture: None, fit: *fit },
                Content::Text { .. } | Content::Shape { .. } => vector_art(textures, ctx, layer, render_at, &fonts, 0, dragged),
            };
            art.insert(layer.id.clone(), shown);
        }
        textures.retain(|id, _| art.contains_key(id));
        art
    }

    /// The editor, while one is open. True when it owns the frame.
    pub(super) fn deck_editor_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut editor) = self.deck_editor.take() else {
            return false;
        };
        let node = (editor.canvas == self.canvas)
            .then(|| self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&editor.node)))
            .flatten()
            .filter(|n| n.kind == nodecraft_nodes::deck::ID)
            .cloned();
        let Some(node) = node else {
            // The deck went (undo, another canvas): nothing left to edit.
            return false;
        };
        let revision = self.session.revision();
        if editor.listing.as_ref().is_none_or(|(r, _)| *r != revision) {
            let listed = self.session.dispatch("deck.list", &json!({"canvas": editor.canvas, "id": editor.node})).map_err(|e| e.message);
            match listed.and_then(|v| listing(v).ok_or_else(|| "The deck's slides can't be read.".to_owned())) {
                Ok(deck) => editor.listing = Some((revision, deck)),
                Err(message) => {
                    self.notice = Some(message);
                    return false;
                }
            }
        }
        let Some((_, deck)) = editor.listing.take() else {
            return false;
        };

        // The picked slide: the same one, else its neighbour where it was, else none.
        let count = deck.slides.len();
        let index =
            editor.slide.as_ref().and_then(|id| deck.slides.iter().position(|e| &e.slide.id == id)).unwrap_or(editor.index.min(count.saturating_sub(1)));
        let current = deck.slides.get(index);
        editor.index = index;
        editor.slide = current.map(|e| e.slide.id.clone());
        if let Some(entry) = current
            && editor.layers.as_ref().is_none_or(|l| l.target != layer_editor::Target::Slide(entry.slide.id.clone()))
        {
            editor.layers = Some(LayerEditor::slide(editor.canvas, editor.node, entry.slide.id.clone()));
        } else if current.is_none() {
            editor.layers = None;
        }

        let ctx = ui.ctx().clone();
        let t = *self.theme.tokens();
        let choices = self.deck_choices(editor.node);
        let frame = deck.frame;
        let slide_listing = current.map(|e| Listing::slide(frame.width, frame.height, e.raw.clone(), choices));
        let mut layers = editor.layers.take();
        if let Some(ed) = &mut layers
            && ed.begin(&ctx)
        {
            editor.name = None;
        }

        // The stage's art, then every thumbnail's.
        let mut art = HashMap::new();
        if let (Some(ed), Some(entry), Some(listed)) = (&mut layers, current, &slide_listing) {
            let dragged = ed.dragged();
            let mut textures = std::mem::take(&mut ed.textures);
            art = self.deck_art(&ctx, &drawn(entry, frame), &mut textures, ed.scale, &dragged);
            ed.textures = textures;
            ed.keep(listed);
        }
        let side = t.layers_panel_width;
        let thumb_width = (side - t.control_gap * 4.0).max(1.0);
        let thumb_scale = f64::from(thumb_width) / f64::from(frame.width.max(1));
        let mut thumbs = Vec::with_capacity(count);
        let mut cache = std::mem::take(&mut editor.thumbs);
        cache.retain(|id, _| deck.slides.iter().any(|e| &e.slide.id == id));
        for entry in &deck.slides {
            let drawn = drawn(entry, frame);
            let textures = cache.entry(entry.slide.id.clone()).or_default();
            let art = self.deck_art(&ctx, &drawn, textures, thumb_scale, &[]);
            thumbs.push((drawn, art));
        }
        editor.thumbs = cache;

        let mut commands: Commands = Vec::new();
        let mut close = false;
        let mut present = false;
        let notice = self.notice.clone();
        let content = ctx.content_rect();
        let gap = t.control_gap * 2.0;
        let frame_params = FrameParams {
            width: frame.width,
            height: frame.height,
            background: frame.background,
            transparent: None,
            colour: Some(Value::Text(deck.background.clone())),
        };
        let shell = egui::Frame::NONE.fill(t.panel_fill).inner_margin(gap);
        egui::Modal::new(egui::Id::new("deck_editor")).frame(shell).backdrop_color(t.canvas_bg).show(&ctx, |ui| {
            let size = (content.size() - Vec2::splat(gap * 2.0)).max(Vec2::splat(1.0));
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.horizontal(|ui| {
                ui.heading("Slides");
                ui.label(egui::RichText::new(&node.title).color(t.text_muted));
                if let Some(entry) = current.filter(|e| e.unsupported > 0) {
                    let what = if entry.unsupported == 1 {
                        "1 layer here needs a newer NodeCraft; it stays as it is.".to_owned()
                    } else {
                        format!("{} layers here need a newer NodeCraft; they stay as they are.", entry.unsupported)
                    };
                    ui.label(egui::RichText::new(what).color(t.stale));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close |= ui.button("Done").clicked();
                    present |= ui.add_enabled(count > 0, egui::Button::new("Present")).clicked();
                    if ui.button("Redo").clicked() {
                        commands.push(("edit.redo", json!({})));
                    }
                    if ui.button("Undo").clicked() {
                        commands.push(("edit.undo", json!({})));
                    }
                    if let Some(notice) = &notice {
                        ui.label(egui::RichText::new(notice).color(t.danger));
                    }
                });
            });
            let body = ui.available_rect_before_wrap();
            if !deck.writable {
                ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                    ui.label(egui::RichText::new("This deck was made by a newer NodeCraft and can't be edited here.").color(t.text_muted));
                });
                ui.allocate_rect(body, Sense::hover());
                return;
            }
            let side = side.min(body.width() / 5.0);
            let sorter_rect = Rect::from_min_size(body.min, Vec2::new(side, body.height()));
            let left = Rect::from_min_size(Pos2::new(sorter_rect.max.x + gap, body.min.y), Vec2::new(side, body.height()));
            let right = Rect::from_min_max(Pos2::new(body.max.x - side, body.min.y), body.max);
            let centre = Rect::from_min_max(Pos2::new(left.max.x + gap, body.min.y), Pos2::new(right.min.x - gap, body.max.y));
            ui.scope_builder(egui::UiBuilder::new().max_rect(sorter_rect), |ui| sorter(ui, &mut editor, &deck, &thumbs, &t, &mut commands));
            match (&mut layers, &slide_listing) {
                (Some(ed), Some(listed)) => {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| layer_editor::layers_panel(ui, ed, listed, &t, &mut commands));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("deck_properties")
                            .show(ui, |ui| layer_editor::properties(ui, ed, listed, &frame_params, &t, &mut commands));
                    });
                    ui.scope_builder(egui::UiBuilder::new().max_rect(centre), |ui| layer_editor::stage(ui, ed, listed, &art, &frame_params, &t, &mut commands));
                }
                _ => {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(centre), |ui| {
                        ui.label(egui::RichText::new("This deck has no slides yet. Add one to start.").color(t.text_muted));
                    });
                }
            }
            ui.allocate_rect(body, Sense::hover());
        });
        if let (Some(ed), Some(listed)) = (&mut layers, &slide_listing) {
            close |= layer_editor::keys(ui, ed, listed, &mut commands);
            ed.end(&ctx);
        } else if !crate::canvas::text_input_focused(&ctx) {
            // No slide to edit: Escape closes, and undo still brings a removed slide back.
            ctx.input_mut(|i| {
                for (id, shortcut) in [("edit.redo", "Command+Shift+Z"), ("edit.redo", "Command+Y"), ("edit.undo", "Command+Z")] {
                    if super::consume_shortcut(i, shortcut) {
                        commands.push((id, json!({})));
                    }
                }
                close |= i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            });
        }
        editor.layers = layers;
        let listed = deck.revision;
        editor.listing = Some((revision, deck));
        self.deck_run(listed, commands);
        if present {
            let at = editor.index;
            self.open_presenter(editor.node);
            if let Some(presenter) = &mut self.presenter {
                presenter.index = at;
            }
            return true;
        }
        if !close {
            self.deck_editor = Some(editor);
        }
        true
    }

    /// The cards a new source layer can show: every other card on the canvas but decks, by title.
    fn deck_choices(&self, deck: NodeId) -> Vec<(String, String)> {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return Vec::new() };
        let mut out: Vec<(String, String)> = canvas
            .nodes()
            .values()
            .filter(|n| n.id != deck && n.kind != nodecraft_nodes::deck::ID)
            .map(|n| (n.id.to_string(), if n.title.trim().is_empty() { n.kind.clone() } else { n.title.clone() }))
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// The slides as thumbnails, top to bottom: click to pick, drag to reorder; with ways to add,
/// remove, move and name them.
fn sorter(ui: &mut egui::Ui, editor: &mut DeckEditor, deck: &Deck, thumbs: &[(Drawable<AssetRef>, HashMap<String, Art>)], t: &Tokens, commands: &mut Commands) {
    let count = deck.slides.len();
    let current = editor.slide.clone();
    let on = |slide: &str| json!({"canvas": editor.canvas, "id": editor.node, "slide": slide});
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(count < deck::MAX_SLIDES, egui::Button::new("Add slide")).clicked() {
            let id = fresh_slide(deck);
            let at = if count == 0 { 0 } else { editor.index.saturating_add(1) };
            commands.push(("deck.slide_add", json!({"canvas": editor.canvas, "id": editor.node, "slide": {"id": id, "name": "", "layers": []}, "index": at})));
            editor.slide = Some(id);
            editor.index = at;
        }
        if let Some(slide) = &current {
            if ui.button("Remove").on_hover_text("Undo brings it back").clicked() {
                commands.push(("deck.slide_remove", on(slide)));
            }
            for (label, to, enabled) in
                [("Up", editor.index.saturating_sub(1), editor.index > 0), ("Down", editor.index.saturating_add(1), editor.index.saturating_add(1) < count)]
            {
                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                    let mut params = on(slide);
                    params["index"] = json!(to);
                    commands.push(("deck.slide_move", params));
                }
            }
        }
    });
    if let Some(entry) = deck.slides.get(editor.index)
        && let Some(name) = text_field(ui, &mut editor.name, &format!("{}.slide-name", entry.slide.id), "Name", &entry.slide.name, false)
    {
        let mut params = on(&entry.slide.id);
        params["name"] = json!(name.trim());
        commands.push(("deck.slide_update", params));
    }
    ui.separator();
    let frame = deck.frame;
    let width = ui.available_width().max(1.0);
    let scale = f64::from(width) / f64::from(frame.width.max(1));
    let thumb = Vec2::new(width, (f64::from(frame.height) * scale) as f32);
    let label = t.body_size * 1.5;
    let mut rows = Vec::with_capacity(count);
    let mut dropped = None;
    egui::ScrollArea::vertical().id_salt("deck_sorter").show(ui, |ui| {
        for (n, (entry, (drawn, art))) in deck.slides.iter().zip(thumbs).enumerate() {
            // Keyed by slide, so a drag stays with its slide when the slides change under it.
            let (rect, _) = ui.allocate_exact_size(thumb + Vec2::new(0.0, label), Sense::hover());
            let response = ui.interact(rect, egui::Id::new(("deck_thumb", &entry.slide.id)), Sense::click_and_drag());
            let title =
                if entry.slide.name.is_empty() { format!("Slide {}", n.saturating_add(1)) } else { format!("{} · {}", n.saturating_add(1), entry.slide.name) };
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, current.as_deref() == Some(entry.slide.id.as_str()), title.clone())
            });
            rows.push(rect);
            let picture = Rect::from_min_size(rect.min, thumb);
            let painter = ui.painter_at(picture);
            let [r, g, b, a] = frame.background;
            painter.rect_filled(picture, 0.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a));
            let stage = Stage { origin: picture.min, scale };
            for layer in &drawn.layers {
                draw_layer(&painter, stage, layer, art.get(&layer.id), t);
            }
            let picked = current.as_deref() == Some(entry.slide.id.as_str());
            let outline = if picked { egui::Stroke::new(t.handle_stroke, t.accent) } else { egui::Stroke::new(1.0, t.panel_stroke) };
            ui.painter().rect_stroke(picture, 0.0, outline, egui::StrokeKind::Inside);
            ui.painter().text(
                Pos2::new(rect.min.x, picture.max.y + label / 2.0),
                egui::Align2::LEFT_CENTER,
                &title,
                egui::FontId::proportional(t.body_size),
                if picked { t.text } else { t.text_muted },
            );
            if response.clicked() {
                editor.slide = Some(entry.slide.id.clone());
                editor.index = n;
            }
            if response.drag_started() {
                editor.moving = Some((entry.slide.id.clone(), n));
            }
            if response.drag_stopped() {
                dropped = editor.moving.take();
            }
            ui.add_space(t.control_gap);
        }
        // While dragging, where the slide would go; a release outside the sorter cancels.
        let inside = ui.ctx().pointer_interact_pos().filter(|p| ui.clip_rect().contains(*p));
        if dropped.is_some() && inside.is_none() {
            dropped = None;
        }
        if editor.moving.is_some() && !ui.ctx().input(|i| i.pointer.any_down()) {
            // The slide went mid-drag, so its release never came.
            editor.moving = None;
        }
        if let Some((slide, _)) = editor.moving.take() {
            let to = inside.map(|p| gap_at(&rows, p.y));
            if let Some(to) = to {
                let y = rows.get(to).map_or_else(|| rows.last().map_or(0.0, |r| r.max.y + t.control_gap / 2.0), |r| r.min.y - t.control_gap / 2.0);
                ui.painter().hline(ui.max_rect().x_range(), y, egui::Stroke::new(t.handle_stroke, t.accent));
            }
            editor.moving = Some((slide, to.unwrap_or(usize::MAX)));
        }
    });
    if let Some((slide, gap)) = dropped
        && let Some(from) = deck.slides.iter().position(|e| e.slide.id == slide)
        && let Some(entry) = deck.slides.get(from)
        && gap != usize::MAX
    {
        // A gap below the slide's own place moves it to one less, once it's out of the way.
        let to = if gap > from { gap.saturating_sub(1) } else { gap };
        if to != from {
            let mut params = on(&entry.slide.id);
            params["index"] = json!(to);
            commands.push(("deck.slide_move", params));
            editor.slide = Some(entry.slide.id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_drops_into_the_nearest_gap() {
        let rows: Vec<Rect> = (0..3).map(|n| Rect::from_min_size(Pos2::new(0.0, n as f32 * 100.0), Vec2::new(50.0, 90.0))).collect();
        assert_eq!(gap_at(&rows, -10.0), 0);
        assert_eq!(gap_at(&rows, 40.0), 0);
        assert_eq!(gap_at(&rows, 60.0), 1);
        assert_eq!(gap_at(&rows, 260.0), 3);
    }
}
