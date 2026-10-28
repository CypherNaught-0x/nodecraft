//! Bounded Deck card thumbnails, using the editor's renderer and shared async image cache.
use super::{
    layer_editor::{Art, Rendered, Stage, draw_layer, render_key, render_scale},
    layer_fonts::HeldFont,
    live::Live,
};
use crate::{
    canvas::CanvasView,
    preview::{Preview, Previews},
    theme::Tokens,
};
use egui::{Color32, FontId, Pos2, Rect, Sense, Vec2};
use nodecraft_actions::{
    deck::{self, Item, Shown, Slide},
    layers::{self, Content, Fonts, Frame, Layer},
};
use nodecraft_engine::{Session, deck_cmds};
use nodecraft_graph::{Canvas, Generation, Node};
use nodecraft_types::{AssetRef, CanvasId, NodeId};
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
};

const MAX_CARDS: usize = 16;
const MAX_SLIDES: usize = 3;
const CARD_TEXTURE_BYTES: u64 = 8 * 1024 * 1024;
mod font_payloads;

pub(super) struct Inputs<'a> {
    pub session: &'a Session,
    pub canvas: &'a Canvas,
    pub node: &'a Node,
    pub live: &'a Live,
    pub previews: &'a mut Previews,
    pub clock: u64,
    pub view: &'a CanvasView,
    pub origin: Pos2,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: VecDeque<Entry>,
    font_payloads: font_payloads::Payloads,
    identity: Option<nodecraft_engine::session::DocumentLineage>,
    #[cfg(test)]
    pub(super) renders: usize,
    #[cfg(test)]
    pub(super) ready_images: HashMap<NodeId, Vec<nodecraft_types::Blake3>>,
}

struct Entry {
    id: (CanvasId, NodeId),
    revision: Option<u64>,
    parsed: Option<Parsed>,
    textures: HashMap<(String, String), Rendered>,
    missing_font: bool,
    used: u64,
}

struct Parsed {
    frame: Frame,
    slides: Vec<Slide>,
    total: usize,
    writable: bool,
}

#[derive(Clone)]
struct Image {
    asset: AssetRef,
    staged: Option<NodeId>,
}

fn parsed(session: &Session, node: &Node) -> Option<Parsed> {
    let frame = deck_cmds::frame_for(session.kinds(), node).ok()?;
    let deck = deck_cmds::stored(node).ok()?;
    let total = deck.slides.len();
    let writable = deck.writable();
    let slides = deck
        .slides
        .into_iter()
        .take(MAX_SLIDES)
        .map(|mut slide| {
            // Unknown data stays in the document, but never occupies a thumbnail cache.
            slide.extra.clear();
            slide.layers.retain(|item| matches!(item, Item::Layer(_)));
            slide
        })
        .collect();
    Some(Parsed { frame, slides, total, writable })
}

fn preview_image(result: &Generation, node: NodeId, nth: u32) -> Shown<Image> {
    match deck_cmds::shown_result(result, nth) {
        Shown::Image(asset) => Shown::Image(Image { asset, staged: Some(node) }),
        Shown::Text(text) => Shown::Text(text),
        Shown::Missing => Shown::Missing,
    }
}

fn source(input: &Inputs<'_>, origin: &str, nth: u32) -> Shown<Image> {
    if let Ok(id) = origin.parse::<NodeId>()
        && let Some((result, _)) = input.live.preview(input.session, input.canvas.id(), id)
    {
        return preview_image(result, id, nth);
    }
    match deck_cmds::shown(input.canvas, origin, nth) {
        Shown::Image(asset) => Shown::Image(Image { asset, staged: None }),
        Shown::Text(text) => Shown::Text(text),
        Shown::Missing => Shown::Missing,
    }
}

fn scale(frame: Frame, area: Rect, count: usize) -> f64 {
    let width = area.width() / count.max(1) as f32;
    (f64::from(width) / f64::from(frame.width)).min(f64::from(area.height()) / f64::from(frame.height)).max(f64::EPSILON)
}

/// Every card shares a fixed texture budget among its vector layers. Hostile geometry draws
/// a placeholder under these bounds rather than allocating full-resolution slide textures.
fn render(ctx: &egui::Context, layer: &Layer, scale: f64, fonts: &Fonts<'_>, key: u64, count: usize) -> Rendered {
    let pixels = CARD_TEXTURE_BYTES / 4 / count.max(1) as u64;
    let side = (pixels as f64).sqrt().floor().min(512.0) as u32;
    let limits = nodecraft_actions::image::ImageLimits {
        edge: side.min(ctx.input(|i| i.max_texture_side).max(1) as u32),
        pixels,
        working_bytes: pixels.saturating_mul(16),
        ..Default::default()
    };
    match layers::render_layer(layer, fonts, scale, limits) {
        Ok((raster, pad)) => {
            let image = egui::ColorImage::from_rgba_unmultiplied([raster.width() as usize, raster.height() as usize], raster.pixels());
            Rendered { key, texture: Some(ctx.load_texture(format!("deck-thumbnail-{}", layer.id), image, egui::TextureOptions::LINEAR)), pad }
        }
        Err(_) => Rendered { key, texture: None, pad: 0.0 },
    }
}

impl Cache {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn forget_missing_fonts(&mut self) {
        self.font_payloads.forget_missing();
    }

    pub(super) fn paint(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, area: Rect, t: &Tokens, input: Inputs<'_>) {
        if self.identity.as_ref().is_none_or(|old| !input.session.is_current_document(old)) {
            self.entries.clear();
            self.font_payloads = Default::default();
            self.identity = Some(input.session.document_lineage());
        }
        #[cfg(test)]
        self.ready_images.insert(input.node.id, Vec::new());
        let id = (input.canvas.id(), input.node.id);
        let at = self.entries.iter().position(|entry| entry.id == id);
        if at.is_none() && self.entries.len() >= MAX_CARDS {
            // Keep this and the previous frame's resident cards. FIFO in paint order makes
            // every card miss forever when one more than the capacity is visible.
            if let Some(old) = self.entries.iter().position(|entry| entry.used.saturating_add(1) < input.clock) {
                self.entries.remove(old);
            } else {
                let label = "Zoom in to preview slides";
                painter.text(area.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(t.body_size), t.text_muted);
                let response = ui.interact(input.view.rect_to_screen(input.origin, area), ui.id().with(("deck_thumbnail_limit", id)), Sense::hover());
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
                response.on_hover_text("Up to 16 visible Deck cards show slide previews at once. Pan or zoom to focus on fewer cards.");
                return;
            }
        }
        let mut entry = at.and_then(|at| self.entries.remove(at)).unwrap_or_else(|| Entry {
            id,
            revision: None,
            parsed: None,
            textures: HashMap::new(),
            missing_font: false,
            used: 0,
        });
        entry.used = input.clock;
        if entry.revision != Some(input.session.revision()) {
            entry.parsed = parsed(input.session, input.node);
            entry.revision = Some(input.session.revision());
        }
        let Some(parsed) = &entry.parsed else {
            painter.text(area.center(), egui::Align2::CENTER_CENTER, "Slides unavailable", FontId::proportional(t.body_size), t.text_muted);
            entry.textures.clear();
            self.entries.push_back(entry);
            return;
        };
        let area = area.shrink2(Vec2::new(0.0, t.control_gap));
        let room = Rect::from_min_max(area.min, Pos2::new(area.max.x, area.max.y - t.label_size - t.control_gap));
        let count = parsed.slides.len();
        let width = (room.width() - t.control_gap * count.saturating_sub(1) as f32) / count.max(1) as f32;
        let thumb_scale = scale(parsed.frame, Rect::from_min_size(room.min, Vec2::new(width, room.height())), 1);
        let render_at = render_scale(thumb_scale * f64::from(input.view.zoom * ui.ctx().pixels_per_point()));
        let drawn: Vec<_> = parsed.slides.iter().map(|slide| deck::drawable(slide, parsed.frame, |origin, nth| source(&input, origin, nth))).collect();
        entry.textures.retain(|(slide, layer), _| parsed.slides.iter().zip(&drawn).any(|(s, d)| &s.id == slide && d.layers.iter().any(|l| &l.id == layer)));
        let vectors = drawn.iter().flat_map(|d| &d.layers).filter(|l| matches!(l.content, Content::Text { .. } | Content::Shape { .. })).count();
        let held: Vec<_> = deck_cmds::fonts::held(input.node)
            .into_iter()
            .filter(|(name, _)| {
                drawn.iter().flat_map(|drawn| &drawn.layers).any(|layer| matches!(&layer.content, Content::Text {font, ..} if layers::same_name(name, font)))
            })
            .map(|(name, font)| HeldFont { name, font })
            .collect();
        let files = self.font_payloads.files(input.session.assets(), &held, input.clock);
        entry.missing_font =
            drawn.iter().flat_map(|drawn| &drawn.layers).any(|layer| matches!(&layer.content, Content::Text {font, ..} if !files.readable(&held, font)));
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        files.render_key(&held).hash(&mut hash);
        // Budget changes invalidate failed/large previews too, even if their content is unchanged.
        vectors.hash(&mut hash);
        let fonts_key = hash.finish();
        let layer_keys = |fonts_key| {
            parsed
                .slides
                .iter()
                .zip(&drawn)
                .flat_map(|(slide, drawn)| {
                    drawn
                        .layers
                        .iter()
                        .filter(|layer| matches!(layer.content, Content::Text { .. } | Content::Shape { .. }))
                        .map(move |layer| ((slide.id.clone(), layer.id.clone()), render_key(layer, render_at, fonts_key)))
                })
                .collect::<HashMap<_, _>>()
        };
        let keys = layer_keys(fonts_key);
        let dirty = keys.iter().any(|(id, key)| entry.textures.get(id).is_none_or(|r| r.key != *key));
        let fonts = if dirty { files.fonts(&held) } else { Fonts::default() };
        for (index, (slide, drawn)) in parsed.slides.iter().zip(&drawn).enumerate() {
            let size = Vec2::new(parsed.frame.width as f32 * thumb_scale as f32, parsed.frame.height as f32 * thumb_scale as f32);
            let cell = Rect::from_min_size(room.min + Vec2::new(index as f32 * (width + t.control_gap), 0.0), Vec2::new(width, room.height()));
            let rect = Rect::from_center_size(cell.center(), size);
            let paint = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
            let [r, g, b, a] = parsed.frame.background;
            paint.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(r, g, b, a));
            for layer in &drawn.layers {
                let art = match &layer.content {
                    Content::Image { input: at, fit } => {
                        let texture = drawn.images.get(*at as usize).and_then(|image| {
                            let assets = image
                                .staged
                                .and_then(|id| input.live.preview(input.session, input.canvas.id(), id).map(|(_, assets)| assets))
                                .unwrap_or(input.session.assets());
                            match input.previews.request_asset(
                                ui.ctx(),
                                image.asset.hash,
                                (size.x * input.view.zoom * ui.ctx().pixels_per_point()).min(512.0),
                                || assets.preview_read(&image.asset),
                            ) {
                                Preview::Ready { texture, source_size, .. } => {
                                    #[cfg(test)]
                                    self.ready_images.entry(input.node.id).or_default().push(image.asset.hash);
                                    Some((texture, source_size))
                                }
                                _ => None,
                            }
                        });
                        Art::Image { texture, fit: *fit }
                    }
                    Content::Source { fit, .. } => Art::Image { texture: None, fit: *fit },
                    Content::Text { .. } | Content::Shape { .. } => {
                        let id = (slide.id.clone(), layer.id.clone());
                        if let Some(key) = keys.get(&id)
                            && entry.textures.get(&id).is_none_or(|r| r.key != *key)
                        {
                            entry.textures.insert(id.clone(), render(ui.ctx(), layer, render_at, &fonts, *key, vectors));
                            #[cfg(test)]
                            {
                                self.renders = self.renders.saturating_add(1);
                            }
                        }
                        let rendered = entry.textures.get(&id);
                        Art::Vector { texture: rendered.and_then(|r| r.texture.as_ref().map(|t| t.id())), pad: rendered.map_or(0.0, |r| r.pad) }
                    }
                };
                draw_layer(&paint, Stage { origin: rect.min, scale: thumb_scale }, layer, Some(&art), t);
            }
            let response = ui.interact(input.view.rect_to_screen(input.origin, rect), ui.id().with(("deck_thumbnail", id, index)), Sense::hover());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, format!("Deck slide {}", index.saturating_add(1))));
            let fallback = if entry.missing_font { "\nSome preview fonts use Lato." } else { "" };
            response.on_hover_text(format!("{} · {}{fallback}", index.saturating_add(1), slide.name));
        }
        let label = match (parsed.writable, parsed.total) {
            (false, _) => "Newer deck version".to_owned(),
            (_, 0) => "No slides yet".to_owned(),
            (_, 1) => "1 slide".to_owned(),
            (_, n) if n > MAX_SLIDES => format!("{MAX_SLIDES} of {n} slides"),
            (_, n) => format!("{n} slides"),
        };
        painter.text(Pos2::new(area.center().x, area.bottom()), egui::Align2::CENTER_BOTTOM, label, FontId::proportional(t.label_size), t.text_muted);
        self.entries.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck695_staged_text_previews_use_the_same_bounded_source_resolution() {
        let result = Generation::new(nodecraft_types::GenerationId::from_bytes([1; 16]), vec![nodecraft_types::Value::Text("é".repeat(4096))]);
        let Shown::Text(text) = preview_image(&result, NodeId::from_bytes([2; 16]), 0) else { panic!("text preview") };
        assert_eq!(text.len(), layers::MAX_TEXT_BYTES / 2);
        assert!(text.chars().all(|c| c == 'é'));
        assert!(matches!(preview_image(&result, NodeId::from_bytes([2; 16]), 1), Shown::Missing));
        assert!(matches!(preview_image(&result, NodeId::from_bytes([2; 16]), u32::MAX), Shown::Missing));
    }

    #[test]
    fn deck695_vector_texture_allocation_is_bounded_for_extreme_valid_geometry() {
        let ctx = egui::Context::default();
        let mut layer = deck::source_layer("box", "Box", (50.0, 50.0), (100.0, 100.0));
        layer.content = Content::Shape { shape: layers::ShapeKind::Rectangle, fill: Some("#336699".into()), stroke: None, stroke_width: 0.0 };
        let count = MAX_SLIDES * layers::MAX_LAYERS;
        let drawn = render(&ctx, &layer, 0.5, &Fonts::default(), 0, count);
        let texture = drawn.texture.unwrap();
        assert!(texture.size()[0] * texture.size()[1] * 4 * count <= CARD_TEXTURE_BYTES as usize);
        layer.width = layers::MAX_COORD;
        layer.height = layers::MAX_COORD;
        layer.check().unwrap();
        assert!(render(&ctx, &layer, 8.0, &Fonts::default(), 0, count).texture.is_none(), "oversized art is a bounded placeholder");
    }

    #[test]
    fn deck695_vector_thumbnail_draws_the_imported_font_instead_of_lato() {
        let mut bytes = include_bytes!("../../../../assets/fonts/Lato-Regular.ttf").to_vec();
        let tables = usize::from(u16::from_be_bytes(bytes[4..6].try_into().unwrap()));
        let head = (0..tables).map(|n| 12 + n * 16).find(|&at| &bytes[at..at + 4] == b"head").unwrap();
        let at = u32::from_be_bytes(bytes[head + 8..head + 12].try_into().unwrap()) as usize;
        bytes[at + 18..at + 20].copy_from_slice(&4096_u16.to_be_bytes());
        let mut fonts = Fonts::default();
        fonts.add("Public font", &bytes).unwrap();
        let mut layer = deck::source_layer("text", "Text", (50.0, 50.0), (30.0, 30.0));
        layer.content = Content::Text {
            text: "MMMMM".into(),
            font: "Public font".into(),
            size: 48.0,
            line_spacing: 1.0,
            tracking: 0.0,
            align: layers::Align::Left,
            color: "#000000".into(),
        };
        let ctx = egui::Context::default();
        let fallback = render(&ctx, &layer, 0.25, &Fonts::default(), 0, 1).texture.unwrap();
        let imported = render(&ctx, &layer, 0.25, &fonts, 1, 1).texture.unwrap();
        assert_ne!(fallback.size(), imported.size(), "different glyph advances produce different rendered bounds");
    }
}
