//! Trained styles from card context menus (#167), driven through real pointer events.
use super::tests::harness;
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

const STYLE: NodeId = NodeId::from_bytes([70; 16]);
const OTHER: NodeId = NodeId::from_bytes([71; 16]);

fn canvas<'a>(h: &'a Harness<'static, NodeCraftApp>) -> &'a nodecraft_graph::Canvas {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap()
}

/// Adds a Train style card holding a trained FLUX style whose trigger is `trigger`, titled `title`.
fn add_style(h: &mut Harness<'static, NodeCraftApp>, id: NodeId, x: f32, title: &str, trigger: &str) {
    let canvas = h.state().canvas;
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": id, "kind": nodecraft_nodes::generation::STYLE_KIND, "pos": {"x": x, "y": 250}})).unwrap();
    session.dispatch("node.title", &json!({"canvas": canvas, "id": id, "title": title})).unwrap();
    let style = nodecraft_media::style::Style {
        provider: "fal".into(),
        trainer: "fal-ai/flux-lora-fast-training".into(),
        family: "flux-1".into(),
        trigger: trigger.into(),
        weights: format!("https://example.com/files/{trigger}.safetensors"),
    };
    let hash = session.import_asset(&nodecraft_media::style::encode(&style).unwrap()).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "application/vnd.nodecraft.style+json; family=flux-1".parse().unwrap(), meta: Default::default() };
    let generation = nodecraft_types::GenerationId::from_bytes([id.as_bytes()[0].wrapping_add(100); 16]);
    let results = json!([{"node": id, "generation": {"id": generation, "outputs": [Value::Media(asset)]}}]);
    session.dispatch("run.record", &json!({"canvas": canvas, "results": results})).unwrap();
    h.run();
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

/// The card wired from a Train style card, and the style card wired into it.
fn styled(h: &Harness<'static, NodeCraftApp>) -> Vec<(NodeId, NodeId)> {
    canvas(h).edges().values().filter(|e| e.to.port == "style").map(|e| (e.from.node, e.to.node)).collect()
}

#[test]
fn a_trained_style_starts_a_styled_card_and_another_style_replaces_it() {
    let mut h = harness();
    add_style(&mut h, STYLE, 150.0, "Ink wash", "inkwash");
    add_style(&mut h, OTHER, -500.0, "Neon", "neonish");
    h.state_mut().view.zoom = 0.5;
    h.run();
    let before = h.state().session.document().clone();
    right_click(&mut h, STYLE);
    h.get_by_label("New image card with this style").click();
    h.run();
    let [(from, card)] = styled(&h)[..] else { panic!("one styled card: {:?} {:?}", styled(&h), h.state().notice) };
    assert_eq!(from, STYLE);
    let made = canvas(&h).nodes().get(&card).unwrap();
    assert_eq!(made.params.get("model"), Some(&Value::Text("fal.flux-lora".into())));
    assert!(matches!(made.params.get("prompt"), Some(Value::Text(p)) if p.starts_with("inkwash")), "{:?}", made.params);
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), [card]);

    // "Use style" on the new card, brought to the middle of the view, marks the one it uses
    // and swaps in the other.
    h.state_mut().view.pan += Vec2::new(-400.0, -200.0);
    h.run();
    right_click(&mut h, card);
    h.get_by_label("Use style ⏵").click();
    h.run();
    assert!(h.query_all_by_label("Ink wash").any(|n| n.accesskit_node().is_disabled()), "the style in use is shown, not offered");
    h.get_by_label("Neon").click();
    h.run();
    assert_eq!(styled(&h), [(OTHER, card)], "{:?}", h.state().notice);
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
    }
    assert_eq!(h.state().session.document(), &before, "two undo steps");
}

#[test]
fn an_untrained_style_card_offers_no_card_and_plain_cards_no_style() {
    let mut h = harness();
    let canvas_id = h.state().canvas;
    h.state_mut()
        .session
        .dispatch("node.create", &json!({"canvas": canvas_id, "id": STYLE, "kind": nodecraft_nodes::generation::STYLE_KIND, "pos": {"x": 150, "y": 250}}))
        .unwrap();
    h.state_mut().view.zoom = 0.5;
    h.run();
    right_click(&mut h, STYLE);
    let item = h.get_by_label("New image card with this style");
    assert!(item.accesskit_node().is_disabled(), "nothing to use until it is trained");
    h.key_press(egui::Key::Escape);
    h.run();
    let text = canvas(&h).nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    right_click(&mut h, text);
    assert!(h.query_by_label("Select upstream").is_some());
    assert!(h.query_by_label("Use style ⏵").is_none());
}

#[test]
#[ignore = "manual public-safe capture; STYLES167_RENDER_DIR"]
fn styles167_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("STYLES167_RENDER_DIR").unwrap());
    let mut h = harness();
    add_style(&mut h, STYLE, 150.0, "Ink wash", "inkwash");
    add_style(&mut h, OTHER, -500.0, "Neon", "neonish");
    h.state_mut().view.zoom = 0.5;
    h.run();
    let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
    shot.run();
    right_click(&mut shot, STYLE);
    shot.render().unwrap().save(dir.join("styles167-train-menu.png")).unwrap();
    shot.get_by_label("New image card with this style").click();
    shot.run();
    let card = styled(&shot)[0].1;
    shot.state_mut().view.pan += Vec2::new(-400.0, -200.0);
    shot.run();
    right_click(&mut shot, card);
    shot.get_by_label("Use style ⏵").click();
    shot.run();
    shot.render().unwrap().save(dir.join("styles167-use-style.png")).unwrap();
}
