//! #1108: a card with keyboard focus shows a focus ring; one focused by a click, or after a
//! click elsewhere, does not.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

fn rings(h: &Harness<'static, NodeCraftApp>) -> usize {
    let app = h.state();
    let t = app.theme.tokens();
    // Shapes come out in screen points, after the card layer's transform.
    let card = super::tests::screen(app);
    let width = t.focus_ring_width;
    h.output()
        .shapes
        .iter()
        .filter(|clipped| {
            matches!(&clipped.shape, egui::epaint::Shape::Rect(r)
                if r.stroke.color == t.accent && (r.stroke.width - width).abs() < 1e-3 && r.rect.contains_rect(card) && r.rect != card)
        })
        .count()
}

fn card_label(h: &Harness<'static, NodeCraftApp>) -> String {
    let app = h.state();
    let node = super::tests::node(app);
    let descriptor = app.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor());
    format!("Node {}", crate::card::title(node, descriptor))
}

fn tab_to_card(h: &mut Harness<'static, NodeCraftApp>) {
    let label = card_label(h);
    for _ in 0..80 {
        if h.get_by_label(&label).is_focused() {
            return;
        }
        h.key_press(egui::Key::Tab);
        h.run();
    }
    panic!("Tab never reached {label}");
}

#[test]
fn tabbing_to_a_card_paints_a_focus_ring_and_clicking_the_canvas_removes_it() {
    let mut h = super::tests::harness();
    h.run();
    assert_eq!(rings(&h), 0, "no ring before anything has focus");
    tab_to_card(&mut h);
    h.run();
    assert_eq!(rings(&h), 1, "the keyboard-focused card shows one ring");
    // Click empty canvas, well away from the card.
    let card = super::tests::screen(h.state());
    let empty = Pos2::new(card.right() + 200.0, card.bottom().min(h.state().viewport.bottom() - 40.0));
    h.hover_at(empty);
    h.event(egui::Event::PointerButton { pos: empty, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos: empty, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(!h.get_by_label(&card_label(&h)).is_focused());
    assert_eq!(rings(&h), 0, "the ring goes with the focus");
}

#[test]
fn a_card_focused_by_a_click_shows_no_ring() {
    let mut h = super::tests::harness();
    h.run();
    let card = super::tests::screen(h.state());
    let at = card.center();
    h.hover_at(at);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert_eq!(rings(&h), 0, "a click selects; the selected border is enough");
}

/// Space (or Enter, or a screen reader's activate) on a keyboard-focused card is a fake click;
/// the ring stays, since that is when it matters most.
#[test]
fn activating_a_keyboard_focused_card_keeps_its_ring() {
    let mut h = super::tests::harness();
    h.run();
    tab_to_card(&mut h);
    h.run();
    h.key_press(egui::Key::Space);
    h.run();
    assert!(h.get_by_label(&card_label(&h)).is_focused());
    assert_eq!(rings(&h), 1, "the ring stays after a keyboard activate");
}

/// At half zoom the ring is still `focus_ring_width` wide on screen.
#[test]
fn the_ring_keeps_its_screen_width_at_half_zoom() {
    let mut h = super::tests::harness();
    let pan = h.state().view.pan;
    h.state_mut().view = crate::canvas::CanvasView { pan, zoom: 0.5 };
    h.run();
    tab_to_card(&mut h);
    h.run();
    assert_eq!(rings(&h), 1);
}
