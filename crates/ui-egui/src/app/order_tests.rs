//! Input order badges (#64), driven with real pointer events.
use super::tests::wire_drag;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const HUB: NodeId = NodeId::from_bytes([9; 16]);

fn edge(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

/// Three text sources stacked on the left, feeding a router's list input in order 1, 2, 3.
fn harness() -> Harness<'static, NodeCraftApp> {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Order").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let mut hub = kinds.get(nodecraft_nodes::router::ID).unwrap().instantiate_instance(HUB, Default::default(), &Default::default()).unwrap();
    hub.pos = nodecraft_graph::Position { x: 700.0, y: 250.0 };
    Change::AddNode { canvas, node: hub }.apply(&mut document).unwrap();
    for n in 1..=3u8 {
        let id = NodeId::from_bytes([n; 16]);
        let mut node = kinds.get("source.text").unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
        node.pos = nodecraft_graph::Position { x: 0.0, y: f32::from(n - 1) * 300.0 };
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
        let wire = nodecraft_graph::Edge {
            id: edge(n),
            from: Endpoint { node: id, port: "out".into() },
            to: Endpoint { node: HUB, port: "in".into() },
            order: u32::from(n) - 1,
        };
        Change::Connect { canvas, edge: wire }.apply(&mut document).unwrap();
    }
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.state_mut().view = CanvasView { pan: Vec2::new(80.0, 60.0), zoom: 0.6 };
    h.run();
    h
}

fn order(h: &Harness<'static, NodeCraftApp>) -> Vec<EdgeId> {
    let app = h.state();
    let mut edges: Vec<_> = app.session.document().canvases().get(&app.canvas).unwrap().edges().values().cloned().collect();
    edges.sort_by_key(|e| e.order);
    edges.into_iter().map(|e| e.id).collect()
}

#[test]
fn dragging_a_badge_onto_another_moves_it_there_in_one_undo_step() {
    let mut h = harness();
    for n in 1..=3 {
        assert!(h.query_by_label(&format!("Input {n} of 3 into in of Router, drag to reorder")).is_some(), "badge {n}");
    }
    let third = h.get_by_label("Input 3 of 3 into in of Router, drag to reorder").rect().center();
    let first = h.get_by_label("Input 1 of 3 into in of Router, drag to reorder").rect().center();
    wire_drag(&mut h, third, first);
    h.run();
    assert_eq!(order(&h), [edge(3), edge(1), edge(2)]);
    assert!(h.query_by_label("Input 1 of 3 into in of Router, drag to reorder").is_some(), "renumbered");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(order(&h), [edge(1), edge(2), edge(3)], "one undo restores the order");
}

#[test]
fn dropping_a_badge_back_on_its_own_place_changes_nothing() {
    let mut h = harness();
    let second = h.get_by_label("Input 2 of 3 into in of Router, drag to reorder").rect().center();
    wire_drag(&mut h, second, second + Vec2::new(2.0, 2.0));
    assert_eq!(order(&h), [edge(1), edge(2), edge(3)]);
    assert!(!h.state().session.can_undo());
    assert!(h.state().notice.is_none());
}

#[test]
fn a_single_noodle_or_a_far_zoom_shows_no_badges() {
    let mut h = harness();
    h.state_mut().view.zoom = 0.3;
    h.run();
    assert!(h.query_by_label("Input 1 of 3 into in of Router, drag to reorder").is_none(), "too small to read");
    h.state_mut().view.zoom = 0.6;
    let canvas = h.state().canvas;
    for n in [2u8, 3] {
        h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas": canvas, "id": edge(n)})).unwrap();
    }
    h.run();
    assert!(h.query_by_label("Input 1 of 1 into in of Router, drag to reorder").is_none(), "one noodle has no order to show");
}

#[test]
fn a_release_off_screen_still_ends_the_drag_and_other_buttons_pan() {
    let mut h = harness();
    let first = h.get_by_label("Input 1 of 3 into in of Router, drag to reorder").rect().center();
    h.hover_at(first);
    h.run();
    h.drag_at(first);
    h.run();
    h.hover_at(first + Vec2::new(0.0, 30.0));
    h.run();
    assert!(h.state().reordering.is_some());
    // The view moves so far that the badge's own spot is off screen, then the button is released.
    h.state_mut().view.pan += Vec2::new(-5000.0, 0.0);
    h.run();
    h.drop_at(first + Vec2::new(0.0, 30.0));
    h.run();
    assert!(h.state().reordering.is_none(), "the drag ends");
    h.state_mut().view.pan += Vec2::new(5000.0, 0.0);
    h.run();
    assert_eq!(order(&h), [edge(1), edge(2), edge(3)]);
    // Middle/right drags starting on a badge pan, as ones on a card do.
    for button in [egui::PointerButton::Middle, egui::PointerButton::Secondary] {
        let badge = h.get_by_label("Input 2 of 3 into in of Router, drag to reorder").rect().center();
        let pan = h.state().view.pan;
        h.hover_at(badge);
        h.event(egui::Event::PointerButton { pos: badge, button, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        h.hover_at(badge + Vec2::new(40.0, 0.0));
        h.run();
        h.event(egui::Event::PointerButton { pos: badge + Vec2::new(40.0, 0.0), button, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run();
        assert!((h.state().view.pan - pan - Vec2::new(40.0, 0.0)).length() < 1.0, "{:?}", h.state().view.pan - pan);
        assert_eq!(order(&h), [edge(1), edge(2), edge(3)]);
    }
}

/// Establish ownership through the badge's real pointer response, without publishing any edit.
fn begin_third_badge_drag(h: &mut Harness<'static, NodeCraftApp>) -> Pos2 {
    let third = h.get_by_label("Input 3 of 3 into in of Router, drag to reorder").rect().center();
    let first = h.get_by_label("Input 1 of 3 into in of Router, drag to reorder").rect().center();
    h.hover_at(third);
    h.run();
    h.drag_at(third);
    h.run();
    h.hover_at(first);
    h.run();
    assert!(h.state().reordering.is_some(), "real badge owns the held primary drag");
    assert_eq!(h.state().session.revision(), 0);
    assert!(!h.state().session.can_undo());
    first
}
#[test]
fn issue333_escape_then_release_cancels_reordering_and_preserves_selection() {
    let mut h = harness();
    h.state_mut().selection.only(NodeId::from_bytes([1; 16]));
    h.state_mut().selection.add(HUB);
    let before = h.state().session.document().clone();
    let selection = h.state().selection.clone();
    let first = begin_third_badge_drag(&mut h);
    h.key_press(egui::Key::Escape);
    h.run();
    let cancelled = h.state().reordering.is_none();
    // Always execute the release before assertions: the old code can still publish here.
    h.drop_at(first);
    h.run();
    assert_eq!(h.state().session.document(), &before, "release after Escape must not reorder");
    assert!(cancelled, "Escape releases retained reorder ownership");
    assert_eq!(order(&h), [edge(1), edge(2), edge(3)]);
    assert_eq!(h.state().selection, selection);
    assert_eq!(h.state().session.revision(), 0);
    assert!(!h.state().session.can_undo() && !h.state().session.can_redo());
    assert!(h.state().reordering.is_none());
}
#[test]
fn issue333_quick_add_waits_for_badge_drag_and_resumes_after_release() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let first = begin_third_badge_drag(&mut h);
    h.key_press(egui::Key::T);
    h.run();
    assert_eq!(h.state().session.document(), &before, "quick-add must yield to the active badge drag");
    assert_eq!(h.state().session.revision(), 0);
    assert!(!h.state().session.can_undo());
    h.key_press(egui::Key::Escape);
    h.run();
    h.drop_at(first);
    h.run();
    assert!(h.state().reordering.is_none());
    h.key_press(egui::Key::T);
    h.run();
    let canvas = h.state().canvas;
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), before.canvases().get(&canvas).unwrap().nodes().len() + 1);
    assert_eq!(h.state().session.revision(), 1, "quick-add resumes after ownership ends");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}
