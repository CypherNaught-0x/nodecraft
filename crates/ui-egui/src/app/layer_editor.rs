//! The Layer Editor (#101): a full-screen view of a Layer Editor card's frame, where its layers
//! are moved, resized, rotated, aligned and restyled. It changes the document only through
//! commands: each finished drag is one `node.layers_set`, each field one `layers.update`, so
//! every gesture is one undo step. Image layers draw from the card previews; each text and shape
//! layer is rendered by the same code a run uses, with the fonts the node holds, into its own
//! texture, and placed on screen with the layer's transform.
//!
//! The same stage, layer list and fields edit one slide of a deck (#694); then every change is a
//! `deck.*` command on that slide instead, and the slide's layers this version can't read are
//! kept where they are.
use super::NodeCraftApp;
use super::layer_fonts::{self, FontFiles, HeldFont};
use crate::theme::Tokens;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use nodecraft_actions::deck::{self, Item};
use nodecraft_actions::image::ImageLimits;
use nodecraft_actions::layers::{self, Align, Content, Edge, Fonts, Layer, ShapeKind, same_name};
use nodecraft_types::{AssetRef, CanvasId, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::HashMap;

/// Rotation snaps to this many degrees while Shift is held.
const ROTATE_STEP: f64 = 15.0;
/// Arrow keys move the selection this many frame pixels, ten times as far with Shift.
const NUDGE: f64 = 1.0;
const ALIGN: [(Edge, &str); 6] =
    [(Edge::Left, "Left"), (Edge::Center, "Centre"), (Edge::Right, "Right"), (Edge::Top, "Top"), (Edge::Middle, "Middle"), (Edge::Bottom, "Bottom")];

/// What the editor's layers belong to.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Target {
    /// A Layer Editor card, changed with `layers.*` and `node.layers_set`.
    Card,
    /// The slide of this id on a Deck card, changed with `deck.slide_update` and
    /// `deck.layer_update`; the host fills in each one's `expected_revision`.
    Slide(String),
}

pub(super) struct LayerEditor {
    canvas: CanvasId,
    pub(super) node: NodeId,
    pub(super) target: Target,
    /// The selected layers' ids.
    pub(super) selected: Vec<String>,
    drag: Option<Drag>,
    /// `layers.list` as of a document revision.
    listing: Option<(u64, Listing)>,
    /// Each text and shape layer's rendering, by layer id.
    pub(super) textures: HashMap<String, Rendered>,
    /// A field being typed or dragged, applied when it lets go: (field key, text).
    draft: Option<(String, String)>,
    /// A field had focus last frame: egui drops it on Escape before the frame runs, so this is
    /// how Escape knows to discard the draft rather than apply it.
    typing: bool,
    /// Frame pixels to screen points, and where the frame's corner is, as last drawn; text
    /// renders at this scale.
    pub(super) scale: f64,
    pub(super) origin: Pos2,
    /// The bytes of each font the node holds.
    pub(super) font_files: FontFiles,
    /// "Add font file…" was pressed, for this text layer if one was shown.
    add_font: Option<Option<String>>,
    /// A font file being chosen or read.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) picking: Option<layer_fonts::Picking>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) picking_revision: Option<u64>,
}

impl LayerEditor {
    pub(super) fn new(canvas: CanvasId, node: NodeId) -> Self {
        Self {
            canvas,
            node,
            target: Target::Card,
            selected: Vec::new(),
            drag: None,
            listing: None,
            textures: HashMap::new(),
            draft: None,
            typing: false,
            scale: 1.0,
            origin: Pos2::ZERO,
            font_files: FontFiles::default(),
            add_font: None,
            #[cfg(not(target_arch = "wasm32"))]
            picking: None,
            #[cfg(not(target_arch = "wasm32"))]
            picking_revision: None,
        }
    }

    /// An editor for one slide of the Deck card `node`.
    pub(super) fn slide(canvas: CanvasId, node: NodeId, slide: String) -> Self {
        Self { target: Target::Slide(slide), ..Self::new(canvas, node) }
    }

    /// Ids of the layers being dragged.
    pub(super) fn dragged(&self) -> Vec<String> {
        self.drag.iter().flat_map(|d| d.now.iter().map(|l| l.id.clone())).collect()
    }

    /// Before drawing: Escape in a field abandons what was typed, and does nothing else. True
    /// when it did, so a caller with fields of its own drops their drafts too.
    pub(super) fn begin(&mut self, ctx: &egui::Context) -> bool {
        let abandon = self.typing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if abandon {
            self.draft = None;
        }
        abandon
    }

    /// After drawing: whether a field has focus, for the next frame's Escape.
    pub(super) fn end(&mut self, ctx: &egui::Context) {
        self.typing = crate::canvas::text_input_focused(ctx);
    }

    /// Keeps the selection and renderings to layers that still exist.
    pub(super) fn keep(&mut self, listing: &Listing) {
        self.textures.retain(|id, _| listing.layers.iter().any(|l| &l.id == id));
        self.selected.retain(|id| listing.layers.iter().any(|l| &l.id == id));
    }
}

/// A text or shape layer's art from `textures`, rendered again when it changed. Mid-drag, the
/// old rendering stretches; it renders again on release.
pub(super) fn vector_art(
    textures: &mut HashMap<String, Rendered>,
    ctx: &egui::Context,
    layer: &Layer,
    scale: f64,
    fonts: &Fonts<'_>,
    fonts_key: u64,
    dragged: &[String],
) -> Art {
    let key = render_key(layer, scale, fonts_key);
    let stale = textures.get(&layer.id).is_none_or(|r| r.key != key);
    if stale && !(dragged.contains(&layer.id) && textures.contains_key(&layer.id)) {
        textures.insert(layer.id.clone(), render(ctx, layer, scale, fonts, fonts_key));
    }
    let rendered = textures.get(&layer.id);
    Art::Vector { texture: rendered.and_then(|r| r.texture.as_ref().map(|t| t.id())), pad: rendered.map_or(0.0, |r| r.pad) }
}

#[derive(Deserialize)]
pub(super) struct Listing {
    width: u32,
    height: u32,
    pub(super) layers: Vec<Layer>,
    #[serde(default)]
    inputs: Vec<Input>,
    #[serde(default)]
    fonts: Vec<HeldFont>,
    /// A slide's layers as stored, bottom first, those this version can't read included.
    #[serde(skip)]
    raw: Vec<Json>,
    /// The cards a slide's new source layer can show: (node id, title).
    #[serde(skip)]
    choices: Vec<(String, String)>,
}

impl Listing {
    /// A slide of a `width` × `height` deck, its layers as `deck.list` gives them.
    pub(super) fn slide(width: u32, height: u32, raw: Vec<Json>, choices: Vec<(String, String)>, fonts: Vec<HeldFont>) -> Self {
        let layers = raw.iter().filter_map(|r| serde_json::from_value::<Item>(r.clone()).ok()).filter_map(|item| item.layer().cloned()).collect();
        Self { width, height, layers, inputs: Vec::new(), fonts, raw, choices }
    }

    /// Every layer id in use, those of layers this version can't read included.
    fn taken(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().map(|l| l.id.as_str()).chain(self.raw.iter().filter_map(|r| r.get("id").and_then(Json::as_str)))
    }
}

#[derive(Deserialize)]
struct Input {
    origin: String,
    image: Option<AssetRef>,
}

pub(super) struct Rendered {
    pub(super) key: u64,
    /// None when the layer can't be drawn at this size.
    pub(super) texture: Option<egui::TextureHandle>,
    /// Layer pixels of room around the box, as `render_layer` returns it.
    pub(super) pad: f64,
}

/// What a layer shows on screen this frame.
pub(super) enum Art {
    /// An image layer's preview and the source's size, once decoded.
    Image {
        texture: Option<(egui::TextureId, [u32; 2])>,
        fit: bool,
    },
    Vector {
        texture: Option<egui::TextureId>,
        pad: f64,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Grip {
    Move,
    /// A corner, by the side of the centre it is on: -1 or 1 across and down.
    Corner(f64, f64),
    Rotate,
}

struct Drag {
    grip: Grip,
    /// Where the pointer went down, in frame pixels.
    start: (f64, f64),
    /// The dragged layers as they were, and as they are now.
    before: Vec<Layer>,
    now: Vec<Layer>,
    guides: Vec<Guide>,
}

#[derive(Clone, Copy)]
enum Guide {
    X(f64),
    Y(f64),
}

/// Frame pixels on screen.
#[derive(Clone, Copy)]
pub(super) struct Stage {
    pub(super) origin: Pos2,
    pub(super) scale: f64,
}

impl Stage {
    fn screen(self, (x, y): (f64, f64)) -> Pos2 {
        Pos2::new(self.origin.x + (x * self.scale) as f32, self.origin.y + (y * self.scale) as f32)
    }
    fn frame(self, p: Pos2) -> (f64, f64) {
        (f64::from(p.x - self.origin.x) / self.scale, f64::from(p.y - self.origin.y) / self.scale)
    }
}

fn rotate((x, y): (f64, f64), degrees: f64) -> (f64, f64) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (x * cos - y * sin, x * sin + y * cos)
}

fn flips(layer: &Layer) -> (f64, f64) {
    (if layer.flip_x { -1.0 } else { 1.0 }, if layer.flip_y { -1.0 } else { 1.0 })
}

/// A point of the layer's own box (`0..width`, `0..height`) in frame pixels, as
/// [`Layer::transform`] maps it.
fn to_frame(layer: &Layer, (u, v): (f64, f64)) -> (f64, f64) {
    let (fx, fy) = flips(layer);
    let (dx, dy) = rotate(((u - layer.width / 2.0) * fx, (v - layer.height / 2.0) * fy), layer.rotation);
    (layer.x + dx, layer.y + dy)
}

fn to_box(layer: &Layer, (x, y): (f64, f64)) -> (f64, f64) {
    let (fx, fy) = flips(layer);
    let (dx, dy) = rotate((x - layer.x, y - layer.y), -layer.rotation);
    (dx * fx + layer.width / 2.0, dy * fy + layer.height / 2.0)
}

fn contains(layer: &Layer, p: (f64, f64)) -> bool {
    let (u, v) = to_box(layer, p);
    (0.0..=layer.width).contains(&u) && (0.0..=layer.height).contains(&v)
}

/// A corner on screen as it is drawn: `side` is -1 or 1 across and down from the centre,
/// rotation included and flips ignored (they mirror the box onto itself).
fn corner(layer: &Layer, (sx, sy): (f64, f64)) -> (f64, f64) {
    let (dx, dy) = rotate((sx * layer.width / 2.0, sy * layer.height / 2.0), layer.rotation);
    (layer.x + dx, layer.y + dy)
}

const CORNERS: [(f64, f64); 4] = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];

/// The rotate handle, `offset` frame pixels above the top edge.
fn rotate_handle(layer: &Layer, offset: f64) -> (f64, f64) {
    let (dx, dy) = rotate((0.0, -layer.height / 2.0 - offset), layer.rotation);
    (layer.x + dx, layer.y + dy)
}

/// The joint bounds of `layers`: (left, top, right, bottom).
fn joint(layers: &[Layer]) -> Option<(f64, f64, f64, f64)> {
    layers.iter().map(Layer::bounds).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// How far to move `moving` (already offset) so an edge or centre meets the nearest guide within
/// `within`: the frame's edges and centre, and every other shown layer's.
fn snap(moving: &[Layer], others: &[&Layer], frame: (f64, f64), within: f64) -> ((f64, f64), Vec<Guide>) {
    let Some((l, t, r, b)) = joint(moving) else {
        return ((0.0, 0.0), Vec::new());
    };
    let mut xs = vec![0.0, frame.0 / 2.0, frame.0];
    let mut ys = vec![0.0, frame.1 / 2.0, frame.1];
    for (ol, ot, or, ob) in others.iter().map(|o| o.bounds()) {
        xs.extend([ol, (ol + or) / 2.0, or]);
        ys.extend([ot, (ot + ob) / 2.0, ob]);
    }
    let best = |edges: [f64; 3], lines: &[f64]| {
        let mut best: Option<(f64, f64)> = None;
        for edge in edges {
            for line in lines {
                let d = line - edge;
                if d.abs() <= within && best.is_none_or(|(b, _)| d.abs() < b.abs()) {
                    best = Some((d, *line));
                }
            }
        }
        best
    };
    let mut guides = Vec::new();
    let dx = best([l, (l + r) / 2.0, r], &xs).map_or(0.0, |(d, line)| {
        guides.push(Guide::X(line));
        d
    });
    let dy = best([t, (t + b) / 2.0, b], &ys).map_or(0.0, |(d, line)| {
        guides.push(Guide::Y(line));
        d
    });
    ((dx, dy), guides)
}

/// The degrees from straight up to `p`, clockwise, around `centre`.
fn bearing(centre: (f64, f64), p: (f64, f64)) -> f64 {
    (p.0 - centre.0).atan2(centre.1 - p.1).to_degrees()
}

/// Into -180..=180.
fn normal_angle(degrees: f64) -> f64 {
    let a = degrees.rem_euclid(360.0);
    if a > 180.0 { a - 360.0 } else { a }
}

impl Drag {
    /// Moves the dragged layers for the pointer at `p` (frame pixels).
    fn update(&mut self, p: (f64, f64), others: &[&Layer], frame: (f64, f64), within: f64, shift: bool, free: bool) {
        self.guides.clear();
        match self.grip {
            Grip::Move => {
                let (mut dx, mut dy) = (p.0 - self.start.0, p.1 - self.start.1);
                if shift {
                    // Along one axis only.
                    if dx.abs() > dy.abs() { dy = 0.0 } else { dx = 0.0 }
                }
                let offset = |layers: &[Layer], dx: f64, dy: f64| layers.iter().map(|l| Layer { x: l.x + dx, y: l.y + dy, ..l.clone() }).collect::<Vec<_>>();
                self.now = offset(&self.before, dx, dy);
                if !free {
                    // Snapping offsets the moved layers, so a snapped edge lands exactly on its target.
                    let ((sx, sy), guides) = snap(&self.now, others, frame, within);
                    self.now = offset(&self.now, sx, sy);
                    self.guides = guides;
                }
            }
            Grip::Corner(sx, sy) => {
                let (Some(before), Some(now)) = (self.before.first(), self.now.first_mut()) else {
                    return;
                };
                let fixed = corner(before, (-sx, -sy));
                let (dx, dy) = rotate((p.0 - fixed.0, p.1 - fixed.1), -before.rotation);
                let (mut w, mut h) = ((dx * sx).max(1.0), (dy * sy).max(1.0));
                if shift && before.width > 0.0 && before.height > 0.0 {
                    let k = (w / before.width).max(h / before.height);
                    (w, h) = ((before.width * k).max(1.0), (before.height * k).max(1.0));
                }
                let (cx, cy) = rotate((sx * w / 2.0, sy * h / 2.0), before.rotation);
                *now = Layer { width: w, height: h, x: fixed.0 + cx, y: fixed.1 + cy, ..before.clone() };
            }
            Grip::Rotate => {
                let (Some(before), Some(now)) = (self.before.first(), self.now.first_mut()) else {
                    return;
                };
                let centre = (before.x, before.y);
                let mut angle = before.rotation + bearing(centre, p) - bearing(centre, self.start);
                if shift {
                    angle = (angle / ROTATE_STEP).round() * ROTATE_STEP;
                }
                now.rotation = normal_angle(angle);
            }
        }
    }
}

/// Render scales step by quarter octaves, so a small zoom change reuses the texture.
pub(super) fn render_scale(scale: f64) -> f64 {
    if !scale.is_finite() || scale <= 0.0 {
        return 1.0;
    }
    ((scale.log2() * 4.0).round() / 4.0).exp2().clamp(1.0 / 64.0, 8.0)
}

pub(super) fn render_key(layer: &Layer, scale: f64, fonts: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&layer.content).unwrap_or_default().hash(&mut hasher);
    fonts.hash(&mut hasher);
    for v in [layer.width, layer.height, scale] {
        v.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn render(ctx: &egui::Context, layer: &Layer, scale: f64, fonts: &Fonts<'_>, fonts_key: u64) -> Rendered {
    let key = render_key(layer, scale, fonts_key);
    // A texture past the GPU's largest side would panic in egui: render again smaller, to fit.
    let largest = ctx.input(|i| i.max_texture_side).max(1) as f64;
    let rendered = layers::render_layer(layer, fonts, scale, ImageLimits::default()).and_then(|(raster, pad)| {
        let side = f64::from(raster.width().max(raster.height()));
        if side <= largest {
            return Ok((raster, pad));
        }
        let (raster, pad) = layers::render_layer(layer, fonts, scale * (largest - 2.0).max(1.0) / side, ImageLimits::default())?;
        if f64::from(raster.width().max(raster.height())) <= largest { Ok((raster, pad)) } else { Err(nodecraft_actions::image::ImageError::Dimensions) }
    });
    match rendered {
        Ok((raster, pad)) => {
            let size = [raster.width() as usize, raster.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, raster.pixels());
            Rendered { key, texture: Some(ctx.load_texture(format!("layer-{}", layer.id), image, egui::TextureOptions::LINEAR)), pad }
        }
        Err(_) => Rendered { key, texture: None, pad: 0.0 },
    }
}

/// A free id `stem-n`.
fn fresh_id(listing: &Listing, stem: &str) -> String {
    let taken: Vec<&str> = listing.taken().collect();
    (1..=taken.len().saturating_add(1)).map(|n| format!("{stem}-{n}")).find(|id| !taken.contains(&id.as_str())).unwrap_or_else(|| stem.into())
}

fn new_layer(listing: &Listing, content: Content, stem: &str, name: &str) -> Layer {
    let (w, h) = (f64::from(listing.width), f64::from(listing.height));
    let (width, height) = match content {
        Content::Text { .. } => (w * 0.6, (h * 0.15).max(1.0)),
        _ => (w.min(h) * 0.3, w.min(h) * 0.3),
    };
    Layer {
        id: fresh_id(listing, stem),
        name: name.into(),
        visible: true,
        locked: false,
        opacity: 1.0,
        x: w / 2.0,
        y: h / 2.0,
        width,
        height,
        rotation: 0.0,
        flip_x: false,
        flip_y: false,
        content,
    }
}

fn text_content(height: f64) -> Content {
    Content::Text {
        text: "Text".into(),
        font: "Lato".into(),
        size: (height * 0.6).clamp(layers::MIN_TEXT_SIZE, layers::MAX_TEXT_SIZE).round(),
        line_spacing: 1.0,
        tracking: 0.0,
        align: Align::Center,
        color: "#000000".into(),
    }
}

fn shape_content(shape: ShapeKind) -> Content {
    Content::Shape { shape, fill: Some("#808080".into()), stroke: None, stroke_width: 0.0 }
}

fn shape_label(shape: ShapeKind) -> &'static str {
    match shape {
        ShapeKind::Rectangle => "Rectangle",
        ShapeKind::Ellipse => "Ellipse",
        ShapeKind::Triangle => "Triangle",
    }
}

pub(super) fn param_json(value: Value) -> Json {
    serde_json::to_value(value).unwrap_or_default()
}

/// A number field that applies when it lets go (drag released, focus lost), so a gesture is
/// one change.
#[allow(clippy::too_many_arguments)]
pub(super) fn number(
    ui: &mut egui::Ui,
    draft: &mut Option<(String, String)>,
    key: &str,
    label: &str,
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    decimals: usize,
) -> Option<f64> {
    let mut v = match draft {
        Some((k, text)) if k == key => text.parse().unwrap_or(value),
        _ => value,
    };
    let label = ui.label(label);
    // Only what the person changes is clamped: a stored value outside the range is left alone.
    let field = egui::DragValue::new(&mut v).range(range).clamp_existing_to_range(false).speed(speed).max_decimals(decimals);
    let r = ui.add(field).labelled_by(label.id);
    if !ui.is_enabled() {
        return None;
    }
    let apply = r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
    if (r.dragged() || r.has_focus()) && !apply {
        *draft = Some((key.into(), v.to_string()));
    } else if draft.as_ref().is_some_and(|(k, _)| k == key) {
        *draft = None;
    }
    (apply && (v - value).abs() > 1e-9).then_some(v)
}

/// A text field that applies when it loses focus.
pub(super) fn text_field(ui: &mut egui::Ui, draft: &mut Option<(String, String)>, key: &str, label: &str, value: &str, multiline: bool) -> Option<String> {
    let mut text = match draft {
        Some((k, text)) if k == key => text.clone(),
        _ => value.to_owned(),
    };
    let label = ui.label(label);
    let edit = if multiline { egui::TextEdit::multiline(&mut text).desired_rows(3) } else { egui::TextEdit::singleline(&mut text) };
    let r = ui.add(edit.desired_width(f32::INFINITY)).labelled_by(label.id);
    if r.has_focus() {
        *draft = Some((key.into(), text.clone()));
        None
    } else if draft.as_ref().is_some_and(|(k, _)| k == key) {
        *draft = None;
        (r.lost_focus() && text != value).then_some(text)
    } else {
        None
    }
}

/// A `#rrggbb` field with a swatch; empty means none where `optional`.
pub(super) fn colour_field(
    ui: &mut egui::Ui,
    draft: &mut Option<(String, String)>,
    key: &str,
    label: &str,
    value: Option<&str>,
    t: &Tokens,
) -> Option<Option<String>> {
    let mut out = None;
    ui.horizontal(|ui| {
        let swatch = value.and_then(|v| layers::parse_color(v).ok());
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(t.body_size), Sense::hover());
        match swatch {
            Some([r, g, b, _]) => ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(r, g, b)),
            None => ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, t.panel_stroke), egui::StrokeKind::Inside),
        };
        if let Some(text) = text_field(ui, draft, key, label, value.unwrap_or(""), false) {
            let text = text.trim().to_owned();
            out = Some((!text.is_empty()).then_some(text));
        }
    });
    out
}

pub(super) type Commands = Vec<(&'static str, Json)>;

impl NodeCraftApp {
    pub(super) fn open_layer_editor(&mut self, node: NodeId) {
        self.selection.only(node);
        self.layer_editor = Some(LayerEditor::new(self.canvas, node));
    }

    pub(super) fn layer_dispatch(&mut self, command: &str, params: &Json) -> bool {
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
    pub(super) fn layer_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut editor) = self.layer_editor.take() else {
            return false;
        };
        let node = (editor.canvas == self.canvas)
            .then(|| self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&editor.node)))
            .flatten()
            .filter(|n| n.kind == nodecraft_nodes::layers::ID)
            .cloned();
        let Some(node) = node else {
            // The node went (undo, another canvas): nothing left to edit.
            return false;
        };
        let revision = self.session.revision();
        if editor.listing.as_ref().is_none_or(|(r, _)| *r != revision) {
            let listed = self.session.dispatch("layers.list", &json!({"canvas": editor.canvas, "id": editor.node})).map_err(|e| e.message);
            match listed.and_then(|v| serde_json::from_value::<Listing>(v).map_err(|_| "The layers can't be read.".to_owned())) {
                Ok(listing) => editor.listing = Some((revision, listing)),
                Err(message) => {
                    self.notice = Some(message);
                    return false;
                }
            }
        }
        let Some((_, listing)) = editor.listing.take() else {
            return false;
        };
        let t = *self.theme.tokens();
        let ctx = ui.ctx().clone();
        let _ = editor.begin(&ctx);
        let edge = (ctx.content_rect().size().max_elem() * ctx.pixels_per_point()).min(2048.0);

        // What each layer shows: previews for images, a rendering for text and shapes.
        let dragged = editor.dragged();
        let scale = render_scale(editor.scale * f64::from(ctx.pixels_per_point()));
        editor.font_files.load(self.session.assets(), &listing.fonts);
        let fonts = editor.font_files.fonts(&listing.fonts);
        let fonts_key = editor.font_files.render_key(&listing.fonts);
        let mut art = HashMap::new();
        for layer in &listing.layers {
            let shown = match &layer.content {
                Content::Source { origin, nth, fit } => {
                    let image = listing.inputs.iter().filter(|i| &i.origin == origin).nth(*nth as usize).and_then(|i| i.image.as_ref());
                    let texture = image.and_then(|asset| {
                        let assets = self.session.assets();
                        match self.previews.request_asset(&ctx, asset.hash, edge, || assets.preview_read(asset)) {
                            crate::preview::Preview::Ready { texture, source_size, .. } => Some((texture, source_size)),
                            _ => None,
                        }
                    });
                    Art::Image { texture, fit: *fit }
                }
                Content::Image { fit, .. } => Art::Image { texture: None, fit: *fit },
                Content::Text { .. } | Content::Shape { .. } => vector_art(&mut editor.textures, &ctx, layer, scale, &fonts, fonts_key, &dragged),
            };
            art.insert(layer.id.clone(), shown);
        }
        drop(fonts);
        editor.keep(&listing);

        let transparent = matches!(node.params.get("transparent"), Some(Value::Bool(true)));
        let background = match node.params.get("background") {
            Some(Value::Text(text)) => layers::parse_color(text).ok(),
            _ => None,
        }
        .unwrap_or([255, 255, 255, 255]);
        let frame_params = FrameParams {
            width: listing.width,
            height: listing.height,
            background,
            transparent: Some(transparent),
            colour: node.params.get("background").cloned(),
        };

        let mut commands: Commands = Vec::new();
        let mut close = false;
        let notice = self.notice.clone();
        let content = ctx.content_rect();
        let gap = t.control_gap * 2.0;
        let shell = egui::Frame::NONE.fill(t.panel_fill).inner_margin(gap);
        egui::Modal::new(egui::Id::new("layer_editor")).frame(shell).backdrop_color(t.canvas_bg).show(&ctx, |ui| {
            let size = (content.size() - Vec2::splat(gap * 2.0)).max(Vec2::splat(1.0));
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.horizontal(|ui| {
                ui.heading("Layers");
                ui.label(egui::RichText::new(&node.title).color(t.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close |= ui.button("Done").clicked();
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
            let side = t.layers_panel_width.min(body.width() / 4.0);
            let left = Rect::from_min_size(body.min, Vec2::new(side, body.height()));
            let right = Rect::from_min_max(Pos2::new(body.max.x - side, body.min.y), body.max);
            let centre = Rect::from_min_max(Pos2::new(left.max.x + gap, body.min.y), Pos2::new(right.min.x - gap, body.max.y));
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| layers_panel(ui, &mut editor, &listing, &t, &mut commands));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                egui::ScrollArea::vertical().id_salt("layer_properties").show(ui, |ui| properties(ui, &mut editor, &listing, &frame_params, &t, &mut commands));
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(centre), |ui| stage(ui, &mut editor, &listing, &art, &frame_params, &t, &mut commands));
            ui.allocate_rect(body, Sense::hover());
        });
        close |= keys(ui, &mut editor, &listing, &mut commands);
        editor.end(&ctx);
        self.layer_font_pick(&mut editor, &ctx, &mut commands, None);
        editor.listing = Some((revision, listing));
        for (command, params) in commands {
            if !self.layer_dispatch(command, &params) {
                break;
            }
        }
        if !close {
            self.layer_editor = Some(editor);
        }
        true
    }
}

impl NodeCraftApp {
    /// Starts a font pick that was asked for and moves one along; a font read is added to the
    /// node, then set on the text layer it was picked for.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn layer_font_pick(&mut self, editor: &mut LayerEditor, ctx: &egui::Context, commands: &mut Commands, expected: Option<u64>) -> bool {
        use layer_fonts::{Picking, Polled};
        if let Some(layer) = editor.add_font.take()
            && editor.picking.is_none()
        {
            editor.picking = Some(Picking::start(layer));
            editor.picking_revision = expected;
        }
        let Some(picking) = editor.picking.take() else {
            return false;
        };
        let picked_revision = editor.picking_revision.take();
        match picking.poll(ctx) {
            Polled::Waiting(picking) => {
                editor.picking = Some(picking);
                editor.picking_revision = picked_revision;
            }
            Polled::Cancelled => {}
            Polled::Failed(message) => self.notice = Some(message),
            Polled::Read(layer, bytes) => match match &editor.target {
                Target::Card => nodecraft_engine::layer_cmds::fonts::import(&mut self.session, editor.canvas, editor.node, &bytes),
                Target::Slide(_) => nodecraft_engine::deck_cmds::fonts::import(&mut self.session, editor.canvas, editor.node, &bytes, picked_revision),
            } {
                Ok(name) => {
                    editor.font_files.forget_missing();
                    self.notice = None;
                    if let Some(layer) = layer {
                        commands.push(update(editor, &layer, json!({"content": {"font": name}})));
                    }
                    return true;
                }
                Err(error) => self.notice = Some(error.message),
            },
        }
        false
    }

    /// The browser has no font picker yet; fonts are added through `layers.font_add`.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn layer_font_pick(&mut self, editor: &mut LayerEditor, _: &egui::Context, _: &mut Commands, _: Option<u64>) -> bool {
        editor.add_font = None;
        false
    }
}

/// Whether the font picker is offered here.
const PICKS_FONTS: bool = cfg!(not(target_arch = "wasm32"));

/// Why `font` draws as Lato, when it does: the node doesn't hold it, or its file can't be read.
fn font_fallback(files: &FontFiles, listing: &Listing, font: &str) -> Option<String> {
    if files.readable(&listing.fonts, font) {
        None
    } else if listing.fonts.iter().any(|f| same_name(&f.name, font)) {
        Some(format!("{font}'s file can't be read, so it draws as Lato. Add the font file again to use it."))
    } else {
        Some(format!("{font} isn't held by this card, so it draws as Lato. Add the font file to use it."))
    }
}

/// A choice of Lato and the node's fonts for a text layer; the name chosen, when it changed.
fn font_choice(ui: &mut egui::Ui, key: &str, font: &str, listing: &Listing) -> Option<String> {
    let mut chosen = None;
    let label = ui.label("Font");
    let names = std::iter::once("Lato").chain(listing.fonts.iter().map(|f| f.name.as_str()));
    let shown = egui::ComboBox::from_id_salt(key).selected_text(font).width(ui.available_width()).show_ui(ui, |ui| {
        for name in names {
            if ui.selectable_label(same_name(name, font), name).clicked() && !same_name(name, font) {
                chosen = Some(name.to_owned());
            }
        }
    });
    shown.response.labelled_by(label.id);
    chosen
}

/// The fonts the node holds, each with a way to remove it, and the way to add one.
fn fonts_panel(ui: &mut egui::Ui, editor: &mut LayerEditor, listing: &Listing, t: &Tokens, commands: &mut Commands) {
    ui.separator();
    ui.label(egui::RichText::new("Fonts").color(t.text_muted));
    ui.horizontal(|ui| {
        ui.label("Lato");
        ui.label(egui::RichText::new("built in").color(t.text_muted).small());
    });
    for font in &listing.fonts {
        ui.horizontal(|ui| {
            ui.label(&font.name);
            if !editor.font_files.readable(&listing.fonts, &font.name) {
                ui.label(egui::RichText::new("can't be read; draws as Lato").color(t.stale).small());
            }
            let remove = ui.small_button("Remove").on_hover_text("Text in this font draws as Lato until it is added again");
            remove.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Remove {}", font.name)));
            if remove.clicked() {
                let command = if editor.target == Target::Card { "layers.font_remove" } else { "deck.font_remove" };
                commands.push((command, json!({"canvas": editor.canvas, "id": editor.node, "name": font.name})));
            }
        });
    }
    if PICKS_FONTS && ui.button("Add font file…").clicked() {
        editor.add_font = Some(None);
    }
    ui.label(
        egui::RichText::new(
            "A .ttf or .otf file, up to 16 MiB; at most 16 per card, and one of a name the card holds replaces it. It is saved with the project.",
        )
        .color(t.text_muted)
        .small(),
    );
}

pub(super) struct FrameParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) background: [u8; 4],
    /// Whether the background is see-through; `None` where there's no such choice (a slide).
    pub(super) transparent: Option<bool>,
    pub(super) colour: Option<Value>,
}

fn on(editor: &LayerEditor, layer: &str) -> Json {
    json!({"canvas": editor.canvas, "id": editor.node, "layer": layer})
}

/// A slide's stored layers rewritten as one `deck.slide_update`.
fn slide_layers(editor: &LayerEditor, slide: &str, layers: Vec<Json>) -> (&'static str, Json) {
    ("deck.slide_update", json!({"canvas": editor.canvas, "id": editor.node, "slide": slide, "layers": layers}))
}

fn layer_json(layer: &Layer) -> Json {
    serde_json::to_value(layer).unwrap_or(Json::Null)
}

fn raw_id(raw: &Json) -> Option<&str> {
    raw.get("id").and_then(Json::as_str)
}

fn update(editor: &LayerEditor, layer: &str, set: Json) -> (&'static str, Json) {
    let mut params = on(editor, layer);
    params["set"] = set;
    match &editor.target {
        Target::Card => ("layers.update", params),
        Target::Slide(slide) => {
            params["slide"] = json!(slide);
            ("deck.layer_update", params)
        }
    }
}

/// The whole stack with `changed` in place of the layers it names, as one command.
fn set_all(editor: &LayerEditor, listing: &Listing, changed: &[Layer]) -> (&'static str, Json) {
    match &editor.target {
        Target::Card => {
            let layers: Vec<&Layer> = listing.layers.iter().map(|l| changed.iter().find(|c| c.id == l.id).unwrap_or(l)).collect();
            ("node.layers_set", json!({"canvas": editor.canvas, "id": editor.node, "layers": layers}))
        }
        Target::Slide(slide) => {
            let layers = listing.raw.iter().map(|r| changed.iter().find(|c| raw_id(r) == Some(c.id.as_str())).map_or_else(|| r.clone(), layer_json)).collect();
            slide_layers(editor, slide, layers)
        }
    }
}

/// `layer` added on top.
fn add_layer(editor: &LayerEditor, listing: &Listing, layer: &Layer) -> (&'static str, Json) {
    match &editor.target {
        Target::Card => ("layers.add", json!({"canvas": editor.canvas, "id": editor.node, "layer": layer})),
        Target::Slide(slide) => {
            let mut layers = listing.raw.clone();
            layers.push(layer_json(layer));
            slide_layers(editor, slide, layers)
        }
    }
}

/// The layers `ids` taken away: one undo step on a slide, one each on a card.
fn remove_layers(editor: &LayerEditor, listing: &Listing, ids: &[String], commands: &mut Commands) {
    match &editor.target {
        Target::Card => commands.extend(ids.iter().map(|id| ("layers.remove", on(editor, id)))),
        Target::Slide(slide) if !ids.is_empty() => {
            let layers = listing.raw.iter().filter(|r| raw_id(r).is_none_or(|id| !ids.iter().any(|i| i == id))).cloned().collect();
            commands.push(slide_layers(editor, slide, layers));
        }
        Target::Slide(_) => {}
    }
}

/// Layer `id`, at `index` in the readable stack, moved one place up (`raise`) or down.
fn reorder(editor: &LayerEditor, listing: &Listing, id: &str, index: usize, raise: bool) -> Option<(&'static str, Json)> {
    match &editor.target {
        Target::Card => {
            let mut params = on(editor, id);
            params["index"] = json!(if raise { index.saturating_add(1) } else { index.saturating_sub(1) });
            Some(("layers.reorder", params))
        }
        Target::Slide(slide) => {
            // Past the readable layer next to it; layers this version can't read stay put.
            let neighbour = if raise { listing.layers.get(index.saturating_add(1)) } else { index.checked_sub(1).and_then(|i| listing.layers.get(i)) }?;
            let mut layers = listing.raw.clone();
            let from = layers.iter().position(|r| raw_id(r) == Some(id))?;
            let moved = layers.remove(from);
            let at = layers.iter().position(|r| raw_id(r) == Some(neighbour.id.as_str()))?;
            layers.insert(if raise { at.saturating_add(1) } else { at }, moved);
            Some(slide_layers(editor, slide, layers))
        }
    }
}

/// The layers `ids` lined up along `to`.
fn align_cmd(editor: &LayerEditor, listing: &Listing, ids: &[&str], to: Edge) -> Option<(&'static str, Json)> {
    match &editor.target {
        Target::Card => Some(("layers.align", json!({"canvas": editor.canvas, "id": editor.node, "layers": ids, "to": to}))),
        Target::Slide(_) => {
            let mut layers = listing.layers.clone();
            let picked: Vec<usize> = ids.iter().filter_map(|id| layers.iter().position(|l| l.id == *id)).collect();
            layers::align(&mut layers, &picked, to, (f64::from(listing.width), f64::from(listing.height)));
            let moved: Vec<Layer> = layers.into_iter().filter(|l| listing.layers.iter().any(|o| o.id == l.id && o != l)).collect();
            (!moved.is_empty()).then(|| set_all(editor, listing, &moved))
        }
    }
}

/// Keys the editor answers while no field has focus: arrows nudge, Delete removes, Escape
/// cancels a drag, then clears the selection, then closes; undo and redo work as on the canvas.
/// True to close.
pub(super) fn keys(ui: &egui::Ui, editor: &mut LayerEditor, listing: &Listing, commands: &mut Commands) -> bool {
    use egui::{Key, Modifiers};
    if crate::canvas::text_input_focused(ui.ctx()) {
        return false;
    }
    let mut close = false;
    ui.input_mut(|i| {
        for (id, shortcut) in [("edit.redo", "Command+Shift+Z"), ("edit.redo", "Command+Y"), ("edit.undo", "Command+Z")] {
            if super::consume_shortcut(i, shortcut) {
                commands.push((id, json!({})));
            }
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) && editor.drag.take().is_none() {
            close = editor.selected.is_empty();
            editor.selected.clear();
        }
        if editor.drag.is_some() {
            return;
        }
        if i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace) {
            let ids = std::mem::take(&mut editor.selected);
            remove_layers(editor, listing, &ids, commands);
            return;
        }
        let step = if i.modifiers.shift { NUDGE * 10.0 } else { NUDGE };
        let mut d = (0.0, 0.0);
        for (key, by) in [(Key::ArrowLeft, (-step, 0.0)), (Key::ArrowRight, (step, 0.0)), (Key::ArrowUp, (0.0, -step)), (Key::ArrowDown, (0.0, step))] {
            if i.consume_key(Modifiers::NONE, key) || i.consume_key(Modifiers::SHIFT, key) {
                d = (d.0 + by.0, d.1 + by.1);
            }
        }
        if d != (0.0, 0.0) {
            let moved: Vec<Layer> = selected(editor, listing).filter(|l| !l.locked).map(|l| Layer { x: l.x + d.0, y: l.y + d.1, ..l.clone() }).collect();
            if !moved.is_empty() {
                commands.push(set_all(editor, listing, &moved));
            }
        }
    });
    close
}

fn selected<'a>(editor: &'a LayerEditor, listing: &'a Listing) -> impl Iterator<Item = &'a Layer> {
    listing.layers.iter().filter(|l| editor.selected.contains(&l.id))
}

/// Select `id`: alone, or toggled into the selection with Shift or Command.
fn pick(editor: &mut LayerEditor, id: &str, add: bool) {
    if add {
        if let Some(at) = editor.selected.iter().position(|s| s == id) {
            editor.selected.remove(at);
        } else {
            editor.selected.push(id.into());
        }
    } else {
        editor.selected = vec![id.into()];
    }
}

/// The stack, top first, with buttons to add, raise, lower and delete.
pub(super) fn layers_panel(ui: &mut egui::Ui, editor: &mut LayerEditor, listing: &Listing, t: &Tokens, commands: &mut Commands) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Add text").clicked() {
            let mut layer = new_layer(listing, text_content(1.0), "text", "Text");
            layer.content = text_content(layer.height);
            editor.selected = vec![layer.id.clone()];
            commands.push(add_layer(editor, listing, &layer));
        }
        ui.menu_button("Add shape", |ui| {
            for shape in [ShapeKind::Rectangle, ShapeKind::Ellipse, ShapeKind::Triangle] {
                if ui.button(shape_label(shape)).clicked() {
                    let layer = new_layer(listing, shape_content(shape), "shape", shape_label(shape));
                    editor.selected = vec![layer.id.clone()];
                    commands.push(add_layer(editor, listing, &layer));
                    ui.close();
                }
            }
        });
        if matches!(editor.target, Target::Slide(_)) {
            ui.menu_button("Add source", |ui| {
                if listing.choices.is_empty() {
                    ui.label(egui::RichText::new("No other cards on this canvas.").color(t.text_muted));
                }
                for (origin, title) in &listing.choices {
                    if ui.button(title).clicked() {
                        let (w, h) = (f64::from(listing.width), f64::from(listing.height));
                        let mut layer = deck::source_layer(origin, title, (w / 2.0, h / 2.0), (w / 2.0, h / 2.0));
                        layer.id = fresh_id(listing, "source");
                        editor.selected = vec![layer.id.clone()];
                        commands.push(add_layer(editor, listing, &layer));
                        ui.close();
                    }
                }
            });
        }
    });
    let one = (editor.selected.len() == 1).then(|| editor.selected.first().cloned()).flatten();
    let index = one.as_ref().and_then(|id| listing.layers.iter().position(|l| &l.id == id));
    ui.horizontal_wrapped(|ui| {
        let top = listing.layers.len().saturating_sub(1);
        for (label, raise, enabled) in [("Raise", true, index.is_some_and(|i| i < top)), ("Lower", false, index.is_some_and(|i| i > 0))] {
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                && let (Some(id), Some(i)) = (&one, index)
            {
                commands.extend(reorder(editor, listing, id, i, raise));
            }
        }
        if ui.add_enabled(!editor.selected.is_empty(), egui::Button::new("Delete")).clicked() {
            let ids = std::mem::take(&mut editor.selected);
            remove_layers(editor, listing, &ids, commands);
        }
    });
    ui.separator();
    egui::ScrollArea::vertical().id_salt("layer_list").show(ui, |ui| {
        if listing.layers.is_empty() {
            let empty = match editor.target {
                Target::Card => "Wire images into the card, or add text or a shape.",
                Target::Slide(_) => "Add text, a shape or a source.",
            };
            ui.label(egui::RichText::new(empty).color(t.text_muted));
        }
        for layer in listing.layers.iter().rev() {
            ui.horizontal(|ui| {
                let mut visible = layer.visible;
                let shown = ui.checkbox(&mut visible, "").on_hover_text("Show in the output");
                shown.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, visible, format!("Show {}", layer.name)));
                if shown.changed() {
                    commands.push(update(editor, &layer.id, json!({"visible": visible})));
                }
                let is_selected = editor.selected.contains(&layer.id);
                let mut name = egui::RichText::new(&layer.name);
                if !layer.visible {
                    name = name.color(t.text_muted);
                }
                if ui.selectable_label(is_selected, name).clicked() {
                    let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                    pick(editor, &layer.id, add);
                }
                if layer.locked {
                    ui.label(egui::RichText::new("locked").color(t.text_muted));
                }
            });
        }
    });
}

/// The selected layer's fields, alignment, or (with nothing selected) the frame's.
pub(super) fn properties(ui: &mut egui::Ui, editor: &mut LayerEditor, listing: &Listing, frame: &FrameParams, t: &Tokens, commands: &mut Commands) {
    // A field still holding an edit stays on screen until it applies, even when the press that
    // took its focus also changed the selection.
    let held = editor.draft.as_ref().map(|(key, _)| key.as_str());
    let holder = held
        .and_then(|key| listing.layers.iter().filter(|l| key.strip_prefix(l.id.as_str()).is_some_and(|rest| rest.starts_with('.'))).max_by_key(|l| l.id.len()));
    let chosen: Vec<Layer> = match (held, holder) {
        (Some(key), _) if key.starts_with("frame.") => Vec::new(),
        (_, Some(layer)) => vec![layer.clone()],
        _ => selected(editor, listing).cloned().collect(),
    };
    if !chosen.is_empty() {
        ui.label(egui::RichText::new("Align").color(t.text_muted));
        // The layers the panel shows, which is the selection unless a held edit says otherwise.
        let ids: Vec<&str> = chosen.iter().map(|l| l.id.as_str()).collect();
        ui.horizontal_wrapped(|ui| {
            for (to, label) in ALIGN {
                let hint = if chosen.len() == 1 { "Against the frame" } else { "Against the selection" };
                if ui.button(label).on_hover_text(hint).clicked() {
                    commands.extend(align_cmd(editor, listing, &ids, to));
                }
            }
        });
        ui.separator();
    }
    let [layer] = chosen.as_slice() else {
        if chosen.is_empty() {
            frame_properties(ui, editor, frame, t, commands);
            fonts_panel(ui, editor, listing, t, commands);
        } else {
            ui.label(egui::RichText::new(format!("{} layers selected", chosen.len())).color(t.text_muted));
        }
        return;
    };
    let id = layer.id.clone();
    let key = |field: &str| format!("{id}.{field}");
    let draft = &mut editor.draft;
    let mut set: Vec<Json> = Vec::new();
    if let Some(name) = text_field(ui, draft, &key("name"), "Name", &layer.name, false) {
        set.push(json!({"name": name}));
    }
    ui.horizontal(|ui| {
        for (field, label, on) in [("visible", "Visible", layer.visible), ("locked", "Locked", layer.locked)] {
            let mut v = on;
            if ui.checkbox(&mut v, label).changed() {
                set.push(json!({field: v}));
            }
        }
    });
    ui.add_enabled_ui(!layer.locked, |ui| {
        egui::Grid::new("layer_geometry").num_columns(4).show(ui, |ui| {
            let limit = layers::MAX_COORD;
            for row in [[("x", "X", layer.x, -limit), ("y", "Y", layer.y, -limit)], [("width", "W", layer.width, 1.0), ("height", "H", layer.height, 1.0)]] {
                for (field, label, value, min) in row {
                    if let Some(v) = number(ui, draft, &key(field), label, value, min..=limit, 1.0, 1) {
                        set.push(json!({field: v}));
                    }
                }
                ui.end_row();
            }
            if let Some(v) = number(ui, draft, &key("rotation"), "Rotation", layer.rotation, -180.0..=180.0, 1.0, 1) {
                set.push(json!({"rotation": v}));
            }
            ui.end_row();
        });
        ui.horizontal(|ui| {
            for (field, label, on) in [("flip_x", "Flip across", layer.flip_x), ("flip_y", "Flip down", layer.flip_y)] {
                let mut v = on;
                if ui.checkbox(&mut v, label).changed() {
                    set.push(json!({field: v}));
                }
            }
        });
    });
    ui.horizontal(|ui| {
        if let Some(v) = number(ui, draft, &key("opacity"), "Opacity", (layer.opacity * 100.0).round(), 0.0..=100.0, 1.0, 0) {
            set.push(json!({"opacity": v / 100.0}));
        }
        ui.label("%");
    });
    ui.separator();
    match &layer.content {
        Content::Text { text, font, size, line_spacing, tracking, align, color } => {
            if let Some(v) = text_field(ui, draft, &key("text"), "Text", text, true) {
                set.push(json!({"content": {"text": v}}));
            }
            if let Some(v) = font_choice(ui, &key("font"), font, listing) {
                set.push(json!({"content": {"font": v}}));
            }
            if let Some(why) = font_fallback(&editor.font_files, listing, font) {
                ui.label(egui::RichText::new(why).color(t.stale).small());
            }
            if PICKS_FONTS && ui.button("Add font file…").clicked() {
                editor.add_font = Some(Some(id.clone()));
            }
            egui::Grid::new("layer_text").num_columns(2).show(ui, |ui| {
                for (field, label, value, range, speed) in [
                    ("size", "Size", *size, layers::MIN_TEXT_SIZE..=layers::MAX_TEXT_SIZE, 0.5),
                    ("line_spacing", "Line spacing", *line_spacing, 0.5..=5.0, 0.01),
                    ("tracking", "Tracking (em)", *tracking, -1.0..=2.0, 0.005),
                ] {
                    if let Some(v) = number(ui, draft, &key(field), label, value, range, speed, 3) {
                        set.push(json!({"content": {field: v}}));
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                for (choice, label, word) in [(Align::Left, "Left", "left"), (Align::Center, "Centre", "center"), (Align::Right, "Right", "right")] {
                    if ui.selectable_label(*align == choice, label).clicked() && *align != choice {
                        set.push(json!({"content": {"align": word}}));
                    }
                }
            });
            if let Some(Some(v)) = colour_field(ui, draft, &key("color"), "Colour", Some(color), t) {
                set.push(json!({"content": {"color": v}}));
            }
        }
        Content::Shape { shape, fill, stroke, stroke_width } => {
            ui.horizontal(|ui| {
                for choice in [ShapeKind::Rectangle, ShapeKind::Ellipse, ShapeKind::Triangle] {
                    if ui.selectable_label(*shape == choice, shape_label(choice)).clicked() && *shape != choice {
                        set.push(json!({"content": {"shape": choice}}));
                    }
                }
            });
            if let Some(v) = colour_field(ui, draft, &key("fill"), "Fill", fill.as_deref(), t) {
                set.push(json!({"content": {"fill": v}}));
            }
            if let Some(v) = colour_field(ui, draft, &key("stroke"), "Outline", stroke.as_deref(), t) {
                set.push(json!({"content": {"stroke": v}}));
            }
            ui.horizontal(|ui| {
                if let Some(v) = number(ui, draft, &key("stroke_width"), "Outline width", *stroke_width, 0.0..=layers::MAX_STROKE, 0.5, 1) {
                    set.push(json!({"content": {"stroke_width": v}}));
                }
            });
        }
        Content::Source { fit, .. } | Content::Image { fit, .. } => {
            let mut v = *fit;
            if ui.checkbox(&mut v, "Keep proportions").on_hover_text("Fit the whole image in the box rather than stretching it").changed() {
                set.push(json!({"content": {"fit": v}}));
            }
        }
    }
    for change in set {
        commands.push(update(editor, &id, change));
    }
}

fn frame_properties(ui: &mut egui::Ui, editor: &mut LayerEditor, frame: &FrameParams, t: &Tokens, commands: &mut Commands) {
    let (canvas, id) = (editor.canvas, editor.node);
    let param = |name: &str, value: Value| ("node.param_set", json!({"canvas": canvas, "id": id, "name": name, "value": param_json(value)}));
    ui.label(egui::RichText::new("Frame").color(t.text_muted));
    if matches!(editor.target, Target::Slide(_)) {
        // Every slide shares the deck's frame, and slide edits are all `deck.*` commands.
        ui.label(format!("{} × {} pixels", frame.width, frame.height));
        ui.label(egui::RichText::new("Every slide shares this size; change it on the Deck card.").color(t.text_muted).small());
        return;
    }
    let (min, max) = (f64::from(layers::MIN_FRAME), f64::from(layers::MAX_FRAME));
    egui::Grid::new("layer_frame").num_columns(2).show(ui, |ui| {
        for (name, label, value) in [("width", "Width", frame.width), ("height", "Height", frame.height)] {
            if let Some(v) = number(ui, &mut editor.draft, &format!("frame.{name}"), label, f64::from(value), min..=max, 4.0, 0) {
                commands.push(param(name, Value::Unsigned(nodecraft_types::Unsigned(v.round() as u64))));
            }
            ui.end_row();
        }
    });
    let colour = match &frame.colour {
        Some(Value::Text(text)) => text.clone(),
        _ => "#ffffff".into(),
    };
    if let Some(Some(v)) = colour_field(ui, &mut editor.draft, "frame.background", "Background", Some(&colour), t) {
        commands.push(param("background", Value::Text(v)));
    }
    if let Some(mut clear) = frame.transparent
        && ui.checkbox(&mut clear, "Transparent background").changed()
    {
        commands.push(param("transparent", Value::Bool(clear)));
    }
    ui.label(egui::RichText::new("Pixels, 100 to 4096. Layers keep their place when the frame changes size.").color(t.text_muted).small());
}

/// Two triangles covering `quad` (screen corners, clockwise from the box's top left) with the
/// whole of `texture`.
fn textured(texture: egui::TextureId, quad: [Pos2; 4], tint: Color32) -> egui::Shape {
    let mut mesh = egui::Mesh::with_texture(texture);
    let uv = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
    for (pos, uv) in quad.into_iter().zip(uv) {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: tint });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    egui::Shape::mesh(mesh)
}

/// The corners of the box `(u0, v0)..(u1, v1)` of `layer` on screen.
fn quad(stage: Stage, layer: &Layer, (u0, v0): (f64, f64), (u1, v1): (f64, f64)) -> [Pos2; 4] {
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)].map(|p| stage.screen(to_frame(layer, p)))
}

pub(super) fn draw_layer(painter: &egui::Painter, stage: Stage, layer: &Layer, art: Option<&Art>, t: &Tokens) {
    let tint = Color32::WHITE.gamma_multiply(layer.opacity.clamp(0.0, 1.0) as f32);
    let (w, h) = (layer.width, layer.height);
    match art {
        Some(Art::Image { texture: Some((texture, [iw, ih])), fit }) => {
            let (iw, ih) = (f64::from(*iw), f64::from(*ih));
            let (from, to) = if *fit && iw > 0.0 && ih > 0.0 {
                let s = (w / iw).min(h / ih);
                let (fw, fh) = (iw * s, ih * s);
                (((w - fw) / 2.0, (h - fh) / 2.0), ((w + fw) / 2.0, (h + fh) / 2.0))
            } else {
                ((0.0, 0.0), (w, h))
            };
            painter.add(textured(*texture, quad(stage, layer, from, to), tint));
        }
        Some(Art::Vector { texture: Some(texture), pad }) => {
            painter.add(textured(*texture, quad(stage, layer, (-pad, -pad), (w + pad, h + pad)), tint));
        }
        _ => {
            // Not drawable yet (an image still to run, or too large to preview): its box.
            let corners = quad(stage, layer, (0.0, 0.0), (w, h)).to_vec();
            painter.add(egui::Shape::convex_polygon(corners, t.card_fill.gamma_multiply(0.6), egui::Stroke::new(1.0, t.panel_stroke)));
        }
    }
}

fn checker(painter: &egui::Painter, rect: Rect, t: &Tokens) {
    painter.rect_filled(rect, 0.0, t.card_fill);
    let cell = (t.body_size).max(4.0);
    let (cols, rows) = ((rect.width() / cell).ceil() as usize, (rect.height() / cell).ceil() as usize);
    for row in 0..rows.min(512) {
        for col in (row % 2..cols.min(512)).step_by(2) {
            let min = rect.min + Vec2::new(col as f32 * cell, row as f32 * cell);
            painter.rect_filled(Rect::from_min_size(min, Vec2::splat(cell)).intersect(rect), 0.0, t.panel_stroke);
        }
    }
}

/// What the pointer would take hold of at `p` (screen): a handle of the one selected layer, else
/// the topmost shown layer under it.
fn grip_at(editor: &LayerEditor, listing: &Listing, stage: Stage, p: Pos2, t: &Tokens) -> Option<(String, Grip)> {
    let reach = t.handle_radius;
    if let [id] = editor.selected.as_slice()
        && let Some(layer) = listing.layers.iter().find(|l| &l.id == id).filter(|l| !l.locked)
    {
        let offset = f64::from(t.layer_rotate_offset) / stage.scale;
        if stage.screen(rotate_handle(layer, offset)).distance(p) <= reach {
            return Some((id.clone(), Grip::Rotate));
        }
        if let Some(side) = CORNERS.iter().find(|side| stage.screen(corner(layer, **side)).distance(p) <= reach) {
            return Some((id.clone(), Grip::Corner(side.0, side.1)));
        }
    }
    let at = stage.frame(p);
    listing.layers.iter().rev().find(|l| l.visible && contains(l, at)).map(|l| (l.id.clone(), Grip::Move))
}

/// The frame and its layers, the selection with its handles, and pointer gestures.
#[allow(clippy::too_many_arguments)]
pub(super) fn stage(
    ui: &mut egui::Ui,
    editor: &mut LayerEditor,
    listing: &Listing,
    art: &HashMap<String, Art>,
    frame: &FrameParams,
    t: &Tokens,
    commands: &mut Commands,
) {
    let area = ui.available_rect_before_wrap();
    let (fw, fh) = (f64::from(listing.width.max(1)), f64::from(listing.height.max(1)));
    let room = area.shrink(f32::from(t.panel_radius));
    let scale = (f64::from(room.width()) / fw).min(f64::from(room.height()) / fh);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    editor.scale = scale;
    let size = Vec2::new((fw * scale) as f32, (fh * scale) as f32);
    let frame_rect = Rect::from_center_size(room.center(), size);
    let stage = Stage { origin: frame_rect.min, scale };
    editor.origin = frame_rect.min;
    let response = ui.interact(area, ui.id().with("layer_stage"), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Layer stage"));

    // Gestures: press takes hold (and selects), drags move the copy, release stores it.
    let (pressed, down, pointer, shift, free, add) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.interact_pos(),
            i.modifiers.shift,
            i.modifiers.alt,
            i.modifiers.shift || i.modifiers.command,
        )
    });
    if pressed
        && response.hovered()
        && let Some(p) = pointer
    {
        match grip_at(editor, listing, stage, p, t) {
            Some((id, grip)) => {
                if grip == Grip::Move && !(editor.selected.contains(&id) && !add) {
                    pick(editor, &id, add);
                }
                let held: Vec<Layer> = if grip == Grip::Move {
                    selected(editor, listing).filter(|l| !l.locked).cloned().collect()
                } else {
                    listing.layers.iter().filter(|l| l.id == id).cloned().collect()
                };
                if !held.is_empty() {
                    editor.drag = Some(Drag { grip, start: stage.frame(p), now: held.clone(), before: held, guides: Vec::new() });
                }
            }
            None if !add => editor.selected.clear(),
            None => {}
        }
    }
    if down {
        if let (Some(drag), Some(p)) = (&mut editor.drag, pointer) {
            let others: Vec<&Layer> = listing.layers.iter().filter(|l| l.visible && drag.before.iter().all(|b| b.id != l.id)).collect();
            drag.update(stage.frame(p), &others, (fw, fh), f64::from(t.layer_snap_distance) / scale, shift, free);
        }
    } else if let Some(drag) = editor.drag.take()
        && drag.now != drag.before
    {
        commands.push(set_all(editor, listing, &drag.now));
    }

    // The frame, then each shown layer bottom first, as dragged.
    let painter = ui.painter().with_clip_rect(area);
    painter.rect_filled(area, 0.0, t.canvas_bg);
    if frame.transparent == Some(true) {
        checker(&painter, frame_rect, t);
    } else {
        let [r, g, b, _] = frame.background;
        painter.rect_filled(frame_rect, 0.0, Color32::from_rgb(r, g, b));
    }
    let moving = editor.drag.as_ref().map(|d| d.now.as_slice()).unwrap_or_default();
    let current = |l: &'_ Layer| moving.iter().find(|m| m.id == l.id).cloned().unwrap_or_else(|| l.clone());
    let inside = painter.with_clip_rect(frame_rect.intersect(area));
    for layer in listing.layers.iter().filter(|l| l.visible).map(current) {
        draw_layer(&inside, stage, &layer, art.get(&layer.id), t);
    }
    painter.rect_stroke(frame_rect, 0.0, egui::Stroke::new(1.0, t.panel_stroke), egui::StrokeKind::Outside);

    // Guides, the selection, and the handles of a single unlocked layer.
    if let Some(drag) = &editor.drag {
        let guide = egui::Stroke::new(1.0, t.accent);
        for g in &drag.guides {
            match *g {
                Guide::X(x) => painter.vline(stage.screen((x, 0.0)).x, area.y_range(), guide),
                Guide::Y(y) => painter.hline(area.x_range(), stage.screen((0.0, y)).y, guide),
            };
        }
    }
    let outline = egui::Stroke::new(t.handle_stroke, t.accent);
    let chosen: Vec<Layer> = selected(editor, listing).map(current).collect();
    for layer in &chosen {
        let corners = quad(stage, layer, (0.0, 0.0), (layer.width, layer.height)).to_vec();
        painter.add(egui::Shape::closed_line(corners, outline));
    }
    if let [layer] = chosen.as_slice()
        && !layer.locked
    {
        let radius = t.port_radius;
        let top = stage.screen(corner(layer, (0.0, -1.0)));
        let knob = stage.screen(rotate_handle(layer, f64::from(t.layer_rotate_offset) / scale));
        painter.line_segment([top, knob], outline);
        painter.circle(knob, radius, t.handle_fill, outline);
        for side in CORNERS {
            let at = stage.screen(corner(layer, side));
            painter.rect(Rect::from_center_size(at, Vec2::splat(radius * 2.0)), 1.0, t.handle_fill, outline, egui::StrokeKind::Middle);
        }
    }

    if let Some(p) = response.hover_pos() {
        let icon = match editor.drag.as_ref().map(|d| d.grip).or_else(|| grip_at(editor, listing, stage, p, t).map(|(_, g)| g)) {
            Some(Grip::Move) => egui::CursorIcon::Move,
            Some(Grip::Corner(sx, sy)) if sx * sy > 0.0 => egui::CursorIcon::ResizeNwSe,
            Some(Grip::Corner(..)) => egui::CursorIcon::ResizeNeSw,
            Some(Grip::Rotate) => egui::CursorIcon::Alias,
            None => egui::CursorIcon::Default,
        };
        ui.ctx().set_cursor_icon(icon);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(rotation: f64, flip_x: bool) -> Layer {
        Layer {
            id: "a".into(),
            name: "A".into(),
            visible: true,
            locked: false,
            opacity: 1.0,
            x: 100.0,
            y: 50.0,
            width: 40.0,
            height: 20.0,
            rotation,
            flip_x,
            flip_y: false,
            content: shape_content(ShapeKind::Rectangle),
        }
    }

    #[test]
    fn a_layer_wider_than_the_largest_texture_renders_smaller_instead_of_panicking() {
        let ctx = egui::Context::default();
        let largest = ctx.input(|i| i.max_texture_side);
        let mut wide = layer(0.0, false);
        wide.width = largest as f64 * 1.5;
        wide.height = 400.0;
        wide.content = Content::Shape { shape: ShapeKind::Rectangle, fill: Some("#ff0000".into()), stroke: None, stroke_width: 0.0 };
        let rendered = render(&ctx, &wide, 1.0, &Fonts::default(), 0);
        let size = rendered.texture.map(|t| t.size()).unwrap();
        assert!(size[0] <= largest && size[1] <= largest, "{size:?}");
    }

    #[test]
    fn screen_geometry_matches_the_layer_transform() {
        for (rotation, flip) in [(0.0, false), (30.0, false), (-75.0, true)] {
            let l = layer(rotation, flip);
            let [a, b, c, d, e, f] = l.transform().as_coeffs();
            for p in [(0.0, 0.0), (40.0, 0.0), (13.0, 7.0)] {
                let ours = to_frame(&l, p);
                let theirs = (a * p.0 + c * p.1 + e, b * p.0 + d * p.1 + f);
                assert!((ours.0 - theirs.0).abs() < 1e-9 && (ours.1 - theirs.1).abs() < 1e-9, "{rotation} {p:?}");
                let back = to_box(&l, ours);
                assert!((back.0 - p.0).abs() < 1e-9 && (back.1 - p.1).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn a_corner_drag_keeps_the_opposite_corner_and_rotation_turns_about_the_centre() {
        let l = layer(30.0, false);
        let fixed = corner(&l, (-1.0, -1.0));
        let mut drag = Drag { grip: Grip::Corner(1.0, 1.0), start: corner(&l, (1.0, 1.0)), before: vec![l.clone()], now: vec![l.clone()], guides: Vec::new() };
        // Grown about the top-left corner: the far corner, rotated, lands where the pointer is.
        let grown = Layer { x: fixed.0 + rotate((40.0, 30.0), 30.0).0, y: fixed.1 + rotate((40.0, 30.0), 30.0).1, width: 80.0, height: 60.0, ..l.clone() };
        let target = corner(&grown, (1.0, 1.0));
        drag.update(target, &[], (400.0, 200.0), 0.0, false, true);
        let now = &drag.now[0];
        assert!((now.width - 80.0).abs() < 1e-9 && (now.height - 60.0).abs() < 1e-9, "{now:?}");
        let kept = corner(now, (-1.0, -1.0));
        assert!((kept.0 - fixed.0).abs() < 1e-9 && (kept.1 - fixed.1).abs() < 1e-9);

        let mut spin = Drag { grip: Grip::Rotate, start: (100.0, 0.0), before: vec![l.clone()], now: vec![l.clone()], guides: Vec::new() };
        spin.update((150.0, 50.0), &[], (400.0, 200.0), 0.0, false, true);
        assert!((spin.now[0].rotation - 120.0).abs() < 1e-9, "{}", spin.now[0].rotation);
        spin.update((150.0, 49.0), &[], (400.0, 200.0), 0.0, true, true);
        assert_eq!(spin.now[0].rotation, 120.0, "Shift snaps to 15°");
    }

    #[test]
    fn moving_snaps_edges_and_centres_to_the_frame_and_other_layers() {
        let l = layer(0.0, false);
        let other = Layer { id: "b".into(), x: 300.0, ..layer(0.0, false) };
        let mut drag = Drag { grip: Grip::Move, start: (100.0, 50.0), before: vec![l.clone()], now: vec![l], guides: Vec::new() };
        // The centre (197) meets the frame's (200), and the top edge (42) the other layer's (40).
        drag.update((197.0, 52.0), &[&other], (400.0, 200.0), 4.0, false, false);
        assert_eq!((drag.now[0].x, drag.now[0].y), (200.0, 50.0));
        assert_eq!(drag.guides.len(), 2);
        // The right edge (278) meets the other layer's left edge (280); nothing is near in y.
        drag.update((258.0, 80.0), &[&other], (400.0, 200.0), 4.0, false, false);
        assert_eq!((drag.now[0].x, drag.now[0].y, drag.guides.len()), (260.0, 80.0, 1));
        // Alt moves freely.
        drag.update((258.0, 80.0), &[&other], (400.0, 200.0), 4.0, false, true);
        assert_eq!((drag.now[0].x, drag.guides.len()), (258.0, 0));
    }
}
