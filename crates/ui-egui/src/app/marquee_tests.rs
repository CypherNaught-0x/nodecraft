use super::*;
use egui_kittest::Harness;
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Tall(NodeDescriptor, Arc<AtomicUsize>);
impl NodeKind for Tall {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn instance<'a>(&'a self, _: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(Cow::Borrowed(&self.0))
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
fn fixture(failure: &str) -> (Harness<'static, NodeCraftApp>, Arc<AtomicUsize>, Rect) {
    let params = (0..8)
        .map(|i| ParamSpec {
            name: format!("p{i}"),
            label: format!("P{i}"),
            description: None,
            control: ParamControl::Toggle,
            required: false,
            default: Some(Value::Bool(false)),
            visible_if: None,
        })
        .collect();
    let mut descriptor = NodeDescriptor {
        id: "test.marquee".into(),
        label: "Marquee".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![],
        params: ParamSchema { params },
        quick_add: None,
    };
    if failure == "budget" {
        descriptor.params.params.push(ParamSpec {
            name: "large".into(),
            label: "Large".into(),
            description: None,
            control: ParamControl::Text { multiline: true, max_bytes: 1_048_576 },
            required: false,
            default: Some(Value::Text("\0".repeat(900_000))),
            visible_if: None,
        });
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let id = NodeId::from_bytes([3; 16]);
    let mut node = Node::new(id, &descriptor.id);
    if failure == "ports" {
        node.outputs.push(nodecraft_graph::Port { name: "unexpected".into(), port_type: PortType { modality: Modality::Text, many: false } });
    }
    if failure == "params" {
        node.params.insert("p0".into(), Value::Text("invalid".into()));
    }
    let tall = card::world_rect(&node, Some(&descriptor), ThemeKind::Dark.tokens());
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Marquee").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(Tall(descriptor, Arc::clone(&calls)))).unwrap();
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.run();
    let placeholder = card::world_rect(h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap(), None, h.state().theme.tokens());
    let top = placeholder.max.y + 120.0;
    h.state_mut().view = CanvasView { pan: Vec2::new(300.0, -top), zoom: 1.0 };
    h.run();
    if failure != "valid" {
        assert!(h.state().painted.is_empty(), "placeholder must be culled");
    }
    calls.store(0, Ordering::SeqCst);
    (h, calls, tall)
}
fn hold(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, to: Pos2) {
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.hover_at(from);
    h.step();
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
    h.step();
    h.hover_at(to);
    h.step();
}
#[test]
fn malformed_undrawn_tall_cards_use_renderer_placeholder() {
    for failure in ["ports", "params"] {
        let (mut h, _, tall) = fixture(failure);
        let before = h.state().session.document().clone();
        let v = h.state().view;
        let o = h.state().viewport.min;
        let from = v.to_screen(o, Pos2::new(tall.min.x - 10.0, -v.pan.y + 15.0));
        let to = v.to_screen(o, Pos2::new(tall.max.x + 10.0, -v.pan.y + 45.0));
        assert!(tall.intersects(Rect::from_two_pos(v.to_world(o, from), v.to_world(o, to))));
        hold(&mut h, from, to);
        assert!(h.state().selection.is_empty(), "{failure}: empty space below placeholder selected malformed card");
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
    }
}
#[test]
fn repeated_undrawn_admission_is_reused_during_marquee() {
    let (mut h, calls, tall) = fixture("params");
    let v = h.state().view;
    let o = h.state().viewport.min;
    hold(&mut h, v.to_screen(o, Pos2::new(tall.min.x - 10.0, -v.pan.y + 15.0)), v.to_screen(o, Pos2::new(tall.max.x + 10.0, -v.pan.y + 45.0)));
    assert!(h.state().marquee.is_some());
    assert!(calls.load(Ordering::SeqCst) > 0);
    calls.store(0, Ordering::SeqCst);
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "same admitted candidate should not invoke instance hook again");
}

#[test]
fn borrowed_admission_matches_renderer_for_valid_and_refused_cards() {
    for failure in ["valid", "ports", "params", "budget"] {
        let (h, _, _) = fixture(failure);
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().next().unwrap();
        let mut schemas = SchemaCache::default();
        let renderer = cached_schema(&mut schemas, &app.session, node, 1);
        assert_eq!(renderer.is_some(), failure == "valid", "{failure}");
        let expected = card::world_rect(node, renderer.map(|s| s.descriptor.as_ref()), app.theme.tokens());
        let mut geometry = admission::GeometryCache::default();
        assert_eq!(geometry.rect(&app.session, node, app.theme.tokens()), expected, "{failure}");
        assert_eq!(schemas.bytes, cache_bytes(&schemas));
    }
}
#[test]
fn gesture_cache_identity_theme_revision_capacity_and_release() {
    let (mut h, calls, _) = fixture("valid");
    let app = h.state_mut();
    let mut geometry = admission::GeometryCache::default();
    geometry.prepare(&mut app.session, app.canvas, app.theme);
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().next().unwrap().clone();
    let tall = geometry.rect(&app.session, &node, app.theme.tokens());
    calls.store(0, Ordering::SeqCst);
    for _ in 0..10 {
        assert_eq!(geometry.rect(&app.session, &node, app.theme.tokens()), tall);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    geometry.prepare(&mut app.session, app.canvas, ThemeKind::Light);
    geometry.rect(&app.session, &node, app.theme.tokens());
    assert!(calls.load(Ordering::SeqCst) > 0);
    app.session.dispatch("node.move", &json!({"canvas":app.canvas,"id":node.id,"pos":{"x":80.0,"y":0.0}})).unwrap();
    geometry.prepare(&mut app.session, app.canvas, app.theme);
    let moved = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&node.id).unwrap();
    assert_eq!(geometry.rect(&app.session, moved, app.theme.tokens()).min.x, 80.0);
    // Same project IDs and revision, but another Session/kind registry: old proof cannot survive.
    let document = app.session.document().clone();
    app.session = Session::new(document).unwrap();
    assert!(geometry.prepare(&mut app.session, app.canvas, app.theme));
    assert_eq!(geometry.rect(&app.session, &node, app.theme.tokens()), card::world_rect(&node, None, app.theme.tokens()));
    // Capacity exhaustion computes correct transient geometry rather than a static/tall guess.
    for n in 0u32..4100 {
        let mut candidate = node.clone();
        let mut id = [0u8; 16];
        id[..4].copy_from_slice(&n.to_le_bytes());
        candidate.id = NodeId::from_bytes(id);
        assert_eq!(geometry.rect(&app.session, &candidate, app.theme.tokens()), card::world_rect(&candidate, None, app.theme.tokens()));
    }
    assert!(geometry.len() <= 4095);
    assert!(geometry.retained_bytes() <= 512 * 1024);
    geometry.clear();
    assert_eq!(geometry.len(), 0);
    assert_eq!(geometry.retained_bytes(), 0);
}

#[test]
fn marquee_release_and_escape_release_retained_geometry() {
    for cancel in [false, true] {
        let (mut h, _, tall) = fixture("params");
        let v = h.state().view;
        let o = h.state().viewport.min;
        let from = v.to_screen(o, Pos2::new(tall.min.x - 10.0, -v.pan.y + 15.0));
        let to = v.to_screen(o, Pos2::new(tall.max.x + 10.0, -v.pan.y + 45.0));
        hold(&mut h, from, to);
        assert!(h.state().marquee_geometry.len() > 0);
        if cancel {
            h.key_press(egui::Key::Escape);
            h.step();
        } else {
            h.drop_at(to);
            h.step();
        }
        assert_eq!(h.state().marquee_geometry.retained_bytes(), 0);
        assert!(!h.state().session.can_undo());
    }
}

#[test]
fn capacity_overflow_still_admits_valid_dynamic_rectangles() {
    let (h, calls, tall) = fixture("valid");
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().next().unwrap();
    let mut cache = admission::GeometryCache::default();
    calls.store(0, Ordering::SeqCst);
    for n in 0u32..4100 {
        let mut candidate = node.clone();
        let mut id = [0u8; 16];
        id[..4].copy_from_slice(&n.to_le_bytes());
        candidate.id = NodeId::from_bytes(id);
        assert_eq!(cache.rect(&app.session, &candidate, app.theme.tokens()), tall);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4100);
    assert_eq!(cache.len(), 4095);
    assert!(cache.retained_bytes() <= 512 * 1024);
}

#[test]
fn raw_undo_and_marquee_release_uses_current_revision_geometry() {
    let (mut h, _, _) = fixture("params");
    h.state_mut().view = CanvasView { pan: Vec2::new(300.0, 180.0), zoom: 1.0 };
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([3; 16]);
    let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
    let original = payload_rect(&h.state().session, node, h.state().theme.tokens());
    h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":id,"pos":{"x":0.0,"y":100.0}})).unwrap();
    h.step();
    let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
    let lower = payload_rect(&h.state().session, node, h.state().theme.tokens());
    // The malformed instance uses the actual compact placeholder, not its declared tall schema.
    let held_y = (original.bottom() + lower.bottom()) * 0.5;
    assert!(held_y > original.bottom() && held_y < lower.bottom());
    let from = h.state().view.to_screen(h.state().viewport.min, Pos2::new(lower.left() - 10.0, held_y));
    h.input_mut().events.extend([
        egui::Event::ModifiersChanged(egui::Modifiers::SHIFT),
        egui::Event::PointerMoved(from),
        egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT },
    ]);
    h.step();
    h.input_mut().events.push(egui::Event::PointerMoved(from - Vec2::new(20.0, 0.0)));
    h.step();
    assert!(h.state().marquee.is_some());
    // Pan leaves the start in world space and the candidate offscreen, as during a held gesture.
    h.state_mut().view.pan.x = -1300.0;
    let end_y = held_y + (lower.bottom() - held_y) * 0.25;
    let to = Pos2::new(50.0, h.state().view.to_screen(h.state().viewport.min, Pos2::new(0.0, end_y)).y);
    // The start is held in world space across the pan, not reinterpreted through the new camera.
    let held = Rect::from_two_pos(Pos2::new(lower.left() - 10.0, held_y), h.state().view.to_world(h.state().viewport.min, to));
    assert!(held.intersects(lower) && !held.intersects(original));
    h.input_mut().events.push(egui::Event::PointerMoved(to));
    h.step();
    assert!(h.state().selection.contains(id), "lower position must intersect the actual held box");
    assert!(!h.state().painted.contains_key(&id));
    assert!(h.state().marquee_geometry.len() > 0);
    h.input_mut().events.extend([
        egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
        egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
        egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::COMMAND },
    ]);
    h.step();
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().pos.y, 0.0);
    assert!(h.state().selection.is_empty(), "Undo and release must not select using the cached lower rectangle");
    assert!(h.state().marquee.is_none());
}

#[test]
fn raw_redo_and_release_invalidates_cached_placeholder_before_painting() {
    let (mut h, _, tall) = fixture("params");
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([3; 16]);
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":id,"name":"p0","value":Value::Bool(false)})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.step();
    let v = h.state().view;
    let o = h.state().viewport.min;
    let from = v.to_screen(o, Pos2::new(tall.min.x - 10.0, -v.pan.y + 15.0));
    let to = v.to_screen(o, Pos2::new(tall.max.x + 10.0, -v.pan.y + 45.0));
    hold(&mut h, from, to);
    assert!(h.state().marquee_geometry.placeholder(id));
    assert!(!h.state().painted.contains_key(&id));
    assert!(h.state().selection.is_empty());
    let modifiers = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
    h.input_mut().events.extend([
        egui::Event::ModifiersChanged(modifiers),
        egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers },
        egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers },
    ]);
    h.step();
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().params.get("p0"), Some(&Value::Bool(false)));
    assert!(h.state().painted.contains_key(&id), "restored valid tall card must not reuse the old placeholder verdict");
    assert!(h.state().selection.contains(id), "selection must use the restored painted geometry");
}

#[test]
fn ascending_geometry_entries_grow_in_batches_and_release_capacity() {
    let (h, _, tall) = fixture("valid");
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().next().unwrap();
    let mut cache = admission::GeometryCache::default();
    let mut previous_bytes = 0;
    let mut growths = 0;
    for n in 0u32..512 {
        let mut candidate = node.clone();
        let mut id = [0u8; 16];
        id.get_mut(..4).unwrap().copy_from_slice(&n.to_be_bytes());
        candidate.id = NodeId::from_bytes(id);
        assert_eq!(cache.rect(&app.session, &candidate, app.theme.tokens()), tall);
        let bytes = cache.retained_bytes();
        if bytes != previous_bytes {
            growths += 1;
            previous_bytes = bytes;
        }
        assert_eq!(cache.len(), usize::try_from(n).unwrap() + 1);
        assert!(bytes <= 512 * 1024);
    }
    assert!(growths <= 10, "ascending entries requested {growths} separate capacity increases");
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.retained_bytes(), 0);
}
