//! #413: two-finger scroll and the wheel pan (or, with Scroll = Zoom, zoom) the canvas over a
//! card too. A scrollable region on the card scrolls first and the canvas takes the remainder;
//! a gesture that started on the canvas keeps panning when the pointer crosses onto a card.
use super::tests::{harness, schema_harness, screen, spec};
use super::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_types::ParamControl;

const ID: NodeId = NodeId::from_bytes([3; 16]);

fn wheel(h: &mut Harness<'static, NodeCraftApp>, phase: egui::TouchPhase, delta: Vec2) {
    h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, phase, delta, modifiers: egui::Modifiers::NONE });
}

/// One mouse-wheel notch at `pos`, run until the smoothed delta has been spent.
fn notch(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, delta: Vec2) {
    h.hover_at(pos);
    h.run();
    wheel(h, egui::TouchPhase::Move, delta);
    h.run();
}

/// A card whose parameter body is taller than the card, so the body itself can scroll.
fn scrollable() -> Harness<'static, NodeCraftApp> {
    let mut h = schema_harness((0..40).map(|n| spec(&format!("p{n}"), ParamControl::Toggle, Value::Bool(false))).collect());
    h.run();
    h
}

fn card(h: &Harness<'static, NodeCraftApp>) -> Rect {
    h.state().view.rect_to_screen(h.state().viewport.min, *h.state().painted.get(&ID).unwrap())
}

/// Where the body's second row sits relative to the card: changes only when the body scrolls.
fn body_offset(h: &Harness<'static, NodeCraftApp>) -> f32 {
    let row = h.get_by_role_and_label(Role::CheckBox, "p1").rect();
    row.min.y - card(h).min.y
}

fn close(a: Vec2, b: Vec2) -> bool {
    (a - b).length() < 1.0
}

#[test]
fn scroll413_scroll_pans_over_a_card_with_nothing_to_scroll_on_both_axes() {
    for delta in [Vec2::new(0.0, 40.0), Vec2::new(0.0, -40.0), Vec2::new(40.0, 0.0), Vec2::new(-40.0, 0.0)] {
        let mut h = harness();
        assert_eq!(h.state().view.zoom, 1.0);
        assert!(h.state().view.zoom >= card::BODY_MIN_ZOOM, "the body is shown, as in the report");
        let document = h.state().session.document().clone();
        let pan = h.state().view.pan;
        let at = screen(h.state()).center();
        notch(&mut h, at, delta);
        assert!(close(h.state().view.pan - pan, delta), "{delta:?}: panned {:?}", h.state().view.pan - pan);
        assert_eq!(h.state().session.document(), &document);
    }
}

#[test]
fn scroll413_scroll_zoom_preference_zooms_over_a_card() {
    let mut h = harness();
    h.state_mut().canvas_prefs.scroll = nodecraft_engine::settings::ScrollAction::Zoom;
    let pan = h.state().view.pan;
    let at = screen(h.state()).center();
    notch(&mut h, at, Vec2::new(0.0, 60.0));
    assert!(h.state().view.zoom > 1.1, "zoom {}", h.state().view.zoom);
    // Horizontal scroll still pans under Scroll = Zoom; it is the zoom anchor's only other effect.
    let zoom = h.state().view.zoom;
    let pan_before = h.state().view.pan;
    let at = screen(h.state()).center();
    notch(&mut h, at, Vec2::new(40.0, 0.0));
    assert_eq!(h.state().view.zoom, zoom);
    assert!(close(h.state().view.pan - pan_before, Vec2::new(40.0, 0.0)), "{:?} from {pan:?}", h.state().view.pan);
}

#[test]
fn scroll413_a_body_that_can_scroll_takes_the_scroll_first() {
    let mut h = scrollable();
    let at = card(&h).center();
    let pan = h.state().view.pan;
    let offset = body_offset(&h);
    notch(&mut h, at, Vec2::new(0.0, -40.0));
    assert!(close(h.state().view.pan, pan), "the body had room to scroll, so the canvas stays put");
    assert!(body_offset(&h) < offset - 20.0, "body scrolled: {offset} -> {}", body_offset(&h));
}

#[test]
fn scroll413_a_body_at_its_limit_passes_the_scroll_to_the_canvas() {
    let mut h = scrollable();
    let at = card(&h).center();
    let pan = h.state().view.pan;
    let offset = body_offset(&h);
    // Already at the top: scrolling further up has nowhere to go in the body.
    notch(&mut h, at, Vec2::new(0.0, 40.0));
    assert!(close(h.state().view.pan - pan, Vec2::new(0.0, 40.0)), "panned {:?}", h.state().view.pan - pan);
    assert_eq!(body_offset(&h), offset);
}

#[test]
fn scroll413_horizontal_scroll_over_a_vertical_body_pans_the_canvas() {
    let mut h = scrollable();
    let at = card(&h).center();
    let pan = h.state().view.pan;
    let offset = body_offset(&h);
    notch(&mut h, at, Vec2::new(-40.0, 0.0));
    assert!(close(h.state().view.pan - pan, Vec2::new(-40.0, 0.0)), "panned {:?}", h.state().view.pan - pan);
    assert_eq!(body_offset(&h), offset);
}

#[test]
fn scroll413_a_swipe_from_the_canvas_keeps_panning_across_a_scrollable_card() {
    let mut h = scrollable();
    let rect = card(&h);
    let background = Pos2::new(rect.min.x - 150.0, rect.center().y);
    assert!(h.state().viewport.contains(background));
    h.hover_at(background);
    h.step();
    let pan = h.state().view.pan;
    let offset = body_offset(&h);
    let step = Vec2::new(0.0, -20.0);
    wheel(&mut h, egui::TouchPhase::Start, Vec2::ZERO);
    h.step();
    for _ in 0..3 {
        wheel(&mut h, egui::TouchPhase::Move, step);
        h.step();
    }
    // Cross onto the card mid-swipe; the body could scroll, but the swipe is the canvas's.
    h.hover_at(card(&h).center());
    h.step();
    for n in 0..5 {
        wheel(&mut h, egui::TouchPhase::Move, step);
        h.step();
        assert!(close(h.state().view.pan - pan, step * (4 + n) as f32), "frame {n}: panned {:?}", h.state().view.pan - pan);
    }
    // The canvas pans after the cards are drawn: one quiet frame shows where the last pan put them.
    h.step();
    assert_eq!(body_offset(&h), offset, "the body must not steal a swipe already under way");
    wheel(&mut h, egui::TouchPhase::End, Vec2::ZERO);
    h.step();
    // A new swipe that starts over the body scrolls the body.
    let pan = h.state().view.pan;
    wheel(&mut h, egui::TouchPhase::Start, Vec2::ZERO);
    h.step();
    for _ in 0..3 {
        wheel(&mut h, egui::TouchPhase::Move, step);
        h.step();
    }
    wheel(&mut h, egui::TouchPhase::End, Vec2::ZERO);
    h.run();
    assert!(close(h.state().view.pan, pan));
    assert!(body_offset(&h) < offset - 40.0, "body scrolled: {offset} -> {}", body_offset(&h));
}

#[test]
fn scroll413_a_body_near_its_limit_takes_the_rest_of_its_room_and_the_canvas_the_remainder() {
    let mut h = scrollable();
    let at = card(&h).center();
    let offset = body_offset(&h);
    notch(&mut h, at, Vec2::new(0.0, -40.0));
    let scrolled = offset - body_offset(&h);
    assert!(scrolled > 20.0, "body scrolled {scrolled}");
    // Run past the end of that gesture, then scroll back up by more than the body has left.
    h.run_steps(30);
    let pan = h.state().view.pan;
    notch(&mut h, at, Vec2::new(0.0, 100.0));
    assert!((body_offset(&h) - offset).abs() < 0.5, "body back at its top: {} vs {offset}", body_offset(&h));
    let panned = h.state().view.pan - pan;
    assert!(close(panned, Vec2::new(0.0, 100.0 - scrolled)), "canvas took the remainder: {panned:?} after the body's {scrolled}");
}
