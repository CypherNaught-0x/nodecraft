//! "Make Element from selection" and the prompt's mention hint (#104).
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const LEFT: NodeId = NodeId::from_bytes([40; 16]);
const RIGHT: NodeId = NodeId::from_bytes([41; 16]);
const CARD: NodeId = NodeId::from_bytes([42; 16]);

/// The headless app plus two images side by side, both selected.
fn with_images() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let png = nodecraft_media::encode_png(8, 6, &[120; 8 * 6 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    for (id, x) in [(RIGHT, 500.0), (LEFT, 200.0)] {
        let at = nodecraft_graph::Position { x, y: -110.0 };
        import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[id]).unwrap();
    }
    h.state_mut().selection.only(LEFT);
    h.state_mut().selection.add(RIGHT);
    h.run();
    h
}

fn right_click(h: &mut Harness<'static, NodeCraftApp>, id: NodeId) {
    let pos = {
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        let descriptor = app.session.kinds().get(&node.kind).ok().map(|k| k.descriptor());
        app.view.rect_to_screen(app.viewport.min, card::world_rect(node, descriptor, app.theme.tokens())).min + Vec2::splat(6.0)
    };
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    h.run();
}

fn canvas<'a>(h: &'a Harness<'static, NodeCraftApp>) -> &'a nodecraft_graph::Canvas {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap()
}

#[test]
fn the_menu_makes_one_element_from_the_selected_images_in_reading_order() {
    let mut h = with_images();
    let before = h.state().session.document().clone();
    right_click(&mut h, LEFT);
    h.get_by_label("Make Element from selection").click();
    h.run();
    let made = canvas(&h).nodes().values().find(|n| n.kind == nodecraft_nodes::element::ID).expect("an Element").clone();
    assert_eq!(nodecraft_nodes::element::name(&made), Some("element"), "{:?}", h.state().notice);
    let mut wires: Vec<_> = canvas(&h).edges().values().filter(|e| e.to.node == made.id).collect();
    wires.sort_by_key(|e| e.order);
    assert_eq!(wires.iter().map(|e| e.from.node).collect::<Vec<_>>(), vec![LEFT, RIGHT], "left to right");
    assert!(made.pos.x > 500.0, "beside the images");
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), vec![made.id]);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo step");
}

#[test]
fn text_only_selections_offer_no_element() {
    let mut h = harness();
    let text = canvas(&h).nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    right_click(&mut h, text);
    assert!(h.query_by_label("Select upstream").is_some());
    assert!(h.query_by_label("Make Element from selection").is_none());
}

#[test]
fn a_prompt_shows_the_elements_it_mentions() {
    let mut h = with_images();
    h.state_mut().make_element();
    let canvas_id = h.state().canvas;
    let session = &mut h.state_mut().session;
    let kind = nodecraft_nodes::generation::KIND;
    session.dispatch("node.create", &json!({"canvas": canvas_id, "id": CARD, "kind": kind, "pos": {"x": 200, "y": 150}})).unwrap();
    let prompt = serde_json::to_value(Value::Text("the #Element in #ff0000".into())).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": canvas_id, "id": CARD, "name": "prompt", "value": prompt})).unwrap();
    h.state_mut().selection.clear();
    for _ in 0..5 {
        h.run();
    }
    assert!(h.query_by_label("Uses #Element").is_some(), "unknown tags aren't listed");
    // A second Element with the same name (from a hostile or old file) is flagged.
    let mut twin = canvas(&h).nodes().values().find(|n| n.kind == nodecraft_nodes::element::ID).unwrap().clone();
    twin.id = NodeId::from_bytes([43; 16]);
    twin.pos.y += 600.0;
    twin.history.clear();
    twin.active = None;
    let mut project = h.state().session.document().clone();
    nodecraft_graph::Change::AddNode { canvas: canvas_id, node: twin }.apply(&mut project).unwrap();
    h.state_mut().session = nodecraft_engine::Session::new(project).unwrap();
    for _ in 0..5 {
        h.run();
    }
    assert!(h.query_by_label_contains("more than one Element has this name").is_some());
}
