//! Lightweight actual-instance geometry and the view-only navigation overlay.
use crate::{
    canvas::{CanvasView, valid_rect},
    card,
    selection::Selection,
    theme::{ThemeKind, Tokens},
};
use egui::{Pos2, Rect, Vec2};
use nodecraft_engine::Session;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde_json::{Value, json};
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

// A private host resource, never serialized. Retaining its Arc in the cache keeps
// the previous allocation alive, so a replacement Session cannot reuse its identity.
pub(crate) struct SessionIdentity;
#[derive(Default)]
pub struct Geometry {
    key: Option<(ProjectId, CanvasId, u64, ThemeKind)>,
    session_identity: Option<Arc<SessionIdentity>>,
    pub rects: BTreeMap<NodeId, Rect>,
    /// Checked instance labels: at most256bytes per actual node; no descriptor bodies retained.
    pub labels: BTreeMap<NodeId, String>,
    pub error: bool,
    #[cfg(test)]
    validations: usize,
}
impl Geometry {
    pub(crate) fn identity(session: &mut Session) -> Arc<SessionIdentity> {
        let identity = session.swap_resource::<Arc<SessionIdentity>>(None).unwrap_or_else(|| Arc::new(SessionIdentity));
        session.swap_resource(Some(Arc::clone(&identity)));
        identity
    }
    pub fn refresh(&mut self, session: &mut Session, canvas: CanvasId, theme: ThemeKind) {
        let identity = Self::identity(session);
        let document = session.document();
        let Some(scene) = document.canvases().get(&canvas) else {
            self.rects.clear();
            self.labels.clear();
            self.key = None;
            self.error = true;
            return;
        };
        let key = (document.id(), canvas, session.revision(), theme);
        if self.key == Some(key) && self.session_identity.as_ref().is_some_and(|previous| Arc::ptr_eq(previous, &identity)) {
            return;
        }
        self.key = Some(key);
        self.session_identity = Some(identity);
        self.rects.clear();
        self.labels.clear();
        self.error = false;
        let mut validated = ValidatedDescriptors::default();
        for node in scene.nodes().values() {
            match instance_info(session, node, theme.tokens(), &mut validated) {
                Ok((rect, label)) => {
                    self.rects.insert(node.id, rect);
                    self.labels.insert(node.id, label);
                }
                Err(_) => {
                    self.error = true;
                    self.labels.insert(node.id, if crate::search::fallback_label(node) { "Unknown node".to_owned() } else { String::new() });
                }
            }
        }
        #[cfg(test)]
        {
            self.validations = validated.validations;
        }
    }
    pub fn bounds(&self, selection: Option<&Selection>, t: &Tokens) -> Option<Rect> {
        if self.error && selection.is_none() {
            return None;
        }
        if let Some(selection) = selection
            && (selection.is_empty() || selection.nodes().iter().any(|id| !self.rects.contains_key(id)))
        {
            return None;
        }
        self.rects.iter().filter(|(id, _)| selection.is_none_or(|s| s.contains(**id))).map(|(_, rect)| decorated(*rect, t)).reduce(|a, b| a.union(b))
    }
}
// References returned as Borrowed live in the immutably borrowed registry for this refresh.
// Owned descriptors may reuse allocations and therefore never enter this identity set.
#[derive(Default)]
struct ValidatedDescriptors<'a> {
    borrowed: BTreeMap<*const nodecraft_nodes::NodeDescriptor, (usize, nodecraft_types::ValidatedParamSchema<'a>)>,
    #[cfg(test)]
    validations: usize,
}
fn instance_info<'a>(
    session: &'a Session,
    node: &nodecraft_graph::Node,
    t: &Tokens,
    validated: &mut ValidatedDescriptors<'a>,
) -> Result<(Rect, String), Error> {
    node.validate_payload()?;
    // Match painting's bounded placeholder on every schema-resolution/admission failure.
    let placeholder = || {
        let rect = card::world_rect(node, None, t);
        if valid_rect(rect) {
            Ok((rect, if crate::search::fallback_label(node) { "Unknown node".to_owned() } else { String::new() }))
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "node placeholder geometry is unavailable"))
        }
    };
    let Ok(kind) = session.kinds().get(&node.kind) else {
        return placeholder();
    };
    let Ok(descriptor) = kind.instance(node) else {
        return placeholder();
    };
    if node.kind != descriptor.id || descriptor.id != kind.descriptor().id || node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
        return placeholder();
    }
    let admit = |bytes: usize, schema: &nodecraft_types::ValidatedParamSchema<'_>| {
        let Ok(resolved) = schema.resolve_borrowed(&node.params, false) else {
            return false;
        };
        let overhead = resolved.visible.iter().fold(256usize, |total, name| total.saturating_add(name.len().saturating_add(96)));
        bytes.saturating_add(overhead) <= crate::app::MAX_SCHEMA_CACHE_BYTES
    };
    let admitted = match &descriptor {
        Cow::Borrowed(value) => {
            let identity = std::ptr::from_ref(*value);
            if let std::collections::btree_map::Entry::Vacant(entry) = validated.borrowed.entry(identity) {
                if value.validate().is_err() {
                    return placeholder();
                }
                let (Some(bytes), Ok(schema)) = (crate::app::descriptor_bytes(value), value.params.validated()) else {
                    return placeholder();
                };
                entry.insert((bytes, schema));
                #[cfg(test)]
                {
                    validated.validations += 1;
                }
            }
            let Some((bytes, schema)) = validated.borrowed.get(&identity) else {
                return placeholder();
            };
            admit(*bytes, schema)
        }
        Cow::Owned(value) => {
            if value.validate().is_err() {
                return placeholder();
            }
            let (Some(bytes), Ok(schema)) = (crate::app::descriptor_bytes(value), value.params.validated()) else {
                return placeholder();
            };
            #[cfg(test)]
            {
                validated.validations += 1;
            }
            admit(bytes, &schema)
        }
    };
    if !admitted {
        return placeholder();
    }
    let rect = card::world_rect(node, Some(&descriptor), t);
    if !valid_rect(rect) {
        return Err(Error::new(ErrorKind::InvalidInput, "node geometry is unavailable"));
    }
    Ok((rect, if crate::search::fallback_label(node) { descriptor.label.clone() } else { String::new() }))
}
#[cfg(test)]
fn instance_rect<'a>(session: &'a Session, node: &nodecraft_graph::Node, t: &Tokens, validated: &mut ValidatedDescriptors<'a>) -> Result<Rect, Error> {
    instance_info(session, node, t, validated).map(|(rect, _)| rect)
}
/// Includes the painted title/unsaved label, body and handles at ordinary zoom. Additional
/// screen padding in fit accounts for minimum screen-sized text/handles at low zoom.
pub fn decorated(rect: Rect, t: &Tokens) -> Rect {
    rect.expand(t.handle_radius).union(Rect::from_min_max(rect.min - Vec2::new(0.0, t.label_size + t.label_gap + t.control_gap), rect.max))
}
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    pub domain: Rect,
    pub screen: Rect,
}
impl Mapping {
    pub fn new(domain: Rect, screen: Rect) -> Option<Self> {
        if !valid_rect(domain) || !valid_rect(screen) || screen.width() <= 0.0 || screen.height() <= 0.0 {
            return None;
        }
        let domain = if domain.width() < 1.0 || domain.height() < 1.0 { domain.expand(1.0) } else { domain };
        Some(Self { domain, screen })
    }
    fn scale(&self) -> f64 {
        (f64::from(self.screen.width()) / (f64::from(self.domain.max.x) - f64::from(self.domain.min.x)).max(1.0))
            .min(f64::from(self.screen.height()) / (f64::from(self.domain.max.y) - f64::from(self.domain.min.y)).max(1.0))
    }
    pub fn point(&self, world: Pos2) -> Pos2 {
        let scale = self.scale();
        let c = ((f64::from(self.domain.min.x) + f64::from(self.domain.max.x)) * 0.5, (f64::from(self.domain.min.y) + f64::from(self.domain.max.y)) * 0.5);
        let mid = self.screen.center();
        Pos2::new((f64::from(mid.x) + (f64::from(world.x) - c.0) * scale) as f32, (f64::from(mid.y) + (f64::from(world.y) - c.1) * scale) as f32)
    }
    pub fn world(&self, pointer: Pos2) -> (f64, f64) {
        let pointer = pointer.clamp(self.screen.min, self.screen.max);
        let c = ((f64::from(self.domain.min.x) + f64::from(self.domain.max.x)) * 0.5, (f64::from(self.domain.min.y) + f64::from(self.domain.max.y)) * 0.5);
        let mid = self.screen.center();
        let scale = self.scale();
        (c.0 + (f64::from(pointer.x) - f64::from(mid.x)) / scale, c.1 + (f64::from(pointer.y) - f64::from(mid.y)) / scale)
    }
    pub fn rect(&self, world: Rect) -> Rect {
        Rect::from_min_max(self.point(world.min), self.point(world.max))
    }
}
#[derive(Clone, Copy)]
pub struct Layout {
    pub overlay: Rect,
    pub map: Rect,
    pub toolbar: Rect,
}
pub fn layout(viewport: Rect, visible: bool, t: &Tokens) -> Layout {
    let width = t.navigation_width.min((viewport.width() - 2.0 * t.navigation_margin).max(1.0));
    let map_height = if visible {
        t.minimap_height.min((viewport.height() - t.navigation_toolbar_height - 2.0 * t.navigation_margin - t.navigation_gap).max(1.0))
    } else {
        0.0
    };
    let height = t.navigation_toolbar_height + if visible { map_height + t.navigation_gap } else { 0.0 };
    let overlay = Rect::from_min_size(viewport.max - Vec2::new(width + t.navigation_margin, height + t.navigation_margin), Vec2::new(width, height));
    let toolbar = Rect::from_min_size(Pos2::new(overlay.min.x, overlay.max.y - t.navigation_toolbar_height), Vec2::new(width, t.navigation_toolbar_height));
    Layout { overlay, map: Rect::from_min_size(overlay.min, Vec2::new(width, map_height)), toolbar }
}
pub struct Navigation {
    pub visible: bool,
    pub geometry: Geometry,
    pub drag: Option<Mapping>,
    pub pointer_owned: bool,
}
impl Default for Navigation {
    fn default() -> Self {
        Self { visible: true, geometry: Geometry::default(), drag: None, pointer_owned: false }
    }
}
pub struct OverlayInput<'a> {
    pub view: CanvasView,
    pub viewport: Rect,
    pub selection: &'a Selection,
    pub all_enabled: bool,
    pub selection_enabled: bool,
    pub toggle_enabled: bool,
    pub pan_enabled: bool,
    pub preview: Option<(NodeId, Vec2)>,
    pub canvas_gesture: bool,
    pub tokens: &'a Tokens,
}
impl Navigation {
    pub fn usable(&self, viewport: Rect, t: &Tokens) -> Rect {
        let top = layout(viewport, self.visible, t).overlay.top();
        Rect::from_min_max(viewport.min, Pos2::new(viewport.max.x, top - t.navigation_gap))
    }
    pub fn show(&mut self, ui: &mut egui::Ui, input: OverlayInput<'_>) -> (bool, Vec<(&'static str, Value)>) {
        let OverlayInput { view, viewport, selection, all_enabled, selection_enabled, toggle_enabled, pan_enabled, preview, canvas_gesture, tokens: t } = input;
        let layout = layout(viewport, self.visible, t);
        let mut actions = Vec::new();
        let visible = view.visible_world(viewport);
        let node_bounds = self.geometry.bounds(None, t);
        let mut domain = node_bounds.map_or(visible, |nodes| if valid_rect(visible) { nodes.union(visible) } else { nodes });
        if let Some((id, delta)) = preview
            && let Some(body) = self.geometry.rects.get(&id)
        {
            domain = domain.union(decorated(body.translate(delta), t));
        }
        let mapping = self.drag.or_else(|| Mapping::new(domain, layout.map.shrink(t.navigation_gap)));
        let pointer = ui.input(|i| i.pointer.interact_pos());
        if self.visible && pan_enabled && self.drag.is_none() && ui.input(|i| i.pointer.primary_pressed()) && pointer.is_some_and(|p| layout.map.contains(p)) {
            self.drag = mapping;
            self.pointer_owned = self.drag.is_some();
        }
        if self.pointer_owned
            && let (Some(mapping), Some(pointer)) = (self.drag, pointer)
        {
            let (x, y) = mapping.world(pointer);
            actions.push(("canvas.pan_to", json!({"x":x,"y":y})));
        }
        let blocked = self.pointer_owned || (!canvas_gesture && pointer.is_some_and(|p| layout.overlay.contains(p)));
        egui::Area::new(ui.id().with("navigation_overlay")).order(egui::Order::Foreground).movable(false).fixed_pos(layout.overlay.min).constrain(false).show(
            ui.ctx(),
            |ui| {
                ui.set_min_size(layout.overlay.size());
                let painter = ui.painter_at(layout.overlay.intersect(viewport));
                painter.rect(layout.overlay, t.panel_radius, t.panel_fill, egui::Stroke::new(t.handle_stroke, t.panel_stroke), egui::StrokeKind::Inside);
                if self.visible {
                    let response =
                        ui.interact(layout.map, ui.id().with("minimap"), if pan_enabled { egui::Sense::click_and_drag() } else { egui::Sense::hover() });
                    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, pan_enabled, "Minimap: click or drag to pan canvas"));
                    if let Some(mapping) = mapping {
                        for (id, body) in &self.geometry.rects {
                            let body = if let Some((preview_id, delta)) = preview
                                && preview_id == *id
                            {
                                body.translate(delta)
                            } else {
                                *body
                            };
                            let mut rect = mapping.rect(body);
                            if rect.width() < t.minimap_node_min {
                                rect = rect.expand2(Vec2::new(t.minimap_node_min * 0.5, 0.0));
                            }
                            if rect.height() < t.minimap_node_min {
                                rect = rect.expand2(Vec2::new(0.0, t.minimap_node_min * 0.5));
                            }
                            painter.rect_filled(rect.intersect(layout.map), t.panel_radius, if selection.contains(*id) { t.accent } else { t.text_muted });
                        }
                        if valid_rect(visible) {
                            painter.rect(
                                mapping.rect(visible).intersect(layout.map),
                                0.0,
                                t.marquee_fill,
                                egui::Stroke::new(t.handle_stroke, t.accent),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                    if self.geometry.error {
                        painter.text(
                            layout.map.center(),
                            egui::Align2::CENTER_CENTER,
                            "Some node geometry is unavailable",
                            egui::FontId::proportional(t.body_size),
                            t.text,
                        );
                    }
                }
                let width = layout.toolbar.width() / 4.0;
                for (index, (label, id, enabled)) in [
                    (if self.visible { "Hide map" } else { "Show map" }, "canvas.minimap_toggle", toggle_enabled),
                    ("Fit all", "canvas.fit_all", all_enabled),
                    ("Fit selected", "canvas.fit_selection", selection_enabled),
                    ("100%", "reset", toggle_enabled),
                ]
                .into_iter()
                .enumerate()
                {
                    let rect = Rect::from_min_size(layout.toolbar.min + Vec2::new(index as f32 * width, 0.0), Vec2::new(width, layout.toolbar.height()));
                    let label = if id == "reset" { format!("{:.0}%", view.zoom * 100.0) } else { label.to_owned() };
                    let response = ui.add_enabled_ui(enabled, |ui| ui.put(rect, egui::Button::new(label).frame(false))).inner;
                    let clicked = response.clicked();
                    if clicked {
                        actions.push((id, json!({})));
                    }
                }
            },
        );
        (blocked, actions)
    }
    pub fn finish_frame(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.pointer.primary_down()) {
            self.drag = None;
            self.pointer_owned = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Node, Project};
    use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{ParamControl, ParamSchema, ParamSpec, PortType, Value};
    use std::{
        borrow::Cow,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Dynamic(NodeDescriptor, NodeDescriptor, Arc<AtomicUsize>);
    impl NodeKind for Dynamic {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            self.2.fetch_add(1, Ordering::Relaxed);
            if node.config.get("test.unavailable") == Some(&Value::Bool(true)) {
                return Err(Error::new(ErrorKind::Unsupported, "instance unavailable"));
            }
            if node.config.get("test.owned") == Some(&Value::Bool(true)) { Ok(Cow::Owned(self.1.clone())) } else { Ok(Cow::Borrowed(&self.1)) }
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![])
        }
    }
    #[test]
    fn offscreen_geometry_resolves_instances_once_per_revision_without_static_fallback() {
        let short = NodeDescriptor {
            id: "test.dynamic".into(),
            label: "Dynamic".into(),
            category: Category::Source,
            inputs: vec![],
            outputs: vec![],
            params: ParamSchema { params: vec![] },
            quick_add: None,
        };
        let mut tall = NodeDescriptor {
            params: ParamSchema {
                params: (0..8)
                    .map(|n| ParamSpec {
                        name: format!("p{n}"),
                        label: format!("P{n}"),
                        description: None,
                        control: ParamControl::Toggle,
                        required: false,
                        default: Some(Value::Bool(false)),
                        visible_if: None,
                    })
                    .collect(),
            },
            ..short.clone()
        };
        tall.params.params.push(ParamSpec {
            name: "large".into(),
            label: "Large".into(),
            description: None,
            control: ParamControl::Text { multiline: true, max_bytes: 1_048_576 },
            required: false,
            default: Some(Value::Text("x".repeat(600_000))),
            visible_if: None,
        });
        tall.params.params.push(ParamSpec {
            name: "choice".into(),
            label: "Choice".into(),
            description: None,
            control: ParamControl::Choice { options: (0..16_384).map(|n| nodecraft_types::Choice { value: n.to_string(), label: n.to_string() }).collect() },
            required: false,
            default: Some(Value::Text("0".into())),
            visible_if: None,
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(Dynamic(short.clone(), tall, calls.clone()))).unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Geometry").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let mut node = Node::new(NodeId::from_bytes([3; 16]), "test.dynamic");
        node.pos.x = 100_000.0;
        node.params.insert("p0".into(), Value::Bool(true));
        Change::AddNode { canvas, node: node.clone() }.apply(&mut document).unwrap();
        for n in 10..1009_u16 {
            let mut other = node.clone();
            let mut bytes = [0; 16];
            bytes[..2].copy_from_slice(&n.to_le_bytes());
            other.id = NodeId::from_bytes(bytes);
            Change::AddNode { canvas, node: other }.apply(&mut document).unwrap();
        }
        let mut session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let mut navigation = Navigation { visible: false, ..Navigation::default() };
        let geometry = &mut navigation.geometry;
        let before = calls.load(Ordering::Relaxed);
        geometry.refresh(&mut session, canvas, ThemeKind::Dark);
        assert_eq!(geometry.validations, 1, "one validated descriptor proof shared by all nonempty instances");
        assert_eq!(calls.load(Ordering::Relaxed) - before, 1000, "each nonempty instance hook still runs");
        let actual = geometry.rects[&node.id];
        assert!(actual.height() > card::world_rect(&node, Some(&short), ThemeKind::Dark.tokens()).height());
        let count = calls.load(Ordering::Relaxed);
        geometry.refresh(&mut session, canvas, ThemeKind::Dark);
        assert_eq!(calls.load(Ordering::Relaxed), count);
        session.dispatch("node.move", &json!({"canvas":canvas,"id":node.id,"pos":{"x":200_000.0,"y":20.0}})).unwrap();
        geometry.refresh(&mut session, canvas, ThemeKind::Dark);
        assert_eq!(calls.load(Ordering::Relaxed) - count, 1000);
        assert_eq!(geometry.validations, 1);
        assert_eq!(geometry.rects[&node.id].min.x, 200_000.0);
        assert_eq!(geometry.rects[&node.id].height(), actual.height());
        let mut owned = node.clone();
        owned.config.insert("test.owned".into(), Value::Bool(true));
        let mut validated = ValidatedDescriptors::default();
        for _ in 0..2 {
            instance_info(&session, &owned, ThemeKind::Dark.tokens(), &mut validated).unwrap();
        }
        assert_eq!(validated.validations, 2, "owned descriptor allocations never share validation");
        geometry.refresh(&mut session, CanvasId::from_bytes([9; 16]), ThemeKind::Dark);
        assert!(geometry.bounds(None, ThemeKind::Dark.tokens()).is_none());
    }
    #[test]
    fn invalid_unavailable_and_cache_rejected_schemas_match_painted_placeholder() {
        let short = NodeDescriptor {
            id: "test.dynamic".into(),
            label: "Dynamic".into(),
            category: Category::Source,
            inputs: vec![],
            outputs: vec![],
            params: ParamSchema::default(),
            quick_add: None,
        };
        for failure in ["invalid", "params", "budget", "ports", "unavailable"] {
            let mut instance = short.clone();
            if failure == "invalid" {
                instance.label.clear();
            }
            if failure == "budget" {
                instance.params.params.push(ParamSpec {
                    name: "escaped".into(),
                    label: "Escaped".into(),
                    description: None,
                    control: ParamControl::Text { multiline: true, max_bytes: 1_048_576 },
                    required: false,
                    default: Some(Value::Text("\0".repeat(900_000))),
                    visible_if: None,
                });
                instance.validate().unwrap();
                assert!(crate::app::descriptor_bytes(&instance).is_none(), "valid escaped text exceeds view cache admission budget");
            }
            let mut node = Node::new(NodeId::from_bytes([31; 16]), "test.dynamic");
            if failure == "params" {
                node.params.insert("unknown".into(), Value::Bool(true));
            }
            if failure == "ports" {
                node.outputs
                    .push(nodecraft_graph::Port { name: "unexpected".into(), port_type: PortType { modality: nodecraft_types::Modality::Text, many: false } });
            }
            if failure == "unavailable" {
                node.config.insert("test.unavailable".into(), Value::Bool(true));
            }
            let mut kinds = nodecraft_nodes::Registry::default();
            kinds.register(Box::new(Dynamic(short.clone(), instance, Arc::new(AtomicUsize::new(0))))).unwrap();
            let document = Project::new(ProjectId::from_bytes([32; 16]), "Fallback").unwrap();
            let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
            assert_eq!(
                instance_rect(&session, &node, ThemeKind::Dark.tokens(), &mut ValidatedDescriptors::default()).unwrap(),
                card::world_rect(&node, None, ThemeKind::Dark.tokens()),
                "{failure}"
            );
        }
    }
    #[test]
    fn replacement_session_retains_identity_and_invalidates_equal_document_keys() {
        let canvas = CanvasId::from_bytes([11; 16]);
        let id = NodeId::from_bytes([12; 16]);
        let mut document = Project::new(ProjectId::from_bytes([13; 16]), "Replacement").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let node = Node::new(id, "unknown.kind");
        Change::AddNode { canvas, node: node.clone() }.apply(&mut document).unwrap();
        let mut session = Session::new(document.clone()).unwrap();
        let mut geometry = Geometry::default();
        geometry.refresh(&mut session, canvas, ThemeKind::Dark);
        let old = Arc::downgrade(geometry.session_identity.as_ref().unwrap());
        let before = geometry.rects[&id];
        let mut moved = node;
        moved.pos.x = 4000.0;
        Change::SetNode { canvas, node: moved }.apply(&mut document).unwrap();
        session = Session::new(document).unwrap();
        assert!(old.upgrade().is_some(), "cache retains old allocation after Session replacement");
        geometry.refresh(&mut session, canvas, ThemeKind::Dark);
        assert_eq!(geometry.rects[&id].min.x - before.min.x, 4000.0);
        assert!(old.upgrade().is_none());
    }
    #[test]
    fn unavailable_instances_use_complete_placeholder_and_nonfinite_bounds_fail() {
        let canvas = CanvasId::from_bytes([21; 16]);
        let id = NodeId::from_bytes([22; 16]);
        let mut document = Project::new(ProjectId::from_bytes([23; 16]), "Placeholder").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let node = Node::new(id, "unknown.kind");
        Change::AddNode { canvas, node: node.clone() }.apply(&mut document).unwrap();
        let session = Session::new(document).unwrap();
        let t = ThemeKind::Dark.tokens();
        let mut validated = ValidatedDescriptors::default();
        let rect = instance_rect(&session, &node, t, &mut validated).unwrap();
        assert_eq!(rect, card::world_rect(&node, None, t));
        assert!(decorated(rect, t).contains_rect(rect));
        let mut bad = node;
        bad.pos.x = f32::NAN;
        assert!(instance_rect(&session, &bad, t, &mut validated).is_err());
        let geometry = Geometry { rects: [(id, rect)].into(), error: true, ..Geometry::default() };
        assert!(geometry.bounds(None, t).is_none(), "never fit a partial graph");
        let mut missing = Selection::default();
        missing.only(NodeId::from_bytes([24; 16]));
        assert!(geometry.bounds(Some(&missing), t).is_none());
    }
    #[test]
    fn mapping_roundtrips_extreme_domains_and_clamps_drag() {
        let domain = Rect::from_min_max(Pos2::new(-f32::MAX, -f32::MAX), Pos2::new(f32::MAX, f32::MAX));
        let screen = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(300.0, 150.0));
        let mapping = Mapping::new(domain, screen).unwrap();
        assert_eq!(mapping.point(Pos2::ZERO), screen.center());
        let (x, y) = mapping.world(screen.center());
        assert_eq!((x, y), (0.0, 0.0));
        assert_eq!(mapping.world(screen.max + Vec2::splat(100.0)), mapping.world(screen.max));
        assert!(Mapping::new(Rect::NOTHING, screen).is_none());
        assert!(Mapping::new(domain, Rect::ZERO).is_none());
    }
}
