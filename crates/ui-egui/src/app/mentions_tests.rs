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
    let text = canvas(&h).nodes().values().find(|n| n.kind == "source.text").unwrap();
    let (story, at) = (text.id, text.pos);
    let session = &mut h.state_mut().session;
    session.dispatch("node.title", &json!({"canvas": canvas_id, "id": story, "title": "Story"})).unwrap();
    let kind = nodecraft_nodes::generation::KIND;
    session.dispatch("node.create", &json!({"canvas": canvas_id, "id": CARD, "kind": kind, "pos": {"x": at.x - 330.0, "y": at.y}})).unwrap();
    h.state_mut().selection.clear();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Prompt").focus();
    h.run();
    (h, story)
}

/// The focused prompt field.
fn prompt_field<'t>(h: &'t Harness<'static, NodeCraftApp>) -> egui_kittest::Node<'t> {
    h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Prompt")
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
    let (row, field) = (h.get_by_label("@Story").rect(), prompt_field(&h).rect());
    assert!(
        (row.left() - field.left()).abs() < 24.0 && row.top() >= field.bottom() - 1.0 && row.top() < field.bottom() + 80.0,
        "the list is under the prompt: {row:?} {field:?}"
    );
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
    assert!(h.query_by_label_contains("@Story, which is no longer on this canvas").is_some(), "the planner's reason, from the cache");
}

/// A second text card, "Stage", under the headless app's "Story": both match `@st`.
fn stage(h: &mut Harness<'static, NodeCraftApp>, story: NodeId) -> NodeId {
    let canvas_id = h.state().canvas;
    let at = canvas(h).nodes().get(&story).unwrap().pos;
    let stage = NodeId::from_bytes([51; 16]);
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas_id, "id": stage, "kind": "source.text", "pos": {"x": at.x, "y": at.y + 400.0}})).unwrap();
    session.dispatch("node.title", &json!({"canvas": canvas_id, "id": stage, "title": "Stage"})).unwrap();
    h.run();
    stage
}

#[test]
fn arrows_choose_the_card_and_wrap() {
    let (mut h, story) = editing();
    let stage = stage(&mut h, story);
    type_text(&mut h, "@st");
    assert!(h.query_by_label("@Story").is_some() && h.query_by_label("@Stage").is_some());
    press(&mut h, egui::Key::ArrowDown);
    press(&mut h, egui::Key::ArrowDown);
    press(&mut h, egui::Key::ArrowUp);
    press(&mut h, egui::Key::ArrowUp);
    press(&mut h, egui::Key::ArrowUp);
    press(&mut h, egui::Key::Enter);
    click_away(&mut h);
    assert_eq!(prompt(&h), format!("{} ", nodecraft_nodes::mention::token("Stage", stage)), "down, down (wraps), up, up (wraps), up: the second row");
}

#[test]
fn typing_more_starts_the_choice_at_the_top() {
    let (mut h, story) = editing();
    stage(&mut h, story);
    type_text(&mut h, "@s");
    press(&mut h, egui::Key::ArrowDown);
    type_text(&mut h, "t");
    press(&mut h, egui::Key::Enter);
    click_away(&mut h);
    assert_eq!(prompt(&h), format!("{} ", nodecraft_nodes::mention::token("Story", story)));
}

#[test]
fn enter_without_a_match_and_shifted_keys_stay_the_fields() {
    let (mut h, _) = editing();
    type_text(&mut h, "@zz");
    press(&mut h, egui::Key::Enter);
    type_text(&mut h, "@st");
    assert!(h.query_by_label("@Story").is_some());
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    h.run();
    click_away(&mut h);
    assert_eq!(prompt(&h), "@zz\n@st\n", "no match: a newline; Shift+Enter: a newline, not a pick");
}

#[test]
fn a_pick_inside_a_word_replaces_all_of_it() {
    let (mut h, story) = editing();
    type_text(&mut h, "@story told");
    for _ in 0..7 {
        press(&mut h, egui::Key::ArrowLeft);
    }
    press(&mut h, egui::Key::Enter);
    click_away(&mut h);
    assert_eq!(prompt(&h), format!("{} told", nodecraft_nodes::mention::token("Story", story)));
}

#[test]
fn too_many_mentions_block_the_card_and_say_so() {
    let (mut h, _) = editing();
    click_away(&mut h);
    let canvas_id = h.state().canvas;
    let text: String = (0..17u8).map(|n| nodecraft_nodes::mention::token("Card", NodeId::from_bytes([n.wrapping_add(100); 16]))).collect();
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas_id, "id": CARD, "name": "prompt", "value": Value::Text(text)})).unwrap();
    for _ in 0..3 {
        h.run();
    }
    assert!(h.query_by_label_contains("Can't run: its prompt mentions more than 16 cards").is_some());
    assert!(h.query_by_label_contains("is no longer on this canvas").is_none(), "the reason stands for the per-mention lines");
}
