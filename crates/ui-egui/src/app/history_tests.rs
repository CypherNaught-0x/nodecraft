//! The history strip on a card with results (#81), driven through real pointer and key events.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::GenerationId;

const NODE: NodeId = NodeId::from_bytes([3; 16]);

fn g(n: u8) -> GenerationId {
    GenerationId::from_bytes([0x40 + n; 16])
}

/// The headless app's text node with `n` results, the newest active.
fn with_results(n: u8) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    for i in 1..=n {
        let canvas = h.state().canvas;
        let results = json!([{"node": NODE, "generation": {"id": g(i), "outputs": [{"type": "text", "value": format!("Take {i}: a forest")}]}}]);
        h.state_mut().session.dispatch("run.record", &json!({"canvas": canvas, "results": results})).unwrap();
    }
    h.run();
    h
}

fn card(app: &NodeCraftApp) -> &Node {
    app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&NODE).unwrap()
}

#[test]
fn the_strip_steps_through_results_with_buttons_chips_and_arrow_keys() {
    let mut h = with_results(3);
    assert!(h.query_by_label("3 / 3").is_some());
    h.get_by_label("Previous result").click();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(2)));
    h.get_by_label("Result 1 of 3").click();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(1)));
    assert!(h.query_by_label("1 / 3").is_some());
    // The clicked chip keeps focus, and → / ← step from there.
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert_eq!(card(h.state()).active, Some(g(2)));
    for _ in 0..3 {
        h.key_press(egui::Key::ArrowLeft);
        h.run();
    }
    assert_eq!(card(h.state()).active, Some(g(1)), "stops at the oldest");
    // So does the focused card.
    // The compact body's center is now usable text, so focus its background padding.
    let rect = screen_rect(h.state());
    let at = Pos2::new(rect.center().x, rect.top() + h.state().theme.tokens().card_padding * h.state().view.zoom * 0.5);
    h.hover_at(at);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    assert!(!h.state().text_focused);
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert_eq!(card(h.state()).active, Some(g(2)));
    // Each step was one undo step.
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(1)));
}

#[test]
fn the_active_chip_stays_inside_the_card_however_many_results() {
    for n in [12u8, 40, 150] {
        let mut h = with_results(n);
        for _ in 0..3 {
            let card_rect = screen_rect(h.state());
            let active = card(h.state()).active.and_then(|id| card(h.state()).history.iter().position(|g| g.id == id)).unwrap();
            let chip = h.get_by_label(&format!("Result {} of {n}, active", active + 1)).rect();
            assert!(card_rect.contains_rect(chip), "{n} results: chip {chip:?} outside card {card_rect:?}");
            h.get_by_label("Previous result").click();
            h.run();
        }
    }
}

fn screen_rect(app: &NodeCraftApp) -> Rect {
    app.view.rect_to_screen(app.viewport.min, payload_rect(&app.session, card(app), app.theme.tokens()))
}

#[test]
fn clicking_elsewhere_after_a_chip_takes_focus_and_arrows_back() {
    let mut h = with_results(3);
    h.get_by_label("Result 2 of 3").click();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(2)));
    h.key_press(egui::Key::ArrowLeft);
    h.run();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(1)), "the chip steps");
    // Now the card's text field: it gets focus, and ← moves its cursor, not the history.
    let field = screen_rect(h.state()).min + Vec2::new(50.0, 55.0);
    h.hover_at(field);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: field, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    h.run();
    assert!(h.state().text_focused, "the text field keeps the focus it was given");
    h.key_press(egui::Key::ArrowRight);
    h.run();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(1)), "arrows belong to the text field");
}

#[test]
fn a_card_with_many_results_draws_a_bounded_window_of_chips() {
    let mut h = with_results(40);
    assert!(h.query_by_label("Result 40 of 40, active").is_some());
    assert!(h.query_by_label("Result 1 of 40").is_none(), "far-off results have no chip");
    h.get_by_label("Previous result").click();
    h.run();
    assert!(h.query_by_label("Result 39 of 40, active").is_some());
}

#[test]
fn the_context_menu_deletes_and_extracts_and_clear_is_undoable() {
    let mut h = with_results(3);
    let nodes = |h: &Harness<'static, NodeCraftApp>| h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().len();
    h.get_by_label("Result 3 of 3, active").click_secondary();
    h.run();
    h.get_by_label("Delete result").click();
    h.run();
    assert_eq!(card(h.state()).history.iter().map(|g| g.id).collect::<Vec<_>>(), [g(1), g(2)]);
    assert_eq!(card(h.state()).active, Some(g(2)), "the neighbour takes over");

    h.get_by_label("Clear results").click();
    h.run();
    assert!(card(h.state()).history.is_empty());
    assert!(h.query_by_label("Clear results").is_none(), "no strip without results");
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(2)));
    assert!(h.query_by_label("2 / 2").is_some());

    h.get_by_label("Result 1 of 2").click_secondary();
    h.run();
    h.get_by_label("Extract to canvas").click();
    h.run();
    assert_eq!(nodes(&h), 2);
    let extracted = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().find(|n| n.id != NODE).unwrap().clone();
    assert_eq!(extracted.kind, "source.text");
    assert_eq!(extracted.params.get("text"), Some(&Value::Text("Take 1: a forest".into())));
    assert!(extracted.pos.x > card(h.state()).pos.x, "lands to the right of its node");
    assert!(extracted.history.is_empty(), "a source feeds downstream from its parameter");
    assert_eq!(card(h.state()).active, Some(g(2)), "extracting leaves the source alone");
    h.get_by_label("Result 2 of 2, active").click_secondary();
    h.run();
    h.get_by_label("Extract to canvas").click();
    h.run();
    let nodes = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().clone();
    let second = nodes.values().find(|n| n.id != NODE && n.id != extracted.id).unwrap();
    assert!(second.pos.y > extracted.pos.y, "the next extract steps down rather than stacking");
}

#[test]
fn a_card_grows_by_the_strip_only_when_it_has_results() {
    let empty = harness();
    let t = *empty.state().theme.tokens();
    let before = payload_rect(&empty.state().session, card(empty.state()), &t).height();
    let h = with_results(1);
    let after = payload_rect(&h.state().session, card(h.state()), &t).height();
    let two_native_rows = crate::history::ROWS as f32 * (t.body_size + t.control_gap);
    assert_eq!(after - before, two_native_rows + t.control_gap * 2.0);
}
