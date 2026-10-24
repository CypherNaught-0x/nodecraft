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

/// Empty canvas: clear of the cards and of the minimap and search overlays.
const EMPTY: Pos2 = Pos2::new(520.0, 380.0);

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}

/// Real backends deliver `Event::Key { pressed: true }` AND `Event::Text(ch)` in ONE raw-input
/// frame, so a single keypress arrives as both. kittest runs one frame per queued `event()`, which
/// would let the menu consume the Key in one frame and quick-add see the Text in the next, hiding
/// #267's double-create. Inject the down-press and its text together in one frame, then release in
/// the next (the release is never the double-create; the co-arriving down-press is).
fn type_key(h: &mut Harness<'static, NodeCraftApp>, key: egui::Key, text: &str) {
    h.input_mut().events.extend([
        egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Text(text.into()),
    ]);
    h.step();
    h.event(egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

/// Drops node 1's output on empty canvas; the menu opens offering every kind that takes text.
fn open_menu(h: &mut Harness<'static, NodeCraftApp>) {
    let from = port_point(h, 1, noodle::Side::Output);
    wire_drag(h, from, EMPTY);
    h.run();
    assert!(h.state().turn_into.is_some(), "a new noodle dropped on empty canvas opens the menu");
    for label in ["Turn into Batch", "Turn into Router", "Turn into Collect", "Turn into Relay"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    assert!(h.query_by_label("Turn into Text").is_none(), "sources take no inputs");
}

/// #267: the Turn-into menu's entry keys ARE the kinds' quick-add letters, and a real backend
/// delivers `Event::Key { pressed: true }` and `Event::Text(ch)` in ONE raw-input frame. So the
/// frame that picks a row must not also let end-of-frame quick-add see the same press: before the
/// ownership freeze was widened to the menu, one R created the menu's node AND a second one at the
/// pointer (nodes 4→6, revision +2). Discriminating: it fails on the unfixed head.
#[test]
fn one_keypress_with_the_menu_open_creates_the_picked_node_only() {
    let mut h = chain_harness();
    let count = nodes(&h).len();
    open_menu(&mut h);
    let before = h.state().session.document().clone();
    let revision = h.state().session.revision();
    // Move the pointer onto bare canvas, clear of the menu itself: over the menu, quick-add yields
    // to the floating Area (#267's other half) and the double-create can't be seen at all. Only a
    // press outside closes the menu, so this hover leaves it open.
    let at = Pos2::new(520.0, 620.0);
    h.hover_at(at);
    h.step();
    assert_eq!(h.ctx.input(|i| i.pointer.hover_pos()), Some(at), "the pointer is over bare canvas");
    let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    let menu = egui::AreaState::load(&h.ctx, egui::Id::new("turn_into_menu")).map(|area| area.rect()).unwrap_or(Rect::NOTHING);
    assert!(!map.contains(at) && !menu.contains(at), "fixture: {at:?} must be bare canvas, menu {menu:?}, minimap {map:?}");
    assert!(h.state().turn_into.is_some(), "and the menu is still open");
    // One frame, both events: `input_mut` because `Harness::event` would run a frame each.
    h.input_mut().events.extend([
        egui::Event::Key { key: egui::Key::R, physical_key: Some(egui::Key::R), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Text("r".into()),
    ]);
    h.step();
    assert!(h.state().turn_into.is_none(), "the menu took the letter and closed");
    let all = nodes(&h);
    let revisions = h.state().session.revision() - revision;
    assert_eq!(
        (all.len(), revisions),
        (count + 1, 1),
        "one keypress creates one node in one revision (nodes {count}→{}, revisions +{}); \
         the unfixed frame fires quick-add as well",
        all.len(),
        revisions,
    );
    let routers: Vec<_> = all.iter().filter(|n| n.kind == nodecraft_nodes::router::ID).collect();
    assert_eq!(routers.len(), 1, "quick-add at the pointer did not also fire");
    let router = routers[0];
    assert_eq!(h.state().selection.nodes(), &[router.id].into_iter().collect(), "selection is the menu's node");
    assert!(links(&h).contains(&(chain_id(1), router.id)), "it is the fed node");
    h.event(egui::Event::Key { key: egui::Key::R, physical_key: Some(egui::Key::R), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo removes the node and its noodle");
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
    // Batch, Router, then Collect.
    for _ in 0..2 {
        h.key_press(egui::Key::ArrowDown);
        h.run();
    }
    h.key_press(egui::Key::Enter);
    h.run();
    let collect = nodes(&h).into_iter().find(|n| n.kind == "test.collect").expect("the third row");
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
fn only_empty_canvas_opens_it_not_a_card_or_an_overlay() {
    let mut h = chain_harness();
    let from = port_point(&h, 1, noodle::Side::Output);
    // The middle of a card (node 4, a source with no input to snap to).
    let card = {
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&chain_id(4)).unwrap();
        app.view.rect_to_screen(app.viewport.min, *app.painted.get(&node.id).unwrap()).center()
    };
    // The minimap, in the bottom-right corner.
    let minimap = h.state().viewport.max - Vec2::new(120.0, 120.0);
    for (to, place) in [(card, "a card"), (minimap, "the minimap")] {
        wire_drag(&mut h, from, to);
        h.run();
        assert!(h.state().turn_into.is_none(), "dropping on {place} doesn't open the menu");
    }
}

#[test]
fn a_click_on_a_row_picks_it() {
    let mut h = chain_harness();
    open_menu(&mut h);
    h.get_by_label("Turn into Relay").click();
    h.run();
    assert!(nodes(&h).iter().any(|n| n.kind == "test.relay" && links(&h).contains(&(chain_id(1), n.id))));
}

#[test]
fn search_waits_while_it_is_open_so_letters_stay_picks() {
    let mut h = chain_harness();
    open_menu(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert!(!h.state().search.open, "search can't take focus from under the menu");
    type_key(&mut h, egui::Key::R, "r");
    assert!(nodes(&h).iter().any(|n| n.kind == nodecraft_nodes::router::ID), "so R still picks the router");
    assert_eq!(h.state().search.draft, "", "and nothing was typed into search");
}

#[test]
fn a_picked_up_noodle_dropped_on_empty_canvas_still_just_disconnects() {
    let mut h = chain_harness();
    let to = port_point(&h, 2, noodle::Side::Input);
    wire_drag(&mut h, to, EMPTY);
    assert!(h.state().turn_into.is_none());
    assert!(!links(&h).contains(&(chain_id(1), chain_id(2))));
}

#[test]
fn retained_menu_owns_clipboard_delete_and_history_keys_after_minimap_integration() {
    for redo in [false, true] {
        let mut h = chain_harness();
        let canvas = h.state().canvas;
        h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":chain_id(4),"pos":{"x":900.0,"y":400.0}})).unwrap();
        if redo {
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        }
        h.state_mut().selection.only(chain_id(1));
        h.run();
        open_menu(&mut h);
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let selection = h.state().selection.clone();
        let history = (h.state().session.can_undo(), h.state().session.can_redo());
        let events = [
            egui::Event::Copy,
            egui::Event::Cut,
            egui::Event::Paste("Turn into ownership sentinel".into()),
            egui::Event::Key { key: egui::Key::D, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
            egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
            egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: if redo { egui::Modifiers::COMMAND | egui::Modifiers::SHIFT } else { egui::Modifiers::COMMAND },
            },
        ];
        for event in events {
            h.event(event);
            h.run();
            assert!(h.state().turn_into.is_some());
            assert_eq!(h.state().session.document(), &before);
            assert_eq!(h.state().session.revision(), revision);
            assert_eq!(h.state().selection, selection);
            assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
            assert!(!h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(_))));
        }
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().turn_into.is_none());
    }
}
