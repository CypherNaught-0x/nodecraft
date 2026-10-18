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
    h.get_by_label(&format!("Node {}", card::title(card(h.state()), None))).click();
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert_eq!(card(h.state()).active, Some(g(2)));
    // Each step was one undo step.
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    h.run();
    assert_eq!(card(h.state()).active, Some(g(1)));
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
    let before = card::world_rect(card(empty.state()), None, &t).height();
    let h = with_results(1);
    let after = card::world_rect(card(h.state()), None, &t).height();
    assert_eq!(after - before, crate::history::RESERVED_ROWS as f32 * t.control_row_height);
}
