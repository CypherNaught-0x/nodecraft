use super::*;
use egui_kittest::Harness;

fn key(kind: usize, pressed: bool) -> egui::Event {
    let (key, modifiers) = match kind {
        0 => (egui::Key::Delete, egui::Modifiers::NONE),
        1 => (egui::Key::Backspace, egui::Modifiers::NONE),
        2 => (egui::Key::Z, egui::Modifiers::COMMAND),
        3 => (egui::Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT),
        4 => (egui::Key::Y, egui::Modifiers::CTRL),
        _ => panic!("unknown fixture key"),
    };
    egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }
}
fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}
fn raw(h: &mut Harness<'static, NodeCraftApp>, events: Vec<egui::Event>) {
    h.input_mut().events.extend(events);
    h.step();
}
fn fixture(kind: usize) -> Harness<'static, NodeCraftApp> {
    let mut h = super::tests::harness();
    let canvas = h.state().canvas;
    let id = *h.state().session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    for title in ["First public title", "Second public title"] {
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":title})).unwrap();
    }
    if kind >= 3 {
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert!(h.state().session.can_redo());
    }
    h.state_mut().selection.only(id);
    h.run();
    h
}
fn map(h: &Harness<'static, NodeCraftApp>) -> Rect {
    crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map
}
fn press(h: &mut Harness<'static, NodeCraftApp>) {
    let at = map(h).center();
    raw(h, vec![egui::Event::PointerMoved(at), pointer(at, true)]);
    assert!(h.state().navigation.pointer_owned);
    assert!(h.state().navigation.drag.is_some());
}
fn owned_mutation(kind: usize, release_frame: bool) {
    let mut h = fixture(kind);
    press(&mut h);
    let outside = Pos2::new(80.0, 120.0);
    assert!(!map(&h).contains(outside));
    raw(&mut h, vec![egui::Event::PointerMoved(outside)]);
    assert!(h.state().navigation.pointer_owned);
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let events = h.state().session.events_since(0).unwrap();
    let selection = h.state().selection.clone();
    let mut input = vec![key(kind, true)];
    if release_frame {
        input.push(pointer(outside, false));
    }
    raw(&mut h, input);
    assert_eq!(h.state().session.document(), &document, "held mutation {kind}, release={release_frame}");
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!(h.state().selection, selection);
    assert!(h.ctx.input(|i| i.events.iter().any(|event| event == &key(kind, true))), "blocked mutation key stays unconsumed");
    if !release_frame {
        raw(&mut h, vec![pointer(outside, false)]);
    }
    assert!(!h.state().navigation.pointer_owned);
    assert!(h.state().navigation.drag.is_none());
    raw(&mut h, vec![key(kind, false)]);
    raw(&mut h, vec![key(kind, true)]);
    assert_eq!(h.state().session.revision(), revision + 1, "fresh released shortcut acts");
    assert_ne!(h.state().session.document(), &document);
    if kind <= 1 {
        assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().is_empty());
    }
    h.state_mut().session.dispatch(if kind == 2 { "edit.redo" } else { "edit.undo" }, &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &document, "normal inverse restores the document");
}
#[test]
fn held_minimap_owns_delete() {
    owned_mutation(0, false);
}
#[test]
fn held_minimap_owns_backspace() {
    owned_mutation(1, false);
}
#[test]
fn held_minimap_owns_undo() {
    owned_mutation(2, false);
}
#[test]
fn held_minimap_owns_shift_redo() {
    owned_mutation(3, false);
}
#[test]
fn held_minimap_owns_ctrl_y_redo() {
    owned_mutation(4, false);
}
#[test]
fn release_frame_minimap_owns_delete() {
    owned_mutation(0, true);
}
#[test]
fn release_frame_minimap_owns_backspace() {
    owned_mutation(1, true);
}
#[test]
fn release_frame_minimap_owns_undo() {
    owned_mutation(2, true);
}
#[test]
fn release_frame_minimap_owns_shift_redo() {
    owned_mutation(3, true);
}
#[test]
fn release_frame_minimap_owns_ctrl_y_redo() {
    owned_mutation(4, true);
}
#[test]
fn fresh_minimap_press_keeps_mutation_before_admission() {
    for kind in 0..5 {
        let mut h = fixture(kind);
        let revision = h.state().session.revision();
        let at = map(&h).center();
        raw(&mut h, vec![egui::Event::PointerMoved(at), pointer(at, true), key(kind, true)]);
        assert_eq!(h.state().session.revision(), revision + 1, "fresh press mutation {kind} retains ordering");
    }
}
#[test]
fn held_minimap_does_not_suppress_escape() {
    let mut h = fixture(0);
    press(&mut h);
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    assert!(!h.state().selection.is_empty());
    raw(&mut h, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert!(h.state().selection.is_empty());
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
}
#[test]
#[ignore = "manual public-scene capture; requires NODECRAFT_361_SCREENSHOT and NODECRAFT_361_EXPECT_BLOCKED"]
fn held_minimap361_visual() {
    let blocked = std::env::var("NODECRAFT_361_EXPECT_BLOCKED").unwrap() == "1";
    let mut h = fixture(0);
    let camera = h.state().view;
    press(&mut h);
    let outside = Pos2::new(80.0, 120.0);
    raw(&mut h, vec![egui::Event::PointerMoved(outside), key(0, true)]);
    assert_eq!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().len(), usize::from(blocked));
    assert_eq!(h.state().session.revision(), if blocked { 2 } else { 3 });
    raw(&mut h, vec![pointer(outside, false), key(0, false)]);
    h.state_mut().view = camera;
    h.run();
    h.render().unwrap().save(std::env::var("NODECRAFT_361_SCREENSHOT").unwrap()).unwrap();
    println!("Actual held Delete outcome: blocked={blocked}, revision={}", h.state().session.revision());
}
