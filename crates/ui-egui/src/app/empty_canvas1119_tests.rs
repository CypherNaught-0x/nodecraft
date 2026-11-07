//! #1119: an empty canvas says how to add a node, its button opens the palette, and the hint goes
//! once a node exists; the sample card's tip can be dismissed.
use super::*;
use egui_kittest::kittest::Queryable;

fn empty() -> egui_kittest::Harness<'static, NodeCraftApp> {
    let mut h = super::tests::harness();
    h.state_mut().guidance = true;
    h.run();
    let canvas = h.state().canvas;
    let node = super::tests::node(h.state()).id;
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": canvas, "id": node})).unwrap();
    h.run();
    assert!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().is_empty());
    h
}

#[test]
fn an_empty_canvas_shows_the_hint_and_its_button_opens_the_palette() {
    let mut h = empty();
    assert!(h.query_by_label("Double-click anywhere to add a node").is_some());
    assert!(h.query_by_label("Drop images, video or a folder here").is_some());
    assert!(h.query_by_label("Open a Technique…").is_some());
    h.get_by_label("Add a node").click();
    h.run();
    assert!(h.state().palette.is_some(), "the button opens the quick-add palette");
}

#[test]
fn adding_a_node_hides_the_hint() {
    let mut h = empty();
    let canvas = h.state().canvas;
    h.state_mut()
        .session
        .dispatch("node.create", &json!({"canvas": canvas, "id": NodeId::from_bytes([0x19; 16]), "kind": "source.image", "pos": {"x": 0.0, "y": 0.0}}))
        .unwrap();
    h.run();
    assert!(h.query_by_label("Double-click anywhere to add a node").is_none());
    assert!(h.query_by_label("Add a node").is_none());
}

#[test]
fn the_sample_cards_tip_shows_until_dismissed() {
    let mut h = super::tests::harness();
    h.state_mut().guidance = true;
    h.run();
    assert!(h.query_by_label("Drag from + to connect to a model").is_some(), "the lone sample Text card has a tip");
    h.get_by_label("Dismiss the tip").click();
    h.run();
    assert!(h.query_by_label("Drag from + to connect to a model").is_none());
}

#[test]
fn the_add_node_button_stays_with_nodes_on_the_canvas_and_opens_the_palette() {
    let mut h = super::tests::harness();
    h.state_mut().guidance = true;
    h.run();
    assert!(!h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().is_empty());
    assert!(h.state().palette.is_none());
    h.get_by_label("+ Add node").click();
    h.run();
    assert!(h.state().palette.is_some(), "the button opens the quick-add palette");
}

#[test]
fn without_guidance_there_is_no_add_node_button() {
    let mut h = super::tests::harness();
    h.run();
    assert!(h.query_by_label("+ Add node").is_none(), "programmatic and headless windows get no first-run chrome");
}
