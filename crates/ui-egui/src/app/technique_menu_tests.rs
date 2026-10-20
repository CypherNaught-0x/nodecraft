//! A technique node's card actions (#94) through its context menu.
use super::tests::harness;
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_types::TechniqueId;

const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const V2: CanvasId = CanvasId::from_bytes([0x72; 16]);
const PLACED: NodeId = NodeId::from_bytes([0x50; 16]);

/// The headless app with its text node published as V1 of a technique and placed as PLACED.
fn with_technique() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let app = h.state_mut();
    let canvas = app.canvas;
    let text = app.session.document().canvases().get(&canvas).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    let publish = |published: CanvasId, seed: u8| {
        json!({"canvas": canvas, "published": published, "seed": NodeId::from_bytes([seed; 16]), "technique": TechniqueId::from_bytes([0x7e; 16]),
            "name": "Echo", "outputs": [{"node": text, "name": "Text"}]})
    };
    app.session.dispatch("technique.publish", &publish(V1, 40)).unwrap();
    app.session.dispatch("technique.place", &json!({"canvas": canvas, "version": V1, "id": PLACED, "pos": {"x": 260.0, "y": -110.0}})).unwrap();
    h.run();
    h
}

fn publish_v2(h: &mut Harness<'static, NodeCraftApp>) {
    let app = h.state_mut();
    let canvas = app.canvas;
    let text = app.session.document().canvases().get(&canvas).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    let params = json!({"canvas": canvas, "published": V2, "seed": NodeId::from_bytes([41; 16]), "technique": TechniqueId::from_bytes([0x7e; 16]),
        "name": "Echo", "outputs": [{"node": text, "name": "Words"}]});
    app.session.dispatch("technique.publish", &params).unwrap();
    h.run();
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

fn placed_version(h: &Harness<'static, NodeCraftApp>) -> u32 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&PLACED).unwrap();
    nodecraft_nodes::technique::interface(node).unwrap().version
}

#[test]
fn a_technique_node_upgrades_from_its_menu_and_opens_its_workflow() {
    let mut h = with_technique();
    right_click(&mut h, PLACED);
    assert!(h.query_by_label("Technique · v1").is_some());
    assert!(h.query_by_label_contains("Upgrade to").is_none(), "v1 is the newest");
    h.key_press(egui::Key::Escape);
    h.run();
    publish_v2(&mut h);
    right_click(&mut h, PLACED);
    h.get_by_label("Upgrade to v2").click();
    h.run();
    assert_eq!(placed_version(&h), 2, "{:?}", h.state().notice);
    right_click(&mut h, PLACED);
    assert!(h.query_by_label("Technique · v2").is_some());
    assert!(h.query_by_label_contains("Upgrade to").is_none());
    h.get_by_label("View workflow").click();
    h.run();
    assert_eq!(h.state().canvas, V2, "the version it now runs is open");
}

#[test]
fn detaching_from_the_menu_selects_what_replaced_the_node_in_one_undo_step() {
    let mut h = with_technique();
    let before = h.state().session.document().clone();
    right_click(&mut h, PLACED);
    h.get_by_label("Detach").click();
    h.run();
    let app = h.state();
    let nodes = app.session.document().canvases().get(&app.canvas).unwrap().nodes();
    assert!(!nodes.contains_key(&PLACED), "{:?}", app.notice);
    assert_eq!(app.selection.nodes().len(), 1);
    assert!(app.selection.nodes().iter().all(|id| nodes.get(id).is_some_and(|node| node.kind == "source.text")));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn a_removed_version_can_still_be_edited_but_not_viewed() {
    let mut h = with_technique();
    h.state_mut().session.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    h.run();
    right_click(&mut h, PLACED);
    assert!(h.get_by_label("View workflow").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Edit technique").accesskit_node().is_disabled(), "its source canvas is still here");
    let home = h.state().canvas;
    h.get_by_label("Edit technique").click();
    h.run();
    assert_eq!(h.state().canvas, home, "the source is the open canvas");
    // Plain nodes offer none of it.
    let text = h.state().session.document().canvases().get(&home).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    right_click(&mut h, text);
    assert!(h.query_by_label("Select upstream").is_some());
    assert!(h.query_by_label("Detach").is_none());
}

#[test]
#[ignore = "manual public-safe capture; TECHNIQUE94_RENDER_DIR"]
fn technique94_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("TECHNIQUE94_RENDER_DIR").unwrap());
    let mut h = with_technique();
    publish_v2(&mut h);
    let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
    shot.run();
    right_click(&mut shot, PLACED);
    shot.render().unwrap().save(dir.join("technique94-menu.png")).unwrap();
}
