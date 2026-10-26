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
    let expected = h.state().view.to_world(h.state().viewport.min, h.state().navigation.usable(h.state().viewport, h.state().theme.tokens()).center());
    h.get_by_label("Place").click();
    h.run();
    let placed: Vec<Node> = elements(&h).into_iter().filter(|n| n.id != id).collect();
    assert_eq!(placed.len(), 1, "{:?}", h.state().notice);
    assert_eq!(nodecraft_nodes::element::name(&placed[0]), Some("element-2"), "under a free name");
    assert!(h.state().selection.contains(placed[0].id));
    // Local evaluation may finish during the pointer-driven frame and append a history
    // strip. Placement centers the fresh command payload, before that later graph edit.
    let mut created = placed[0].clone();
    created.history.clear();
    created.active = None;
    let rect = payload_rect(&h.state().session, &created, h.state().theme.tokens());
    assert!(
        (rect.center() - expected).length() < 0.01,
        "resolved library card geometry must center in view: actual {:?}, expected {:?}, viewport {:?}, view {:?}",
        rect.center(),
        expected,
        h.state().viewport,
        h.state().view
    );
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "placing is one undo step");

    // Removing asks first.
    h.get_by_label("…").click();
    h.run();
    h.get_all_by_label("Remove").last().unwrap().click();
    h.run();
    h.get_by_label("Keep").click();
    h.run();
    assert!(h.query_by_label_contains("#element · general").is_some(), "kept");
    h.get_by_label("…").click();
    h.run();
    h.get_all_by_label("Remove").last().unwrap().click();
    h.run();
    h.get_by_label("Yes, remove").click();
    h.run();
    assert!(h.query_by_label_contains("The library is empty").is_some());
}

/// The app with its Element saved and the Elements panel open.
fn with_saved() -> (Harness<'static, NodeCraftApp>, NodeId) {
    let (mut h, id) = with_element();
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("element.save", &json!({"canvas": canvas, "id": id})).unwrap();
    h.state_mut().element_library = Default::default();
    h.state_mut().element_library.open = true;
    h.run();
    (h, id)
}

fn library(h: &mut Harness<'static, NodeCraftApp>) -> serde_json::Value {
    h.state_mut().session.dispatch("library.elements", &json!({})).unwrap()["elements"].clone()
}

#[test]
fn an_entry_is_renamed_in_the_panel_and_its_placed_element_still_updates() {
    let (mut h, id) = with_saved();
    h.get_by_label("…").click();
    h.run();
    h.get_by_label("Rename").click();
    h.run();
    h.run();
    for _ in 0.."element".len() {
        h.key_press(egui::Key::Backspace);
    }
    h.event(egui::Event::Text("Hero".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().notice, None);
    assert_eq!(library(&mut h)[0]["name"], "Hero");
    assert!(h.query_by_label_contains("#Hero · general").is_some());

    // Escape keeps the name.
    h.get_by_label("…").click();
    h.run();
    h.get_by_label("Rename").click();
    h.run();
    h.event(egui::Event::Text("x".into()));
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(library(&mut h)[0]["name"], "Hero");

    // Placed now, it updates from Hero whatever the card is called.
    h.get_by_label("Place").click();
    h.run();
    let placed = elements(&h).into_iter().find(|n| n.id != id).unwrap();
    right_click(&mut h, placed.id);
    assert!(h.query_by_label("Update from Elements library").is_some());
    let original = elements(&h).into_iter().find(|n| n.id == id).unwrap();
    assert!(h.state().element_library.source(&original).is_none(), "the original was never placed from Hero");
    assert_eq!(h.state().element_library.source(&placed).as_deref(), Some("Hero"));
}

#[test]
fn an_entry_is_shared_exported_and_imported_from_the_panel() {
    let (mut h, _) = with_saved();
    h.get_by_label("Shared").click();
    h.run();
    assert_eq!(library(&mut h)[0]["shared"], true);

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("element.ncelement");
    h.state_mut().exports.answer = Some(file.clone());
    h.get_by_label("…").click();
    h.run();
    h.get_by_label("Export…").click();
    h.run();
    h.run();
    assert!(file.is_file(), "{:?}", h.state().notice);

    // Importing over its own name asks first.
    h.state_mut().element_library.answer = Some(file.clone());
    h.get_by_label("Import…").click();
    h.run();
    h.run();
    assert!(h.query_by_label_contains("already has an Element").is_some(), "{:?}", h.state().notice);
    h.get_by_label("Replace it").click();
    h.run();
    assert_eq!(h.state().notice, None);
    assert_eq!(library(&mut h)[0]["shared"], false, "an imported Element is private");
}

#[test]
fn entries_that_cant_be_used_are_listed_with_why() {
    let (mut h, _) = with_saved();
    h.state_mut().element_library.open = false;
    h.run();
    // A newer build's entry, put in the library file by hand.
    let store = nodecraft_engine::element_library::MemoryElements::default();
    let entry = json!({"name": "Villain", "category": "character", "guidelines": "", "images": [], "shared": false, "voice": "low"});
    nodecraft_engine::element_library::ElementStore::save(&store, &serde_json::to_vec(&json!({"version": 1, "elements": [entry]})).unwrap()).unwrap();
    nodecraft_engine::element_library::install(&mut h.state_mut().session, std::rc::Rc::new(store));
    h.state_mut().element_library = Default::default();
    h.state_mut().element_library.open = true;
    h.run();
    assert!(h.query_by_label_contains("1 entry can't be used").is_some());
    assert!(h.query_by_label_contains("#Villain:").is_some());
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
