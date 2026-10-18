//! "Turn into" (#60): drop a new noodle on empty canvas, pick a kind, get the node and its
//! connection in one undo step. Driven through real pointer and key events.
use super::tests::{chain_harness_with, chain_id, links, port_point, wire_drag};
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_nodes::{JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};

/// A test kind whose mode takes anything, as a generator's would.
struct Accepting(NodeDescriptor);
impl NodeKind for Accepting {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<ModeId> {
        ModeId::new("test").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Err(Error::new(ErrorKind::Unsupported, "test-only kind"))
    }
}

fn chain_harness() -> Harness<'static, NodeCraftApp> {
    chain_harness_with(|descriptor| Box::new(Accepting(descriptor)))
}

const EMPTY: Pos2 = Pos2::new(880.0, 560.0);

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}

fn type_key(h: &mut Harness<'static, NodeCraftApp>, key: egui::Key, text: &str) {
    h.key_press(key);
    h.event(egui::Event::Text(text.into()));
    h.run();
}

/// Drops node 1's output on empty canvas; the menu opens offering every kind that takes text.
fn open_menu(h: &mut Harness<'static, NodeCraftApp>) {
    let from = port_point(h, 1, noodle::Side::Output);
    wire_drag(h, from, EMPTY);
    h.run();
    assert!(h.state().turn_into.is_some(), "a new noodle dropped on empty canvas opens the menu");
    for label in ["Turn into Router", "Turn into Collect", "Turn into Relay"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    assert!(h.query_by_label("Turn into Text").is_none(), "sources take no inputs");
}

#[test]
fn a_kinds_key_creates_it_where_the_noodle_dropped_connected_in_one_undo_step() {
    let mut h = chain_harness();
    let before = h.state().session.document().clone();
    let count = nodes(&h).len();
    open_menu(&mut h);
    assert_eq!(h.state().session.document(), &before, "opening the menu changes nothing");
    type_key(&mut h, egui::Key::R, "r");
    assert!(h.state().turn_into.is_none());
    let all = nodes(&h);
    assert_eq!(all.len(), count + 1);
    let router = all.iter().find(|n| n.kind == nodecraft_nodes::router::ID).unwrap();
    assert!(links(&h).contains(&(chain_id(1), router.id)));
    let dropped = h.state().view.to_world(h.state().viewport.min, EMPTY);
    assert!((router.pos.x - dropped.x).abs() < 1.0, "its input side sits where the noodle dropped");
    assert_eq!(h.state().selection.nodes(), &[router.id].into_iter().collect());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo removes the node and its noodle");
}

#[test]
fn arrows_and_enter_pick_and_a_list_input_takes_the_noodle() {
    let mut h = chain_harness();
    open_menu(&mut h);
    h.key_press(egui::Key::ArrowDown);
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    let collect = nodes(&h).into_iter().find(|n| n.kind == "test.collect").expect("the second row");
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert!(canvas.edges().values().any(|e| e.from.node == chain_id(1) && e.to == Endpoint { node: collect.id, port: "items".into() }));
}

#[test]
fn escape_or_a_click_elsewhere_closes_it_and_canvas_keys_wait_while_it_is_open() {
    let mut h = chain_harness();
    let before = h.state().session.document().clone();
    open_menu(&mut h);
    // T is the Text source's quick-add key, and no entry here uses it: nothing happens.
    type_key(&mut h, egui::Key::T, "t");
    assert!(h.state().turn_into.is_some());
    assert_eq!(h.state().session.document(), &before);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().turn_into.is_none());
    assert_eq!(h.state().session.document(), &before);
    open_menu(&mut h);
    h.hover_at(Pos2::new(60.0, 60.0));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: Pos2::new(60.0, 60.0), button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    assert!(h.state().turn_into.is_none());
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn a_picked_up_noodle_dropped_on_empty_canvas_still_just_disconnects() {
    let mut h = chain_harness();
    let to = port_point(&h, 2, noodle::Side::Input);
    wire_drag(&mut h, to, EMPTY);
    assert!(h.state().turn_into.is_none());
    assert!(!links(&h).contains(&(chain_id(1), chain_id(2))));
}
