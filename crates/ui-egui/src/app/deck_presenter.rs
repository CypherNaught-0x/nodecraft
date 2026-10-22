//! Presenting a deck (#122): a full-window view of a Deck card's slides, one at a time. The arrow
//! keys, Space, Page Up/Down, Home and End move through them, a click goes on, and Escape leaves.
//! Slides draw with the Layer Editor's on-screen drawing, so a slide looks the same in both; each
//! source layer shows what its node shows now, and one whose node is gone or has nothing yet is
//! an empty box. Presenting a selection first makes a Deck card from it (`deck.create`), so what
//! was presented can be kept, reordered and shown again.
use super::NodeCraftApp;
use super::layer_editor::{Art, Rendered, Stage, draw_layer, render, render_key, render_scale};
use crate::ids;
use egui::{Pos2, Rect, Sense, Vec2};
use nodecraft_actions::deck::{self, Shown, Slide};
use nodecraft_actions::layers::{self, Content, Fonts, Frame};
use nodecraft_types::{AssetRef, CanvasId, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::HashMap;

/// How far right of the selection a new Deck card goes.
const GAP: f32 = 360.0;

pub(super) struct Presenter {
    pub(super) canvas: CanvasId,
    pub(super) node: NodeId,
    /// The slide shown, from 0.
    pub(super) index: usize,
    listing: Option<(u64, Listing)>,
    /// The current slide's rendered text and shape layers, by layer id.
    textures: HashMap<String, Rendered>,
}

impl Presenter {
    pub(super) fn new(canvas: CanvasId, node: NodeId) -> Self {
        Self { canvas, node, index: 0, listing: None, textures: HashMap::new() }
    }
}

/// A deck as `deck.list` returns it.
struct Listing {
    frame: Frame,
    slides: Vec<(Slide, Vec<Source>)>,
}

#[derive(Deserialize)]
struct Listed {
    width: u32,
    height: u32,
    background: String,
    slides: Vec<Json>,
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

#[derive(Deserialize)]
struct Sources {
    #[serde(default)]
    sources: Vec<Source>,
}

fn listing(value: Json) -> Option<Listing> {
    let listed: Listed = serde_json::from_value(value).ok()?;
    let background = layers::parse_color(&listed.background).ok()?;
    let slides = listed
        .slides
        .into_iter()
        .filter_map(|raw| {
            let sources = serde_json::from_value::<Sources>(raw.clone()).ok()?.sources;
            Some((serde_json::from_value::<Slide>(raw).ok()?, sources))
        })
        .collect();
    Some(Listing { frame: Frame { width: listed.width, height: listed.height, background }, slides })
}

/// Where a key moves the presenter: to a slide, or out.
enum Step {
    To(usize),
    Leave,
}

fn step(ctx: &egui::Context, index: usize, count: usize) -> Option<Step> {
    let last = count.saturating_sub(1);
    ctx.input_mut(|i| {
        let mut pressed = |keys: &[egui::Key]| keys.iter().any(|key| i.consume_key(egui::Modifiers::NONE, *key));
        if pressed(&[egui::Key::Escape]) {
            Some(Step::Leave)
        } else if pressed(&[egui::Key::ArrowRight, egui::Key::ArrowDown, egui::Key::Space, egui::Key::PageDown]) {
            Some(Step::To(index.saturating_add(1).min(last)))
        } else if pressed(&[egui::Key::ArrowLeft, egui::Key::ArrowUp, egui::Key::PageUp]) {
            Some(Step::To(index.saturating_sub(1)))
        } else if pressed(&[egui::Key::Home]) {
            Some(Step::To(0))
        } else if pressed(&[egui::Key::End]) {
            Some(Step::To(last))
        } else {
            None
        }
    })
}

impl NodeCraftApp {
    pub(super) fn open_presenter(&mut self, node: NodeId) {
        self.selection.only(node);
        self.presenter = Some(Presenter::new(self.canvas, node));
    }

    /// Makes a Deck card from the selected cards, right of them, and presents it.
    pub(super) fn present_selection(&mut self) {
        let document = self.session.document();
        let Some(canvas) = document.canvases().get(&self.canvas) else { return };
        let picked: Vec<_> = self.selection.nodes().iter().filter_map(|id| canvas.nodes().get(id)).filter(|n| n.kind != nodecraft_nodes::deck::ID).collect();
        let (Some(right), Some(top)) = (picked.iter().map(|n| n.pos.x).reduce(f32::max), picked.iter().map(|n| n.pos.y).reduce(f32::min)) else {
            self.notice = Some("Select cards to present.".into());
            return;
        };
        let nodes: Vec<NodeId> = picked.iter().map(|n| n.id).collect();
        let Some(id) = ids::node_id(document, ("deck", self.session.revision())) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        let params = json!({"canvas": self.canvas, "id": id, "pos": {"x": right + GAP, "y": top}, "title": "Deck", "nodes": nodes});
        if self.layer_dispatch("deck.create", &params) {
            self.open_presenter(id);
        }
    }

    /// The presenter, while one is open. True when it owns the frame.
    pub(super) fn deck_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut presenter) = self.presenter.take() else {
            return false;
        };
        let present = (presenter.canvas == self.canvas)
            .then(|| self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&presenter.node)))
            .flatten()
            .is_some_and(|n| n.kind == nodecraft_nodes::deck::ID);
        if !present {
            // The deck went (undo, another canvas): nothing left to present.
            return false;
        }
        let revision = self.session.revision();
        if presenter.listing.as_ref().is_none_or(|(r, _)| *r != revision) {
            let listed = self.session.dispatch("deck.list", &json!({"canvas": presenter.canvas, "id": presenter.node})).map_err(|e| e.message);
            match listed.and_then(|v| listing(v).ok_or_else(|| "The deck's slides can't be read.".to_owned())) {
                Ok(listing) => presenter.listing = Some((revision, listing)),
                Err(message) => {
                    self.notice = Some(message);
                    return false;
                }
            }
        }
        let Some((_, listing)) = presenter.listing.take() else {
            return false;
        };
        let ctx = ui.ctx().clone();
        let count = listing.slides.len();
        presenter.index = presenter.index.min(count.saturating_sub(1));
        let mut close = false;
        match step(&ctx, presenter.index, count) {
            Some(Step::Leave) => close = true,
            Some(Step::To(index)) if index != presenter.index => {
                presenter.index = index;
                presenter.textures.clear();
            }
            _ => {}
        }

        let t = *self.theme.tokens();
        let content = ctx.content_rect();
        let gap = t.control_gap * 2.0;
        let footer = t.control_gap * 6.0;
        let frame = listing.frame;
        let (fw, fh) = (f64::from(frame.width), f64::from(frame.height));
        let room = (content.size() - Vec2::new(gap * 2.0, gap * 2.0 + footer)).max(Vec2::splat(1.0));
        let scale = (f64::from(room.x) / fw).min(f64::from(room.y) / fh);
        let size = Vec2::new((fw * scale) as f32, (fh * scale) as f32);
        let slide_rect = Rect::from_center_size(Pos2::new(content.center().x, content.min.y + gap + room.y / 2.0), size);
        let stage = Stage { origin: slide_rect.min, scale };

        // What each layer of the slide shows.
        let slide = listing.slides.get(presenter.index);
        let drawn = slide.map(|(slide, sources)| {
            deck::drawable(slide, frame, |origin, nth| match sources.iter().find(|s| s.origin == origin && s.nth == nth) {
                Some(Source { image: Some(image), .. }) => Shown::Image(image.clone()),
                Some(Source { text: Some(text), .. }) => Shown::Text(text.clone()),
                _ => Shown::Missing,
            })
        });
        let edge = (content.size().max_elem() * ctx.pixels_per_point()).min(2048.0);
        let render_at = render_scale(scale * f64::from(ctx.pixels_per_point()));
        let fonts = Fonts::default();
        let mut art = HashMap::new();
        for layer in drawn.iter().flat_map(|d| &d.layers) {
            let shown = match &layer.content {
                Content::Image { input, fit } => {
                    let image = drawn.as_ref().and_then(|d| d.images.get(*input as usize));
                    let texture = image.and_then(|asset| {
                        let assets = self.session.assets();
                        match self.previews.request_asset(&ctx, asset.hash, edge, || assets.preview_read(asset)) {
                            crate::preview::Preview::Ready { texture, source_size, .. } => Some((texture, source_size)),
                            _ => None,
                        }
                    });
                    Art::Image { texture, fit: *fit }
                }
                Content::Source { fit, .. } => Art::Image { texture: None, fit: *fit },
                Content::Text { .. } | Content::Shape { .. } => {
                    let key = render_key(layer, render_at, 0);
                    if presenter.textures.get(&layer.id).is_none_or(|r| r.key != key) {
                        presenter.textures.insert(layer.id.clone(), render(&ctx, layer, render_at, &fonts, 0));
                    }
                    let rendered = presenter.textures.get(&layer.id);
                    Art::Vector { texture: rendered.and_then(|r| r.texture.as_ref().map(|t| t.id())), pad: rendered.map_or(0.0, |r| r.pad) }
                }
            };
            art.insert(layer.id.clone(), shown);
        }
        presenter.textures.retain(|id, _| art.contains_key(id));

        let [r, g, b, a] = frame.background;
        let background = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let shell = egui::Frame::NONE.fill(t.canvas_bg).inner_margin(gap);
        let notice = self.notice.clone();
        egui::Modal::new(egui::Id::new("deck_presenter")).frame(shell).backdrop_color(t.canvas_bg).show(&ctx, |ui| {
            ui.set_min_size((content.size() - Vec2::splat(gap * 2.0)).max(Vec2::splat(1.0)));
            let response = ui.allocate_rect(slide_rect, Sense::click());
            let painter = ui.painter_at(slide_rect);
            painter.rect_filled(slide_rect, 0.0, background);
            for layer in drawn.iter().flat_map(|d| &d.layers) {
                draw_layer(&painter, stage, layer, art.get(&layer.id), &t);
            }
            if count == 0 {
                painter.text(
                    slide_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "This deck has no slides yet.",
                    egui::FontId::proportional(t.control_gap * 4.0),
                    t.text_muted,
                );
            }
            if response.clicked() {
                presenter.index = presenter.index.saturating_add(1).min(count.saturating_sub(1));
                presenter.textures.clear();
            }
            let bar = Rect::from_min_max(Pos2::new(content.min.x + gap, slide_rect.max.y + gap), Pos2::new(content.max.x - gap, content.max.y - gap));
            ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
                ui.horizontal(|ui| {
                    let name = slide.map_or("", |(s, _)| s.name.as_str());
                    let place = if count == 0 { "0 / 0".to_owned() } else { format!("{} / {count}", presenter.index.saturating_add(1)) };
                    ui.label(if name.is_empty() { place } else { format!("{place} · {name}") });
                    let missing = drawn.as_ref().map_or(0, |d| d.missing.len());
                    if missing > 0 {
                        let what = if missing == 1 { "1 source has nothing to show".to_owned() } else { format!("{missing} sources have nothing to show") };
                        ui.label(egui::RichText::new(what).color(t.text_muted));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close |= ui.button("Done").on_hover_text("Esc").clicked();
                        if ui.add_enabled(presenter.index.saturating_add(1) < count, egui::Button::new("Next")).clicked() {
                            presenter.index = presenter.index.saturating_add(1);
                            presenter.textures.clear();
                        }
                        if ui.add_enabled(presenter.index > 0, egui::Button::new("Previous")).clicked() {
                            presenter.index = presenter.index.saturating_sub(1);
                            presenter.textures.clear();
                        }
                        if let Some(notice) = &notice {
                            ui.label(egui::RichText::new(notice).color(t.danger));
                        }
                    });
                });
            });
        });
        presenter.listing = Some((revision, listing));
        if !close {
            self.presenter = Some(presenter);
        }
        true
    }
}
