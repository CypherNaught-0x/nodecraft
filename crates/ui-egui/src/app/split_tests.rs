//! "Split into grid" (#86) through the card's context menu.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const IMAGE: NodeId = NodeId::from_bytes([40; 16]);

/// The headless app plus an 8×6 Image node to its right.
fn with_image() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let png = nodecraft_media::encode_png(8, 6, &[120; 8 * 6 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[IMAGE]).unwrap();
    h.run();
    h
}

/// Right-clicks just inside node `id`'s card.
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

fn count(h: &Harness<'static, NodeCraftApp>) -> usize {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().len()
}

#[test]
fn the_context_menu_splits_an_image_into_selected_nodes_in_one_undo_step() {
    let mut h = with_image();
    let before = h.state().session.document().clone();
    let nodes = count(&h);
    right_click(&mut h, IMAGE);
    h.get_by_label_contains("Split into grid").hover();
    h.run();
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut().selection.only(IMAGE);
    let text = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    h.state_mut().selection.add(text);
    assert!(h.state_mut().layout_enabled(&ctx, &json!({"mode":"grid"})).is_err());
    assert!(h.state_mut().dispatch_layout(&ctx, &json!({"mode":"grid"})).is_err());
    assert_eq!(h.state().session.document(), &before);
    h.get_by_label("Split into 4 images").click();
    h.run();
    assert_eq!(count(&h), nodes + 4, "{:?}", h.state().notice);
    assert_eq!(h.state().selection.nodes().len(), 4, "the tiles are selected");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn nodes_without_an_image_offer_no_split() {
    let mut h = with_image();
    let text = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    right_click(&mut h, text);
    assert!(h.query_by_label("Select upstream").is_some());
    assert!(h.query_by_label_contains("Split into grid").is_none());
}

#[test]
fn the_grid_shows_on_the_image_while_the_split_menu_is_open() {
    let mut h = with_image();
    for _ in 0..100 {
        h.run();
        if h.query_by_label("Image 8×6").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(h.query_by_label("Image 8×6").is_some(), "the preview is ready");
    right_click(&mut h, IMAGE);
    h.get_by_label_contains("Split into grid").hover();
    h.run();
    h.run();
    assert!(h.query_by_label("Image 8×6, split preview 2 rows by 2 columns").is_some(), "the grid is drawn over the image");
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert!(h.query_by_label_contains("split preview").is_none(), "and goes when the menu closes");
}

#[test]
fn split_preview_owns_input_until_the_tag_filter_can_resume() {
    let mut h = with_image();
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
    for _ in 0..100 {
        h.run();
        if h.query_by_label("Image 8×6").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    right_click(&mut h, IMAGE);
    h.get_by_label_contains("Split into grid").hover();
    h.run();
    h.run();
    assert!(h.query_by_label("Image 8×6, split preview 2 rows by 2 columns").is_some());
    let before = h.state().session.document().clone();
    let revision = h.state().session.revision();
    assert!(h.state_mut().tag_filter_enabled(&ctx, "canvas.tag_filter_next", &json!({})).is_err());
    assert!(h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).is_err());
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().tag_filter.current, None);
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert!(h.query_by_label_contains("split preview").is_none());
    assert!(h.state_mut().tag_filter_enabled(&ctx, "canvas.tag_filter_next", &json!({})).is_ok());
    assert_eq!(h.state().session.document(), &before);
}
