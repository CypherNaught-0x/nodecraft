//! The prompt's `@` picker and mention hint (#505), driven through real key and pointer events.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const CARD: NodeId = NodeId::from_bytes([50; 16]);

fn canvas<'a>(h: &'a Harness<'static, NodeCraftApp>) -> &'a nodecraft_graph::Canvas {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap()
}

fn prompt(h: &Harness<'static, NodeCraftApp>) -> String {
    match canvas(h).nodes().get(&CARD).unwrap().params.get("prompt") {
        Some(Value::Text(text)) => text.clone(),
        None => String::new(),
        other => panic!("{other:?}"),
    }
}

/// The headless app's text card titled "Story", and an image generation card under it whose
/// prompt is open for editing. Returns the text card's id.
fn editing() -> (Harness<'static, NodeCraftApp>, NodeId) {
    let mut h = harness();
    let canvas_id = h.state().canvas;
    let story = canvas(&h).nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    let session = &mut h.state_mut().session;
    session.dispatch("node.title", &json!({"canvas": canvas_id, "id": story, "title": "Story"})).unwrap();
    let kind = nodecraft_nodes::generation::KIND;
    session.dispatch("node.create", &json!({"canvas": canvas_id, "id": CARD, "kind": kind, "pos": {"x": 200, "y": 150}})).unwrap();
    h.state_mut().selection.clear();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Prompt").focus();
    h.run();
    (h, story)
}

fn type_text(h: &mut Harness<'static, NodeCraftApp>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.run();
}

fn press(h: &mut Harness<'static, NodeCraftApp>, key: egui::Key) {
    h.key_press(key);
    h.run();
}

/// Clicks bare canvas, so the prompt loses focus and keeps its draft.
fn click_away(h: &mut Harness<'static, NodeCraftApp>) {
    let pos = Pos2::new(900.0, 620.0);
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
}

#[test]
fn typing_at_lists_text_cards_and_enter_inserts_the_mention() {
    let (mut h, story) = editing();
    type_text(&mut h, "Retell @st");
    assert!(h.query_by_label("@Story").is_some(), "the list offers the text card");
    press(&mut h, egui::Key::Enter);
    assert!(h.query_by_label("@Story").is_none(), "the list closes");
    type_text(&mut h, "briefly");
    click_away(&mut h);
    assert_eq!(prompt(&h), format!("Retell {} briefly", nodecraft_nodes::mention::token("Story", story)), "Enter took no newline");
    for _ in 0..3 {
        h.run();
    }
    assert!(h.query_by_label("Reads @Story").is_some(), "the hint names what the prompt reads");
    assert!(h.query_by_label_contains("Retell @Story briefly").is_some(), "the preview shows the title, not the id");
}

#[test]
fn escape_closes_the_list_and_keeps_the_text() {
    let (mut h, _) = editing();
    type_text(&mut h, "mail @s");
    assert!(h.query_by_label("@Story").is_some());
    press(&mut h, egui::Key::Escape);
    assert!(h.query_by_label("@Story").is_none());
    type_text(&mut h, "o");
    assert!(h.query_by_label("@Story").is_none(), "stays closed for this @");
    click_away(&mut h);
    assert_eq!(prompt(&h), "mail @so", "Escape closed only the list");
}

#[test]
fn clicking_a_row_inserts_and_keeps_editing() {
    let (mut h, story) = editing();
    type_text(&mut h, "@");
    h.get_by_label("@Story").click();
    h.run();
    type_text(&mut h, "again");
    click_away(&mut h);
    assert_eq!(prompt(&h), format!("{} again", nodecraft_nodes::mention::token("Story", story)));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(prompt(&h), "", "one edit, not the search and then the mention");
}

#[test]
fn a_mention_of_a_deleted_card_is_flagged() {
    let (mut h, story) = editing();
    type_text(&mut h, "@st");
    press(&mut h, egui::Key::Tab);
    type_text(&mut h, "x");
    click_away(&mut h);
    assert_eq!(prompt(&h), format!("{} x", nodecraft_nodes::mention::token("Story", story)), "Tab kept the field focused");
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": canvas_id, "id": story})).unwrap();
    for _ in 0..3 {
        h.run();
    }
    assert!(h.query_by_label("@Story is no longer on this canvas").is_some());
}
