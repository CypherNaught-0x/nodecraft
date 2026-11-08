//! Connecting, rewiring and disconnecting cards with the keyboard alone (#1109).
use super::tests::TestKind;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_nodes::{Category, NodeDescriptor};
use nodecraft_types::{ParamSchema, ProjectId};

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const ALPHA: NodeId = NodeId::from_bytes([92; 16]);
const BETA: NodeId = NodeId::from_bytes([93; 16]);
const RELAY: NodeId = NodeId::from_bytes([94; 16]);
const SECOND: NodeId = NodeId::from_bytes([95; 16]);
const WIRED: EdgeId = EdgeId::from_bytes([96; 16]);

/// Two text sources on the left, two text relays on the right; `wired` connects Alpha to Relay.
fn harness(wired: bool) -> Harness<'static, NodeCraftApp> {
    let text = PortType { modality: Modality::Text, many: false };
    let port = |name: &str| nodecraft_graph::Port { name: name.into(), port_type: text };
    let relay = NodeDescriptor {
        id: "test.relay".into(),
        label: "Relay".into(),
        category: Category::Action,
        inputs: vec![port("in")],
        outputs: vec![port("out")],
        params: ParamSchema { params: vec![] },
        quick_add: None,
    };
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(TestKind(relay))).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([90; 16]), "Keys").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind, title, x, y) in [
        (ALPHA, "source.text", "Alpha", 0.0, 0.0),
        (BETA, "source.text", "Beta", 0.0, 400.0),
        (RELAY, "test.relay", "Relay", 450.0, 0.0),
        (SECOND, "test.relay", "Second", 450.0, 400.0),
    ] {
        let mut node = kinds.get(kind).unwrap().descriptor().instantiate(id, &BTreeMap::new()).unwrap();
        node.pos = nodecraft_graph::Position { x, y };
        node.title = title.into();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    if wired {
        let edge =
            nodecraft_graph::Edge { id: WIRED, from: Endpoint { node: ALPHA, port: "out".into() }, to: Endpoint { node: RELAY, port: "in".into() }, order: 0 };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, C).unwrap());
    h.state_mut().view = CanvasView { pan: Vec2::new(100.0, 100.0), zoom: 0.3 };
    h.run();
    h
}

fn edges(h: &Harness<'static, NodeCraftApp>) -> BTreeSet<(NodeId, NodeId)> {
    h.state().session.document().canvases().get(&C).unwrap().edges().values().map(|e| (e.from.node, e.to.node)).collect()
}

fn focused(h: &Harness<'static, NodeCraftApp>) -> Option<egui::Id> {
    h.ctx.memory(|m| m.focused())
}

fn port(h: &Harness<'static, NodeCraftApp>, node: NodeId, side: Side) -> egui::Id {
    let name = match side {
        Side::Input => "in",
        Side::Output => "out",
    };
    h.state().port_widgets.id_of(side, &Endpoint { node, port: name.into() }).unwrap()
}

/// Press Tab until the port has keyboard focus.
fn tab_to(h: &mut Harness<'static, NodeCraftApp>, node: NodeId, side: Side) {
    for _ in 0..400 {
        if focused(h) == Some(port(h, node, side)) {
            return;
        }
        h.key_press(egui::Key::Tab);
        h.run();
    }
    panic!("Tab never reached the port");
}

fn press(h: &mut Harness<'static, NodeCraftApp>, key: egui::Key) {
    h.key_press(key);
    h.run();
}

#[test]
fn enter_connects_a_focused_output_to_the_chosen_input_in_one_undo_step() {
    let mut h = harness(false);
    let before = h.state().session.document().clone();
    tab_to(&mut h, ALPHA, Side::Output);
    press(&mut h, egui::Key::Enter);
    // Focus moves to the first input that would accept it, and the noodle is pending.
    assert!(h.state().key_wiring.is_some());
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    // Arrows and Tab cycle the compatible inputs, wrapping, and never move egui's own focus.
    press(&mut h, egui::Key::ArrowDown);
    assert_eq!(focused(&h), Some(port(&h, SECOND, Side::Input)));
    press(&mut h, egui::Key::Tab);
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    h.run();
    assert_eq!(focused(&h), Some(port(&h, SECOND, Side::Input)));
    assert!(edges(&h).is_empty(), "choosing changes nothing");
    press(&mut h, egui::Key::Enter);
    assert!(h.state().key_wiring.is_none());
    assert_eq!(edges(&h), [(ALPHA, SECOND)].into_iter().collect(), "notice: {:?}", h.state().notice);
    // Focus returns to where the connection started.
    assert_eq!(focused(&h), Some(port(&h, ALPHA, Side::Output)));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo(), "one changeset per connect");
}

#[test]
fn space_starts_from_an_unconnected_input_and_connects_an_upstream_output() {
    let mut h = harness(false);
    tab_to(&mut h, SECOND, Side::Input);
    press(&mut h, egui::Key::Space);
    // Outputs in reading order: Alpha, Beta, then Relay.
    assert_eq!(focused(&h), Some(port(&h, ALPHA, Side::Output)));
    press(&mut h, egui::Key::ArrowRight);
    assert_eq!(focused(&h), Some(port(&h, BETA, Side::Output)));
    press(&mut h, egui::Key::Space);
    assert_eq!(edges(&h), [(BETA, SECOND)].into_iter().collect(), "notice: {:?}", h.state().notice);
}

#[test]
fn a_connected_input_reroutes_its_noodle_and_shift_delete_disconnects_it() {
    let mut h = harness(true);
    let before = h.state().session.document().clone();
    // Picking up the noodle starts on the input it holds now.
    tab_to(&mut h, RELAY, Side::Input);
    press(&mut h, egui::Key::Enter);
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    press(&mut h, egui::Key::ArrowDown);
    assert_eq!(focused(&h), Some(port(&h, SECOND, Side::Input)));
    press(&mut h, egui::Key::Enter);
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    assert_eq!(canvas.edges().len(), 1);
    assert_eq!(canvas.edges().get(&WIRED).unwrap().to.node, SECOND, "the same edge moved: edge.rewire");
    let rewired = h.state().session.document().clone();
    // A second output into an occupied single input replaces its noodle in one step.
    tab_to(&mut h, BETA, Side::Output);
    press(&mut h, egui::Key::Enter);
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    press(&mut h, egui::Key::ArrowDown);
    press(&mut h, egui::Key::Enter);
    assert_eq!(edges(&h), [(BETA, SECOND)].into_iter().collect(), "notice: {:?}", h.state().notice);
    let replaced = h.state().session.document().clone();
    // Shift+Delete on the focused input removes its connection.
    tab_to(&mut h, SECOND, Side::Input);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Delete);
    h.run();
    assert!(edges(&h).is_empty());
    assert_eq!(h.state().session.document().canvases().get(&C).unwrap().nodes().len(), 4, "no card was deleted");
    // Undo restores each step in turn.
    for expected in [&replaced, &rewired, &before] {
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document(), expected);
    }
    assert!(!h.state().session.can_undo());
}

#[test]
fn escape_cancels_a_pending_connection_and_nothing_else() {
    let mut h = harness(true);
    let before = h.state().session.document().clone();
    h.state_mut().selection.only_edge(WIRED);
    h.run();
    tab_to(&mut h, BETA, Side::Output);
    press(&mut h, egui::Key::Enter);
    assert!(h.state().key_wiring.is_some());
    press(&mut h, egui::Key::Escape);
    assert!(h.state().key_wiring.is_none());
    assert_eq!(h.state().session.document(), &before);
    // The canvas's own Escape (clear the selection) did not also run, and focus went home.
    assert!(h.state().selection.contains_edge(WIRED), "Escape was consumed by the connection");
    assert_eq!(focused(&h), Some(port(&h, BETA, Side::Output)));
    // The next Escape belongs to the canvas again.
    press(&mut h, egui::Key::Escape);
    assert!(!h.state().selection.contains_edge(WIRED));
}

#[test]
fn a_pointer_press_ends_a_keyboard_connection_without_editing() {
    let mut h = harness(false);
    let before = h.state().session.document().clone();
    tab_to(&mut h, ALPHA, Side::Output);
    press(&mut h, egui::Key::Enter);
    assert!(h.state().key_wiring.is_some());
    let empty = h.state().viewport.right_bottom() - Vec2::splat(20.0);
    h.hover_at(empty);
    h.run();
    h.drag_at(empty);
    h.run();
    h.drop_at(empty);
    h.run();
    assert!(h.state().key_wiring.is_none());
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn ports_and_connections_have_accessible_names() {
    let mut h = harness(true);
    tab_to(&mut h, BETA, Side::Output);
    press(&mut h, egui::Key::Enter);
    press(&mut h, egui::Key::Enter);
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    assert_eq!(canvas.edges().len(), 1, "Beta replaced Alpha into Relay");
    tab_to(&mut h, ALPHA, Side::Output);
    press(&mut h, egui::Key::Enter);
    press(&mut h, egui::Key::ArrowDown);
    press(&mut h, egui::Key::Enter);
    assert_eq!(edges(&h), [(BETA, RELAY), (ALPHA, SECOND)].into_iter().collect());
    for label in ["Text from Beta to Relay · text", "Text from Alpha to Second · text"] {
        assert!(h.query_by_label(label).is_some(), "edge {label}");
    }
    assert_eq!(h.query_all_by_label_contains(" from ").count(), 2, "every edge, once");
    assert!(h.query_by_role_and_label(Role::Button, "Text output of Beta, connected to Relay").is_some());
    assert!(h.query_by_role_and_label(Role::Button, "Text input of Second, connected to Alpha").is_some());
    assert!(h.query_by_role_and_label(Role::Button, "Text output of Relay").is_some());
}

/// After a flow ends without Escape or Enter, the port left with focus answers Tab again.
fn tab_moves_focus_off(h: &mut Harness<'static, NodeCraftApp>) {
    let before = focused(h);
    assert!(before.is_some(), "a port keeps focus");
    press(h, egui::Key::Tab);
    assert_ne!(focused(h), before, "Tab is not trapped on the port");
}

#[test]
fn panning_with_a_drag_ends_the_flow_and_leaves_tab_working() {
    let mut h = harness(false);
    tab_to(&mut h, ALPHA, Side::Output);
    press(&mut h, egui::Key::Enter);
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    let pan = h.state().view.pan;
    let start = h.state().viewport.center() + Vec2::new(0.0, 240.0);
    h.hover_at(start);
    h.run();
    h.drag_at(start);
    h.run();
    h.hover_at(start + Vec2::new(-60.0, -30.0));
    h.run();
    h.drop_at(start + Vec2::new(-60.0, -30.0));
    h.run();
    assert_ne!(h.state().view.pan, pan, "the drag panned the canvas");
    assert!(h.state().key_wiring.is_none());
    assert!(edges(&h).is_empty());
    // The drag left focus where it was; the port's keys work again.
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    tab_moves_focus_off(&mut h);
}

#[test]
fn undo_removing_the_anchor_ends_the_flow_and_leaves_tab_and_escape_working() {
    let mut h = harness(false);
    let added = NodeId::from_bytes([97; 16]);
    h.state_mut().session.dispatch("node.create", &json!({"canvas": C, "id": added, "kind": "source.text", "pos": {"x": 0, "y": 800}})).unwrap();
    h.state_mut().view = CanvasView { pan: Vec2::new(100.0, 60.0), zoom: 0.3 };
    h.run();
    tab_to(&mut h, added, Side::Output);
    press(&mut h, egui::Key::Enter);
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(!h.state().session.document().canvases().get(&C).unwrap().nodes().contains_key(&added), "undo removed the anchor");
    assert!(h.state().key_wiring.is_none());
    assert_eq!(focused(&h), Some(port(&h, RELAY, Side::Input)));
    // Escape drops focus as everywhere else, and Tab then moves it again.
    press(&mut h, egui::Key::Escape);
    assert_eq!(focused(&h), None, "Escape is not swallowed by a stale lock");
    tab_to(&mut h, RELAY, Side::Input);
    tab_moves_focus_off(&mut h);
}
