//! The shell owns view state; the engine owns all document data and mutations.
use crate::{
    canvas::{self, CanvasView},
    card::{self, EditTarget, EditorState, Intent},
    navigation::Navigation,
    selection::{self, Selection},
    theme::{self, ThemeKind},
};
use egui::{Pos2, Rect, Sense, Vec2};
use nodecraft_engine::{
    Session,
    settings::{CanvasSettings, NodeCraftSettings, Theme},
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

const MAX_SCHEMA_CACHE_BYTES: usize = 16 * 1024 * 1024;
struct CachedSchema {
    descriptor: Arc<nodecraft_nodes::NodeDescriptor>,
    visible: BTreeSet<String>,
    descriptor_bytes: usize,
    borrowed_identity: Option<usize>,
    revision: u64,
    last_used: u64,
}

fn cache_bytes(cache: &BTreeMap<NodeId, CachedSchema>) -> usize {
    let mut shared = BTreeSet::new();
    cache.values().fold(0usize, |total, entry| {
        let bytes = if shared.insert(Arc::as_ptr(&entry.descriptor) as usize) { entry.descriptor_bytes } else { 0 };
        // Include conservative per-entry/tree/string overhead, not just shared descriptor text.
        total.saturating_add(bytes).saturating_add(256).saturating_add(entry.visible.iter().map(|name| name.len().saturating_add(96)).sum::<usize>())
    })
}
fn descriptor_bytes(descriptor: &nodecraft_nodes::NodeDescriptor) -> Option<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_SCHEMA_CACHE_BYTES {
                return Err(std::io::Error::other("view schema budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, descriptor).ok()?;
    // Conservative heap accounting for Vec/String/enum/tree overhead beyond wire text.
    counter.0.checked_mul(4).filter(|bytes| *bytes <= MAX_SCHEMA_CACHE_BYTES)
}
fn cached_schema<'a>(cache: &'a mut BTreeMap<NodeId, CachedSchema>, session: &Session, node: &nodecraft_graph::Node, clock: u64) -> Option<&'a CachedSchema> {
    if cache.get(&node.id).is_some_and(|entry| entry.revision == session.revision()) {
        if let Some(entry) = cache.get_mut(&node.id) {
            entry.last_used = clock;
        }
        return cache.get(&node.id);
    }
    let descriptor = session.kinds().get(&node.kind).ok()?.resolve_instance(node).ok()?;
    if node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
        return None;
    }
    let nodecraft_nodes::ResolvedParams { visible, .. } = descriptor.params.resolve(&node.params, false).ok()?;
    let (descriptor, borrowed_identity, bytes) = match descriptor {
        Cow::Borrowed(descriptor) => {
            // Safe pointer identity only: no raw-pointer dereference. Distinct borrowed variants
            // of one dynamic kind remain distinct; the registry outlives this view cache.
            let identity = descriptor as *const nodecraft_nodes::NodeDescriptor as usize;
            if let Some(shared) = cache.values().find(|entry| entry.borrowed_identity == Some(identity)) {
                (Arc::clone(&shared.descriptor), Some(identity), shared.descriptor_bytes)
            } else {
                let bytes = descriptor_bytes(descriptor)?;
                (Arc::new(descriptor.clone()), Some(identity), bytes)
            }
        }
        Cow::Owned(descriptor) => {
            let bytes = descriptor_bytes(&descriptor)?;
            (Arc::new(descriptor), None, bytes)
        }
    };
    cache.insert(node.id, CachedSchema { descriptor, visible, descriptor_bytes: bytes, borrowed_identity, revision: session.revision(), last_used: clock });
    while cache_bytes(cache) > MAX_SCHEMA_CACHE_BYTES {
        let oldest = cache.iter().filter(|(id, _)| **id != node.id).min_by_key(|(_, entry)| entry.last_used).map(|(id, _)| *id);
        if let Some(id) = oldest {
            cache.remove(&id);
        } else {
            cache.remove(&node.id);
            return None;
        }
    }
    cache.get(&node.id)
}

struct Drag {
    id: NodeId,
    gesture: String,
    pos: nodecraft_graph::Position,
}

/// A Shift-drag box select in progress: its world-space start and the selection it adds to.
/// Escape cancels it; a cancelled box keeps suppressing primary-drag panning until release.
struct Marquee {
    start: Pos2,
    base: BTreeSet<NodeId>,
    cancelled: bool,
}

/// Selection queries offered by a card's context menu.
enum SelectQuery {
    Upstream,
    Downstream,
    Kind(String),
}

/// A card's world rect: as painted this frame, or (for cards not drawn, e.g. off screen) from the
/// kind's static descriptor.
#[cfg(test)]
fn card_geometry(painted: &BTreeMap<NodeId, Rect>, kinds: &nodecraft_nodes::Registry, node: &nodecraft_graph::Node, t: &theme::Tokens) -> Rect {
    painted.get(&node.id).copied().unwrap_or_else(|| card::world_rect(node, kinds.get(&node.kind).ok().map(|kind| kind.descriptor()), t))
}

/// A card's world rect for box selection: as painted this frame, or for a card that wasn't drawn,
/// its resolved instance rect when the card could reach the box at all (the largest card at its
/// position). Cards that can't reach it skip the resolution.
fn box_geometry(painted: &BTreeMap<NodeId, Rect>, kinds: &nodecraft_nodes::Registry, node: &nodecraft_graph::Node, world: Rect, t: &theme::Tokens) -> Rect {
    if let Some(rect) = painted.get(&node.id) {
        return *rect;
    }
    let largest = Rect::from_min_size(Pos2::new(node.pos.x, node.pos.y), Vec2::new(t.card_width, t.card_max_height));
    if !largest.intersects(world) {
        return largest;
    }
    let resolved = kinds.get(&node.kind).ok().and_then(|kind| kind.resolve_instance(node).ok());
    card::world_rect(node, resolved.as_deref(), t)
}

mod navigation;

pub struct NodeCraftApp {
    pub theme: ThemeKind,
    pub view: CanvasView,
    pub session: Session,
    pub canvas: CanvasId,
    pub selection: Selection,
    pub canvas_prefs: CanvasSettings,
    pub navigation: Navigation,
    navigation_mailbox: Rc<RefCell<Option<serde_json::Value>>>,
    #[cfg(not(target_arch = "wasm32"))]
    store: Option<nodecraft_engine::settings::SettingsStore>,
    pub viewport: Rect,
    pub notice: Option<String>,
    editors: BTreeMap<NodeId, EditorState>,
    drag: Option<Drag>,
    marquee: Option<Marquee>,
    /// World rects of the cards painted this frame. Geometry for selection (and noodles) comes
    /// from here, not the evictable schema cache, so it matches what is on screen.
    painted: BTreeMap<NodeId, Rect>,
    /// egui drops focus on a bare Escape before the frame runs, so remember last frame's.
    text_focused: bool,
    schemas: BTreeMap<NodeId, CachedSchema>,
    cache_clock: u64,
    next_gesture: u64,
    centred: bool,
}
impl NodeCraftApp {
    pub fn new(ctx: &egui::Context) -> Result<Self, Error> {
        let mut app = Self::headless()?;
        app.apply_settings(ctx, &NodeCraftSettings::default());
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        Ok(app)
    }
    /// The app with settings from `store`, which it saves while running and flushes on exit.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_settings(ctx: &egui::Context, store: nodecraft_engine::settings::SettingsStore) -> Result<Self, Error> {
        let settings = store.get().clone();
        let mut app = Self::new(ctx)?;
        app.apply_settings(ctx, &settings);
        app.store = Some(store);
        Ok(app)
    }

    /// Takes the values the shell uses from `settings` and restyles `ctx` for the theme.
    pub fn apply_settings(&mut self, ctx: &egui::Context, settings: &NodeCraftSettings) {
        self.theme = match settings.appearance.theme {
            Theme::Dark => ThemeKind::Dark,
            Theme::Light => ThemeKind::Light,
        };
        self.canvas_prefs = settings.canvas.clone();
        theme::apply(ctx, self.theme);
    }

    pub fn headless() -> Result<Self, Error> {
        let kinds = nodecraft_nodes::Registry::builtins()?;
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Untitled")?;
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document)?;
        let mut values = BTreeMap::new();
        values.insert("text".into(), Value::Text("A quiet forest at dawn, soft light through the trees.".into()));
        let mut node = kinds.get("source.text")?.descriptor().instantiate(NodeId::from_bytes([3; 16]), &values)?;
        node.pos = nodecraft_graph::Position { x: -160.0, y: -110.0 };
        Change::AddNode { canvas, node }.apply(&mut document)?;
        let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins()?, kinds)?;
        Self::from_session(session, canvas)
    }
    pub fn from_session(mut session: Session, canvas: CanvasId) -> Result<Self, Error> {
        if !session.document().canvases().contains_key(&canvas) {
            return Err(Error::new(ErrorKind::NotFound, "canvas does not exist"));
        }
        let navigation_mailbox = Rc::new(RefCell::new(None));
        let mailbox = Rc::clone(&navigation_mailbox);
        session.register_host_service(nodecraft_engine::navigation_cmds::SERVICE, move |request| {
            let mut slot = mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "navigation host is busy"))?;
            if slot.is_some() {
                return Err(Error::new(ErrorKind::Conflict, "navigation request is already pending"));
            }
            *slot = Some(request.clone());
            Ok(json!({"accepted":true}))
        })?;
        Ok(Self {
            theme: ThemeKind::default(),
            view: CanvasView::default(),
            session,
            canvas,
            selection: Selection::default(),
            canvas_prefs: CanvasSettings::default(),
            navigation: Navigation::default(),
            navigation_mailbox,
            #[cfg(not(target_arch = "wasm32"))]
            store: None,
            viewport: Rect::NOTHING,
            notice: None,
            editors: BTreeMap::new(),
            drag: None,
            marquee: None,
            painted: BTreeMap::new(),
            text_focused: false,
            schemas: BTreeMap::new(),
            cache_clock: 0,
            next_gesture: 0,
            centred: false,
        })
    }
    pub fn home(&mut self) -> Pos2 {
        self.navigation.geometry.refresh(&self.session, self.canvas, self.theme);
        self.navigation.geometry.rects.values().copied().reduce(|a, b| a.union(b)).map_or(Pos2::ZERO, |r| r.center())
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.cache_clock = self.cache_clock.saturating_add(1);
        let t = self.theme.tokens();
        let viewport = ui.max_rect();
        self.viewport = viewport;
        if !self.centred {
            let home = self.home();
            self.view.reset(viewport, home);
            self.centred = true;
        }
        let selected_before = self.selection.clone();
        if !canvas::text_input_focused(ui.ctx()) {
            let command = ui.input_mut(|input| {
                self.session.registry().specs().filter(|spec| matches!(spec.id, "edit.undo" | "edit.redo")).find_map(|spec| {
                    spec.shortcut
                        .into_iter()
                        .chain(spec.alternate_shortcuts.iter().copied())
                        .any(|shortcut| consume_shortcut(input, shortcut))
                        .then_some(spec.id)
                })
            });
            if let Some(command) = command {
                if let Err(error) = self.session.dispatch(command, &json!({})) {
                    self.notice = Some(error.message);
                } else {
                    self.notice = None;
                    // Invalid drafts remain view state across unrelated undo/redo.
                }
            }
            let (select_all, escape) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::COMMAND, egui::Key::A), i.key_pressed(egui::Key::Escape)));
            if select_all && let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
                self.selection.replace(canvas.nodes().keys().copied().collect());
            }
            // Escape closes an open menu or leaves a text field first, then cancels a box
            // select, then clears.
            if escape && !self.text_focused && !egui::Popup::is_any_open(ui.ctx()) {
                match &mut self.marquee {
                    Some(marquee) if !marquee.cancelled => {
                        marquee.cancelled = true;
                        self.selection.replace(marquee.base.clone());
                    }
                    _ => self.selection.clear(),
                }
            }
        }
        let overlay_blocks = self.navigation_overlay(ui);
        let shift = ui.input(|i| i.modifiers.shift);
        let background = ui.interact(viewport, ui.id().with("canvas"), Sense::click_and_drag());
        if !overlay_blocks
            && background.drag_started_by(egui::PointerButton::Primary)
            && shift
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.marquee = Some(Marquee { start: self.view.to_world(viewport.min, origin), base: self.selection.nodes().clone(), cancelled: false });
        }
        let over_card = ui.input(|i| i.pointer.hover_pos()).is_some_and(|pointer| {
            self.session.document().canvases().get(&self.canvas).is_some_and(|c| {
                c.nodes().values().any(|node| {
                    let rect = card::world_rect(node, self.schemas.get(&node.id).map(|cached| cached.descriptor.as_ref()), t);
                    self.view.rect_to_screen(viewport.min, rect).contains(pointer)
                })
            })
        });
        let home = if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Num0)) { self.home() } else { Pos2::ZERO };
        let allow_scroll_pan = !over_card || self.view.zoom < card::BODY_MIN_ZOOM;
        if !overlay_blocks {
            canvas::navigate(ui, &background, &mut self.view, home, &self.canvas_prefs, allow_scroll_pan, self.marquee.is_none());
        }
        if !overlay_blocks && background.clicked() && !shift {
            self.selection.clear();
        }
        let painter = ui.painter_at(viewport);
        painter.rect_filled(viewport, 0.0, t.canvas_bg);
        canvas::paint_grid(&painter, viewport, &self.view, t);
        let mut intents = Vec::new();
        let mut pan = Vec2::ZERO;
        let mut query = None;
        if let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
            self.editors.retain(|id, _| canvas.nodes().contains_key(id));
            self.schemas.retain(|id, _| canvas.nodes().contains_key(id));
            self.selection.retain_existing(canvas);
            // Selected cards draw last, on top.
            let selected = self.selection.clone();
            self.painted.clear();
            for node in
                canvas.nodes().values().filter(|node| !selected.contains(node.id)).chain(canvas.nodes().values().filter(|node| selected.contains(node.id)))
            {
                // Cull conservatively before resolving potentially large instance schemas.
                let position = self.drag.as_ref().filter(|drag| drag.id == node.id).map_or(node.pos, |drag| drag.pos);
                let bounds = Rect::from_min_size(Pos2::new(position.x, position.y), Vec2::new(t.card_width, t.card_max_height));
                if !self.view.rect_to_screen(viewport.min, bounds).expand(card::cull_margin(t, self.view.zoom)).intersects(viewport) {
                    if let Some(editor) = self.editors.get_mut(&node.id) {
                        editor.mark_hidden();
                    }
                    continue;
                }
                let schema = cached_schema(&mut self.schemas, &self.session, node, self.cache_clock);
                let descriptor = schema.map(|cached| cached.descriptor.as_ref());
                let editor = self.editors.entry(node.id).or_default();
                if let Some(result) = card::show(
                    ui,
                    &self.view,
                    node,
                    card::CardView {
                        descriptor,
                        visible: schema.map(|cached| &cached.visible),
                        preview_pos: self.drag.as_ref().filter(|drag| drag.id == node.id).map(|drag| drag.pos),
                    },
                    editor,
                    self.selection.contains(node.id),
                    t,
                ) {
                    let r = result.response;
                    self.painted.insert(node.id, r.rect);
                    if !overlay_blocks && r.clicked() {
                        if shift {
                            self.selection.toggle(node.id);
                        } else {
                            self.selection.only(node.id);
                        }
                    } else if !overlay_blocks
                        && (r.drag_started_by(egui::PointerButton::Primary) || r.secondary_clicked() || r.long_touched())
                        && !self.selection.contains(node.id)
                    {
                        // Grabbing or right-clicking a selected card keeps the rest of the selection.
                        if shift {
                            self.selection.add(node.id);
                        } else {
                            self.selection.only(node.id);
                        }
                    }
                    if !overlay_blocks || egui::Popup::is_any_open(ui.ctx()) {
                        r.context_menu(|ui| {
                            if ui.button("Select upstream").clicked() {
                                query = Some(SelectQuery::Upstream);
                            }
                            if ui.button("Select downstream").clicked() {
                                query = Some(SelectQuery::Downstream);
                            }
                            let label = descriptor.map_or(node.kind.as_str(), |d| d.label.as_str());
                            if ui.button(format!("Select all {label} nodes")).clicked() {
                                query = Some(SelectQuery::Kind(node.kind.clone()));
                            }
                        });
                    }
                    if !overlay_blocks && r.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(serial) = self.next_gesture.checked_add(1) {
                            self.next_gesture = serial;
                            self.drag = Some(Drag { id: node.id, gesture: format!("canvas-drag-{serial}"), pos: node.pos });
                        } else {
                            self.notice = Some("Too many drag gestures; reopen the project to continue.".into());
                            self.drag = None;
                        }
                    }
                    if !overlay_blocks && (r.dragged_by(egui::PointerButton::Middle) || r.dragged_by(egui::PointerButton::Secondary)) {
                        pan += r.drag_delta() * self.view.zoom;
                    }
                    if !overlay_blocks || self.text_focused {
                        intents.extend(result.intents.into_iter().map(|intent| (node.id, intent)));
                    }
                }
            }
            match query {
                Some(SelectQuery::Upstream) => self.selection.replace(selection::upstream(canvas, self.selection.nodes())),
                Some(SelectQuery::Downstream) => self.selection.replace(selection::downstream(canvas, self.selection.nodes())),
                Some(SelectQuery::Kind(kind)) => self.selection.replace(selection::of_kind(canvas, &kind)),
                None => {}
            }
            if let Some(marquee) = &mut self.marquee
                && !marquee.cancelled
                && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            {
                // Nodes removed mid-drag (e.g. by undo) must not come back from the base.
                marquee.base.retain(|id| canvas.nodes().contains_key(id));
                let world = Rect::from_two_pos(marquee.start, self.view.to_world(viewport.min, pointer));
                let mut nodes = marquee.base.clone();
                let (painted, kinds) = (&self.painted, self.session.kinds());
                nodes.extend(selection::in_box(canvas, world, |node| box_geometry(painted, kinds, node, world, t)));
                self.selection.replace(nodes);
                // Above the card layers, which are drawn over the background painter.
                ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("marquee"))).with_clip_rect(viewport).rect(
                    self.view.rect_to_screen(viewport.min, world),
                    0.0,
                    t.marquee_fill,
                    egui::Stroke::new(1.0, t.accent),
                    egui::StrokeKind::Inside,
                );
            }
        } else {
            self.selection.clear();
        }
        // Cards were painted before this frame's selection changes; show them without waiting for input.
        if self.selection != selected_before {
            ui.ctx().request_repaint();
        }
        for (id, intent) in intents {
            let (command, params, editing) = match intent {
                Intent::Move(pos) => {
                    if let Some(drag) = &mut self.drag
                        && drag.id == id
                    {
                        drag.pos = pos;
                    }
                    continue;
                }
                Intent::Title(title) => ("node.title", json!({"canvas":self.canvas,"id":id,"title":title}), Some(EditTarget::Title)),
                Intent::Parameter { name, value } => {
                    ("node.param_set", json!({"canvas":self.canvas,"id":id,"name":name,"value":value}), Some(EditTarget::Parameter(name)))
                }
            };
            match self.session.dispatch(command, &params) {
                Ok(_) => {
                    self.notice = None;
                    if let Some(target) = editing
                        && let Some(editor) = self.editors.get_mut(&id)
                    {
                        editor.accepted(&target);
                    }
                }
                Err(error) => {
                    self.notice = Some(error.message.clone());
                    if let Some(target) = editing
                        && let Some(editor) = self.editors.get_mut(&id)
                    {
                        editor.failed(target, error.message);
                    }
                }
            }
        }
        if !ui.input(|i| i.pointer.primary_down())
            && let Some(drag) = self.drag.take()
            && let Err(error) = self.session.dispatch("node.move", &json!({"canvas":self.canvas,"id":drag.id,"pos":drag.pos,"gesture":drag.gesture}))
        {
            self.notice = Some(error.message);
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.marquee = None;
        }
        self.view.pan_by(pan);
        let hidden_draft_notice = self
            .editors
            .values()
            .any(|editor| editor.hidden_dirty)
            .then_some("Unsaved draft in a hidden node or field. Zoom or pan back to edit it, or discard drafts.");
        if let Some(notice) = self.notice.as_deref().or(hidden_draft_notice) {
            egui::Area::new(ui.id().with("notice"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(t.card_padding, -t.card_padding))
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding).show(ui, |ui| {
                        ui.label(notice);
                        if hidden_draft_notice.is_some() && ui.button("Discard drafts").clicked() {
                            self.editors.clear();
                        }
                    });
                });
        }
        self.navigation.finish_frame(ui.ctx());
        self.text_focused = canvas::text_input_focused(ui.ctx());
    }
}
fn consume_shortcut(input: &mut egui::InputState, shortcut: &str) -> bool {
    let mut modifiers = egui::Modifiers::NONE;
    let mut key = None;
    for part in shortcut.split('+') {
        match part {
            "Command" => modifiers |= egui::Modifiers::COMMAND,
            "Ctrl" => modifiers |= egui::Modifiers::CTRL,
            "Shift" => modifiers |= egui::Modifiers::SHIFT,
            "Alt" => modifiers |= egui::Modifiers::ALT,
            "Z" => key = Some(egui::Key::Z),
            "Y" => key = Some(egui::Key::Y),
            "1" => key = Some(egui::Key::Num1),
            "2" => key = Some(egui::Key::Num2),
            "M" => key = Some(egui::Key::M),
            _ => return false,
        }
    }
    key.is_some_and(|key| input.consume_key(modifiers, key))
}

impl eframe::App for NodeCraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store {
            let tick = store.tick(std::time::Instant::now());
            if let nodecraft_engine::settings::SaveStatus::Failed(e) = &tick.status {
                log::warn!("settings not saved, will retry: {e}");
            }
            for moved in store.take_moved_aside() {
                log::warn!("settings file was damaged and moved to {}", moved.display());
            }
            if let Some(deadline) = tick.next_deadline {
                ctx.request_repaint_after(deadline.saturating_duration_since(std::time::Instant::now()));
            }
        }
        // Last, so it sees any `CancelClose` from an unsaved-changes check (none in M0).
        craft_quit::on_close_request(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
    fn on_exit(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        // A read-only store (newer schema, unwritable folder) was reported at startup.
        if let Some(store) = &mut self.store
            && !store.is_read_only()
            && let Err(e) = store.flush()
        {
            log::warn!("settings not saved on exit: {e}");
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.theme.tokens().canvas_bg.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests;
