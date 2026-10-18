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
        assert!(h.query_by_label(&format!("Input {n} of 3, drag to reorder")).is_some(), "badge {n}");
    }
    let third = h.get_by_label("Input 3 of 3, drag to reorder").rect().center();
    let first = h.get_by_label("Input 1 of 3, drag to reorder").rect().center();
    wire_drag(&mut h, third, first);
    h.run();
    assert_eq!(order(&h), [edge(3), edge(1), edge(2)]);
    assert!(h.query_by_label("Input 1 of 3, drag to reorder").is_some(), "renumbered");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(order(&h), [edge(1), edge(2), edge(3)], "one undo restores the order");
}

#[test]
fn dropping_a_badge_back_on_its_own_place_changes_nothing() {
    let mut h = harness();
    let second = h.get_by_label("Input 2 of 3, drag to reorder").rect().center();
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
    assert!(h.query_by_label("Input 1 of 3, drag to reorder").is_none(), "too small to read");
    h.state_mut().view.zoom = 0.6;
    let canvas = h.state().canvas;
    for n in [2u8, 3] {
        h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas": canvas, "id": edge(n)})).unwrap();
    }
    h.run();
    assert!(h.query_by_label("Input 1 of 1, drag to reorder").is_none(), "one noodle has no order to show");
}
