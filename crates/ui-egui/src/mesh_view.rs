//! 3D model previews on cards (#111): a model decoded off the frame, drawn by the wgpu renderer
//! in `nodecraft-gpu` through an egui paint callback, and turned by dragging it.
//!
//! The decoded models are kept in a bounded cache here; their GPU buffers and each card's
//! offscreen images live in the egui renderer's callback resources ([`MeshResources`]), put there
//! by [`install`]. Without that (tests, a renderer that isn't wgpu) the card says what the model
//! is instead of drawing it. The camera and style are view state: turning a model changes nothing
//! in the document.
use eframe::egui_wgpu::{self, wgpu};
use egui::{FontId, Rect, Sense, Vec2};
use nodecraft_gpu::mesh::{GpuMesh, MeshData, MeshRenderer, Orbit, Style, Target, View};
use nodecraft_graph::Node;
use nodecraft_media::{MeshFormat, MeshLimits, decode_mesh};
use nodecraft_types::{AssetRef, Blake3, Value};
use std::{collections::HashMap, sync::Arc};

use crate::theme::Tokens;

/// Decoded models kept at once, in bytes.
pub const MAX_MODEL_BYTES: usize = 512 * 1024 * 1024;
/// GPU buffers kept at once, in bytes.
pub const MAX_GPU_BYTES: u64 = 512 * 1024 * 1024;
/// Most edges a wireframe draws; a model with more is drawn shaded instead.
pub const MAX_EDGES: usize = 6_000_000;
/// Largest model decoded inline where there's no worker (the browser build).
pub const MAX_INLINE_BYTES: usize = 8 * 1024 * 1024;
/// Offscreen images kept at once; the least recently drawn go first.
const MAX_TARGETS: usize = 32;
/// Frames a card's offscreen image survives without being drawn.
const TARGET_FRAMES: u64 = 120;

/// The 3D model a node shows: a Source 3D node's asset, else the first model in its active result.
pub fn mesh_of(node: &Node) -> Option<&AssetRef> {
    fn model(value: &Value) -> Option<&AssetRef> {
        match value {
            Value::Media(asset) if MeshFormat::from_mime(asset.mime.essence()).is_some() => Some(asset),
            _ => None,
        }
    }
    if node.kind == nodecraft_nodes::mesh::SOURCE {
        return node.params.get("asset").and_then(model);
    }
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    result.outputs.iter().find_map(|value| match value {
        Value::List(items) => items.iter().find_map(|item| match item {
            Value::List(inner) => inner.iter().find_map(model),
            single => model(single),
        }),
        single => model(single),
    })
}

/// A decoded model, ready to upload.
pub struct Model {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    edges: Option<Vec<u32>>,
    center: [f32; 3],
    radius: f32,
    bytes: usize,
}

impl Model {
    fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mesh = decode_mesh(bytes, &MeshLimits::default()).map_err(|_| "Can't read this 3D model")?;
            let bounds = mesh.bounds().ok_or("This 3D model is empty")?;
            let edges = mesh.edges(MAX_EDGES);
            let bytes = mesh
                .positions
                .len()
                .saturating_mul(40)
                .saturating_add(mesh.indices.len().saturating_mul(4))
                .saturating_add(edges.as_ref().map_or(0, |e| e.len().saturating_mul(4)));
            Ok(Self {
                center: bounds.center(),
                radius: bounds.radius(),
                positions: mesh.positions,
                normals: mesh.normals,
                colors: mesh.colors,
                indices: mesh.indices,
                edges,
                bytes,
            })
        }))
        .unwrap_or(Err("Can't read this 3D model"))
    }

    /// Triangles in the model.
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    fn data(&self) -> MeshData<'_> {
        MeshData { positions: &self.positions, normals: &self.normals, colors: &self.colors, indices: &self.indices, edges: self.edges.as_deref() }
    }
}

/// What a card can show for its model right now.
#[derive(Clone)]
pub enum MeshPreview {
    Ready { hash: Blake3, model: Arc<Model> },
    Pending,
    Unavailable(&'static str),
}

enum Entry {
    Pending,
    Ready { model: Arc<Model>, used: u64 },
    Failed(&'static str),
}

/// How one card looks at its model: view state, never saved.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshView {
    pub orbit: Orbit,
    pub style: Style,
}

type Done = (Blake3, Result<Model, &'static str>);

/// The decoded-model cache. One per app.
#[derive(Default)]
pub struct Meshes {
    entries: HashMap<Blake3, Entry>,
    bytes: usize,
    clock: u64,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<Worker>,
}

#[cfg(not(target_arch = "wasm32"))]
struct Worker {
    done: std::sync::mpsc::Receiver<Done>,
    handle: Option<std::thread::JoinHandle<()>>,
    cancel: nodecraft_format::asset_source::TransferCancel,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Worker {
    fn drop(&mut self) {
        // A decode can't be stopped half way, but a read can; the thread ends on its own.
        self.cancel.cancel();
        drop(self.handle.take());
    }
}

fn read_message(reason: nodecraft_format::PreviewReadError) -> &'static str {
    use nodecraft_format::PreviewReadError as E;
    match reason {
        E::Missing => "This 3D model's data is missing",
        E::TooLarge => "This 3D model is too large to preview",
        E::Busy => "Preview busy; try again",
        E::Corrupt => "Can't read this 3D model",
        E::Cancelled => "Preview cancelled",
        E::Unavailable | E::Unreadable | E::Capacity => "3D preview unavailable",
    }
}

impl Meshes {
    /// Decoded bytes held now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Takes a finished decode; call once per frame.
    pub fn poll(&mut self) {
        self.clock = self.clock.wrapping_add(1);
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::sync::mpsc::TryRecvError;
            let finished = match self.worker.as_ref().map(|w| w.done.try_recv()) {
                Some(Ok(done)) => Some(Some(done)),
                Some(Err(TryRecvError::Disconnected)) => Some(None),
                Some(Err(TryRecvError::Empty)) | None => None,
            };
            if let Some(done) = finished {
                if let Some(mut worker) = self.worker.take()
                    && let Some(handle) = worker.handle.take()
                {
                    // The thread has sent its one result (or died); joining is immediate.
                    let _ = handle.join();
                }
                match done {
                    Some((hash, result)) => self.finish(hash, result),
                    // The worker died mid-decode: let the model be asked for again.
                    None => self.entries.retain(|_, entry| !matches!(entry, Entry::Pending)),
                }
            }
        }
    }

    fn finish(&mut self, hash: Blake3, result: Result<Model, &'static str>) {
        let entry = match result {
            Ok(model) => {
                self.bytes = self.bytes.saturating_add(model.bytes);
                Entry::Ready { model: Arc::new(model), used: self.clock }
            }
            Err(why) => Entry::Failed(why),
        };
        if let Some(Entry::Ready { model, .. }) = self.entries.insert(hash, entry) {
            self.bytes = self.bytes.saturating_sub(model.bytes);
        }
        self.evict();
    }

    /// Drops least recently used models until under budget, never one used this frame.
    fn evict(&mut self) {
        while self.bytes > MAX_MODEL_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter_map(|(hash, entry)| match entry {
                    Entry::Ready { used, .. } if *used != self.clock => Some((*used, *hash)),
                    _ => None,
                })
                .min();
            let Some((_, hash)) = oldest else { return };
            if let Some(Entry::Ready { model, .. }) = self.entries.remove(&hash) {
                self.bytes = self.bytes.saturating_sub(model.bytes);
            }
        }
    }

    /// The model of `asset`. `source` reads it, and is only called when a decode is about to start.
    pub fn request(
        &mut self,
        ctx: &egui::Context,
        asset: &AssetRef,
        source: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
    ) -> MeshPreview {
        let hash = asset.hash;
        match self.entries.get_mut(&hash) {
            Some(Entry::Ready { model, used }) => {
                *used = self.clock;
                return MeshPreview::Ready { hash, model: Arc::clone(model) };
            }
            Some(Entry::Failed(why)) => return MeshPreview::Unavailable(why),
            Some(Entry::Pending) => return MeshPreview::Pending,
            None => {}
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.worker.is_some() {
            // One decode at a time; this one starts when that one ends.
            return MeshPreview::Pending;
        }
        let input = match source() {
            Ok(input) if input.hash() == hash => input,
            Ok(_) => return self.failed(hash, "Can't read this 3D model"),
            Err(nodecraft_format::PreviewReadError::Missing) => return MeshPreview::Unavailable("This 3D model's data is missing"),
            Err(why) => return self.failed(hash, read_message(why)),
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (send, done) = std::sync::mpsc::sync_channel::<Done>(1);
            let cancel = nodecraft_format::asset_source::TransferCancel::default();
            let read_cancel = cancel.clone();
            let ctx = ctx.clone();
            let spawned = std::thread::Builder::new().name("nodecraft-3d".into()).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    input.read_verified(&read_cancel, &Default::default()).map_err(read_message).and_then(|bytes| Model::decode(&bytes))
                }))
                .unwrap_or(Err("Can't read this 3D model"));
                let _ = send.send((hash, result));
                ctx.request_repaint();
            });
            match spawned {
                Ok(handle) => {
                    self.worker = Some(Worker { done, handle: Some(handle), cancel });
                    self.entries.insert(hash, Entry::Pending);
                    MeshPreview::Pending
                }
                Err(_) => MeshPreview::Unavailable("3D preview unavailable"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            // No worker here: only a small model already in memory decodes inline.
            let result = match input.resident() {
                Some(bytes) if bytes.len() <= MAX_INLINE_BYTES => Model::decode(bytes),
                _ => Err("3D preview not available here"),
            };
            self.finish(hash, result);
            self.request(ctx, asset, || Err(nodecraft_format::PreviewReadError::Unavailable))
        }
    }

    fn failed(&mut self, hash: Blake3, why: &'static str) -> MeshPreview {
        self.entries.insert(hash, Entry::Failed(why));
        MeshPreview::Unavailable(why)
    }

    /// Decodes inline, for tests: the model is ready in the same frame.
    #[cfg(test)]
    pub(crate) fn inline(&mut self, hash: Blake3, bytes: &[u8]) {
        self.finish(hash, Model::decode(bytes));
    }
}

/// What a card draws for its model.
pub struct CardMesh<'a> {
    pub preview: MeshPreview,
    pub view: &'a mut MeshView,
    /// Whether [`install`] put a renderer in place.
    pub gpu: bool,
}

/// Draws a card's model in `area` (world points, drawn at `zoom`), and turns it on drag.
pub(crate) fn show(ui: &mut egui::Ui, area: Rect, zoom: f32, card: CardMesh<'_>, t: &Tokens) {
    let CardMesh { preview, view, gpu } = card;
    let response = ui.interact(area, ui.id().with("mesh"), Sense::click_and_drag());
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    painter.rect_filled(area, t.card_radius, t.canvas_bg);
    let text = |text: &str| painter.text(area.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(t.body_size), t.text_muted);
    let label = match &preview {
        MeshPreview::Ready { hash, model } => {
            if response.dragged() {
                let delta = response.drag_delta();
                let turn = std::f32::consts::PI;
                view.orbit = view.orbit.turned(-delta.x / area.width().max(1.0) * turn, delta.y / area.height().max(1.0) * turn);
            }
            if response.double_clicked() {
                view.orbit = Orbit::default();
            }
            let hovered = response.hovered() || response.dragged();
            if hovered {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 && ui.input(|i| i.modifiers.command || i.modifiers.alt) {
                    view.orbit = view.orbit.zoomed((scroll / 200.0).exp());
                }
            }
            let style = match view.style {
                Style::Shaded => "shaded",
                Style::Unlit => "unlit",
                Style::Wireframe => "wireframe",
            };
            let triangles = model.triangles();
            if gpu {
                // Under the drawing: shows only if the GPU can't take the model.
                text("Can't draw this 3D model here");
                let ppp = ui.ctx().pixels_per_point();
                let pixels = (area.size() * zoom * ppp).round();
                let size = [pixels.x.clamp(1.0, 8192.0) as u32, pixels.y.clamp(1.0, 8192.0) as u32];
                let line = t.wire_mesh.to_normalized_gamma_f32();
                let background = t.canvas_bg.to_normalized_gamma_f32();
                let linear = |c: [f32; 4]| {
                    let decode = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
                    [decode(c[0]), decode(c[1]), decode(c[2]), c[3]]
                };
                let callback = MeshCallback {
                    hash: *hash,
                    model: Arc::clone(model),
                    view: View {
                        orbit: view.orbit,
                        style: view.style,
                        center: model.center,
                        radius: model.radius,
                        background: linear(background),
                        line: linear(line),
                    },
                    size,
                    slot: response.id,
                    frame: ui.ctx().cumulative_frame_nr(),
                };
                painter.add(egui_wgpu::Callback::new_paint_callback(area, callback));
            } else {
                text(&format!("3D model · {triangles} triangles"));
            }
            controls(ui, area, view, t);
            format!("3D model, {triangles} triangles, {style} view")
        }
        MeshPreview::Pending => {
            text("Loading 3D model…");
            "Loading 3D model…".to_owned()
        }
        MeshPreview::Unavailable(why) => {
            text(why);
            (*why).to_owned()
        }
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, label.clone()));
}

/// The style toggle and reset, over the preview's bottom-left corner.
fn controls(ui: &mut egui::Ui, area: Rect, view: &mut MeshView, t: &Tokens) {
    let style = match view.style {
        Style::Shaded => "Shaded",
        Style::Unlit => "Unlit",
        Style::Wireframe => "Wireframe",
    };
    // Small enough to leave the model in view: a line of label text each.
    let height = (t.label_size * 1.8).min(area.height() / 4.0);
    let width = (t.label_size * 6.0).min(area.width() / 3.0);
    let at = area.left_bottom() + Vec2::new(t.control_gap, -t.control_gap - height);
    let style_rect = Rect::from_min_size(at, Vec2::new(width, height));
    let reset_rect = Rect::from_min_size(at + Vec2::new(width + t.control_gap, 0.0), Vec2::new(width * 0.7, height));
    let font = |text: &str| egui::RichText::new(text).size(t.label_size);
    let style_button = ui.put(style_rect, egui::Button::new(font(style)).small()).on_hover_text("Change how the model is drawn");
    if style_button.clicked() {
        view.style = match view.style {
            Style::Shaded => Style::Unlit,
            Style::Unlit => Style::Wireframe,
            Style::Wireframe => Style::Shaded,
        };
    }
    if ui.put(reset_rect, egui::Button::new(font("Reset")).small()).on_hover_text("Back to the starting view").clicked() {
        view.orbit = Orbit::default();
    }
}

/// The renderer and what it keeps between frames, stored in the egui renderer's callback
/// resources.
pub struct MeshResources {
    renderer: MeshRenderer,
    meshes: HashMap<Blake3, (GpuMesh, u64)>,
    refused: HashMap<Blake3, u64>,
    gpu_bytes: u64,
    targets: HashMap<egui::Id, Slot>,
}

struct Slot {
    target: Target,
    used: u64,
    /// The model this frame's image shows, if it was drawn this frame.
    drawn: Option<(Blake3, u64)>,
}

impl MeshResources {
    fn mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, hash: Blake3, model: &Model, frame: u64) -> bool {
        if let Some((_, used)) = self.meshes.get_mut(&hash) {
            *used = frame;
            return true;
        }
        if self.refused.contains_key(&hash) {
            return false;
        }
        match GpuMesh::upload(device, queue, model.data()) {
            Ok(mesh) => {
                self.gpu_bytes = self.gpu_bytes.saturating_add(mesh.bytes());
                self.meshes.insert(hash, (mesh, frame));
                while self.gpu_bytes > MAX_GPU_BYTES {
                    let oldest = self.meshes.iter().filter(|(_, (_, used))| *used != frame).min_by_key(|(_, (_, used))| *used).map(|(hash, _)| *hash);
                    let Some(hash) = oldest else { break };
                    if let Some((mesh, _)) = self.meshes.remove(&hash) {
                        self.gpu_bytes = self.gpu_bytes.saturating_sub(mesh.bytes());
                    }
                }
                true
            }
            Err(_) => {
                self.refused.insert(hash, frame);
                false
            }
        }
    }

    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, call: &MeshCallback) {
        if !self.mesh(device, queue, call.hash, &call.model, call.frame) {
            return;
        }
        let size = self.renderer.clamp_size(call.size);
        let stale = self.targets.get(&call.slot).is_none_or(|slot| slot.target.size() != size);
        if stale {
            let target = self.renderer.target(device, size);
            self.targets.insert(call.slot, Slot { target, used: call.frame, drawn: None });
        }
        let frame = call.frame;
        self.targets.retain(|_, slot| slot.used.saturating_add(TARGET_FRAMES) >= frame);
        while self.targets.len() > MAX_TARGETS {
            let oldest = self.targets.iter().filter(|(id, _)| **id != call.slot).min_by_key(|(_, slot)| slot.used).map(|(id, _)| *id);
            let Some(id) = oldest else { break };
            self.targets.remove(&id);
        }
        let (Some(slot), Some((mesh, _))) = (self.targets.get_mut(&call.slot), self.meshes.get(&call.hash)) else { return };
        self.renderer.render(queue, encoder, &slot.target, mesh, &call.view);
        slot.used = frame;
        slot.drawn = Some((call.hash, frame));
    }
}

/// Puts the 3D renderer into eframe's wgpu renderer, so cards can draw models. Call once, with
/// the creation context's render state.
pub fn install(render: &egui_wgpu::RenderState) {
    let renderer = MeshRenderer::new(&render.device, render.target_format, 1);
    let resources = MeshResources { renderer, meshes: HashMap::new(), refused: HashMap::new(), gpu_bytes: 0, targets: HashMap::new() };
    render.renderer.write().callback_resources.insert(resources);
}

struct MeshCallback {
    hash: Blake3,
    model: Arc<Model>,
    view: View,
    size: [u32; 2],
    slot: egui::Id,
    frame: u64,
}

impl egui_wgpu::CallbackTrait for MeshCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(resources) = resources.get_mut::<MeshResources>() {
            resources.prepare(device, queue, encoder, self);
        }
        Vec::new()
    }

    fn paint(&self, info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(resources) = resources.get::<MeshResources>() else { return };
        let Some(slot) = resources.targets.get(&self.slot) else { return };
        if slot.drawn != Some((self.hash, self.frame)) {
            return;
        }
        let ppp = info.pixels_per_point;
        let rect = info.viewport;
        resources.renderer.blit(pass, &slot.target, [rect.min.x * ppp, rect.min.y * ppp, rect.width() * ppp, rect.height() * ppp]);
    }
}

#[cfg(test)]
mod tests;
