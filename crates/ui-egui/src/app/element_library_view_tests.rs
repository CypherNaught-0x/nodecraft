//! The Elements library from the canvas (#552): save an Element from its card's menu, place it
//! from the Elements panel, and update it from the library.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const PIC: NodeId = NodeId::from_bytes([40; 16]);

/// The headless app with an Element made of one image.
fn with_element() -> (Harness<'static, NodeCraftApp>, NodeId) {
    let mut h = harness();
    let png = nodecraft_media::encode_png(8, 6, &[120; 8 * 6 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: -400.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[PIC]).unwrap();
    h.state_mut().selection.only(PIC);
    h.state_mut().make_element();
    h.run();
    let id = elements(&h).first().unwrap().id;
    (h, id)
}

fn elements(h: &Harness<'static, NodeCraftApp>) -> Vec<Node> {
    let app = h.state();
    app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().filter(|n| n.kind == nodecraft_nodes::element::ID).cloned().collect()
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

#[test]
fn an_element_is_saved_from_its_menu_and_placed_from_the_panel() {
    let (mut h, id) = with_element();
    right_click(&mut h, id);
    assert!(h.query_by_label("Update from Elements library").is_none(), "nothing to update from yet");
    h.get_by_label("Save to Elements library").click();
    h.run();
    assert_eq!(h.state().notice, None);
    right_click(&mut h, id);
    assert!(h.query_by_label("Replace in Elements library").is_some(), "saved");
    assert!(h.query_by_label("Update from Elements library").is_some());
    h.key_press(egui::Key::Escape);
    h.run();

    h.get_by_label("Elements").click();
    h.run();
    assert!(h.query_by_label_contains("#element · general · 1 image").is_some());
    let before = h.state().session.document().clone();
    h.get_by_label("Place").click();
    h.run();
    let placed: Vec<Node> = elements(&h).into_iter().filter(|n| n.id != id).collect();
    assert_eq!(placed.len(), 1, "{:?}", h.state().notice);
    assert_eq!(nodecraft_nodes::element::name(&placed[0]), Some("element-2"), "under a free name");
    assert!(h.state().selection.contains(placed[0].id));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "placing is one undo step");

    // The Element card has image Remove buttons too; the panel is drawn last.
    h.get_all_by_label("Remove").last().unwrap().click();
    h.run();
    assert!(h.query_by_label_contains("The library is empty").is_some());
}

#[test]
fn update_takes_the_librarys_settings() {
    let (mut h, id) = with_element();
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("element.save", &json!({"canvas": canvas, "id": id})).unwrap();
    let guidelines = serde_json::to_value(Value::Text("red scarf".into())).unwrap();
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas, "id": id, "name": "guidelines", "value": guidelines})).unwrap();
    h.state_mut().element_library = Default::default();
    h.run();
    right_click(&mut h, id);
    h.get_by_label("Update from Elements library").click();
    h.run();
    assert_eq!(h.state().notice, None);
    let node = elements(&h).into_iter().find(|n| n.id == id).unwrap();
    assert_eq!(node.params.get("guidelines"), Some(&Value::Text(String::new())));
}
