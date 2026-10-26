//! Deck card previews and copied sources through the real canvas (#695).
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const IMAGE: NodeId = NodeId::from_bytes([95; 16]);
const TEXT: NodeId = NodeId::from_bytes([96; 16]);
const DECK: NodeId = NodeId::from_bytes([97; 16]);

fn populate(app: &mut NodeCraftApp) {
    let canvas = app.canvas;
    let initial: Vec<_> = app.session.document().canvases().get(&canvas).unwrap().nodes().keys().copied().collect();
    for id in initial {
        app.session.dispatch("node.remove", &json!({"canvas":canvas,"id":id})).unwrap();
    }
    let mut pixels = Vec::new();
    for y in 0..90 {
        for x in 0..160 {
            pixels.extend_from_slice(if x < 80 && y < 60 { &[48, 120, 186, 255] } else { &[231, 161, 116, 255] });
        }
    }
    let png = nodecraft_media::encode_png(160, 90, &pixels, &Default::default()).unwrap();
    import_files(
        &mut app.session,
        canvas,
        nodecraft_graph::Position { x: -420.0, y: -150.0 },
        300.0,
        &[Incoming { name: "Public colours.png", bytes: &png }],
        &[IMAGE],
    )
    .unwrap();
    app.session.dispatch("node.create", &json!({"canvas":canvas,"id":TEXT,"kind":"source.text","pos":{"x":-420,"y":180}})).unwrap();
    app.session.dispatch("node.param_set", &json!({"canvas":canvas,"id":TEXT,"name":"text","value":Value::Text("A public story".into())})).unwrap();
    app.session.dispatch("deck.create", &json!({"canvas":canvas,"id":DECK,"pos":{"x":0,"y":-150},"title":"Storyboard","nodes":[IMAGE,TEXT]})).unwrap();
    app.session.dispatch("deck.slide_add", &json!({"canvas":canvas,"id":DECK,"slide":{"id":"closing","name":"Closing","layers":[]}})).unwrap();
    app.session.dispatch("deck.slide_add", &json!({"canvas":canvas,"id":DECK,"slide":{"id":"notes","name":"Notes","layers":[]}})).unwrap();
}

#[test]
#[ignore = "actual GPU capture; DECK695_RENDER_DIR and optional DECK695_BASELINE"]
fn deck695_visual() {
    let output = std::path::PathBuf::from(std::env::var_os("DECK695_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let baseline = std::env::var_os("DECK695_BASELINE").is_some();
    let mut app = NodeCraftApp::headless().unwrap();
    populate(&mut app);
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    for _ in 0..40 {
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(h.query_by_label("Deck slide 1").is_some(), !baseline);
    h.render().unwrap().save(output.join(if baseline { "before.png" } else { "after.png" })).unwrap();
}

fn headless() -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    populate(&mut app);
    let mut h = Harness::builder().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run_steps(4);
    h
}

#[test]
fn deck695_zoom_rasterizes_from_held_font_bytes_without_reading_them_again() {
    let mut app = NodeCraftApp::headless().unwrap();
    populate(&mut app);
    let canvas = app.canvas;
    let font = super::deck_fonts_tests::lito();
    nodecraft_engine::deck_cmds::fonts::import(&mut app.session, canvas, DECK, &font, None).unwrap();
    let mut text = nodecraft_actions::deck::source_layer("headline", "Headline", (320.0, 180.0), (500.0, 100.0));
    text.content = nodecraft_actions::layers::Content::Text {
        text: "A cached public font".into(),
        font: "Lito".into(),
        size: 48.0,
        line_spacing: 1.0,
        tracking: 0.0,
        align: nodecraft_actions::layers::Align::Center,
        color: "#18283d".into(),
    };
    app.session.dispatch("deck.slide_add", &json!({"canvas":canvas,"id":DECK,"index":0,"slide":{"id":"fonts","name":"Fonts","layers":[text]}})).unwrap();
    super::layer_fonts::PAYLOAD_READS.with(|reads| reads.set(0));
    let mut h = Harness::builder().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run_steps(4);
    let reads = super::layer_fonts::PAYLOAD_READS.with(std::cell::Cell::get);
    let renders = h.state().deck_thumbnails.renders;
    assert!(reads > 0);
    for zoom in [0.65, 0.95, 1.25, 0.75, 1.0] {
        h.state_mut().view.zoom = zoom;
        h.run_steps(3);
    }
    assert!(h.state().deck_thumbnails.renders > renders, "zoom must actually rerasterize the slide text");
    assert_eq!(super::layer_fonts::PAYLOAD_READS.with(std::cell::Cell::get), reads, "a scale-only change must reuse validated font payloads");
}

fn crowded() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    populate(&mut app);
    let canvas = app.canvas;
    for id in [IMAGE, DECK] {
        app.session.dispatch("node.remove", &json!({"canvas":canvas,"id":id})).unwrap();
    }
    app.session.dispatch("node.move", &json!({"canvas":canvas,"id":TEXT,"pos":{"x":-10000,"y":-10000}})).unwrap();
    for n in 0..17u8 {
        let id = NodeId::from_bytes([100 + n; 16]);
        let pos = json!({"x":-780 + 320 * i32::from(n % 5), "y":-820 + 450 * i32::from(n / 5)});
        app.session.dispatch("deck.create", &json!({"canvas":canvas,"id":id,"pos":pos,"title":format!("Public deck {:02}", n + 1),"nodes":[TEXT]})).unwrap();
    }
    // The offscreen source must not move the startup home away from the storyboard.
    app.centred = true;
    app.view = CanvasView { pan: Vec2::new(850.0, 900.0), zoom: 1.0 };
    app
}

#[test]
fn deck695_seventeen_visible_cards_keep_resident_textures_and_admit_after_pan() {
    let mut h = Harness::builder().with_size(Vec2::new(1900.0, 2000.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), crowded());
    h.run_steps(4);
    let renders = h.state().deck_thumbnails.renders;
    h.run_steps(4);
    assert_eq!(h.state().deck_thumbnails.renders, renders, "17 visible cards must not evict every resident each idle frame");
    assert_eq!(renders, 16, "only the bounded resident cards render");
    assert!(h.query_by_label("Zoom in to preview slides").is_some(), "one overflow card explains how to see its slides");
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":NodeId::from_bytes([100;16]),"pos":{"x":-10000,"y":-10000}})).unwrap();
    h.run_steps(4);
    assert!(h.query_by_label("Zoom in to preview slides").is_none(), "the overflow card enters after a resident leaves view");
    assert_eq!(h.state().deck_thumbnails.renders, renders + 1);
}

#[test]
#[ignore = "actual GPU capture; DECK695_CROWD_RENDER_DIR and optional DECK695_CROWD_BASELINE"]
fn deck695_crowded_visual() {
    let output = std::path::PathBuf::from(std::env::var_os("DECK695_CROWD_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let baseline = std::env::var_os("DECK695_CROWD_BASELINE").is_some();
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1900.0, 2000.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), crowded());
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run_steps(4);
    let renders = h.state().deck_thumbnails.renders;
    h.run_steps(2);
    assert_eq!(h.state().deck_thumbnails.renders > renders, baseline, "only the old FIFO repaints on idle frames");
    assert_eq!(h.query_by_label("Zoom in to preview slides").is_some(), !baseline);
    h.render().unwrap().save(output.join(if baseline { "before.png" } else { "after.png" })).unwrap();
}

#[test]
fn deck695_card_shows_first_three_slides_and_reuses_vectors_until_source_content_changes() {
    let mut h = headless();
    for n in 1..=3 {
        assert!(h.query_by_label(&format!("Deck slide {n}")).is_some());
    }
    assert!(h.query_by_label("Deck slide 4").is_none());
    let renders = h.state().deck_thumbnails.renders;
    assert!(renders > 0);
    h.run_steps(4);
    assert_eq!(h.state().deck_thumbnails.renders, renders, "unchanged frames reuse rendered text");
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":IMAGE,"pos":{"x":-410,"y":-150}})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().deck_thumbnails.renders, renders, "moving a source does not alter slide content");
    h.state_mut()
        .session
        .dispatch("node.param_set", &json!({"canvas":canvas,"id":TEXT,"name":"text","value":Value::Text("The next public chapter".into())}))
        .unwrap();
    h.run_steps(3);
    assert_eq!(h.state().deck_thumbnails.renders, renders + 1, "only the changed source-text layer renders again");
}

fn selected_deck_sources(h: &Harness<'static, NodeCraftApp>) -> Vec<NodeId> {
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    let deck = canvas.nodes().values().find(|n| n.kind == nodecraft_nodes::deck::ID && h.state().selection.contains(n.id)).unwrap();
    nodecraft_engine::deck_cmds::stored(deck)
        .unwrap()
        .slides
        .into_iter()
        .flat_map(|s| s.layers)
        .filter_map(|item| match item {
            nodecraft_actions::deck::Item::Layer(nodecraft_actions::layers::Layer {
                content: nodecraft_actions::layers::Content::Source { origin, .. },
                ..
            }) => Some(origin.parse().unwrap()),
            _ => None,
        })
        .collect()
}

#[test]
fn deck695_real_duplicate_and_paste_follow_copied_sources_as_one_undo_step() {
    let mut h = headless();
    let original = h.state().session.document().clone();
    h.state_mut().selection.replace([IMAGE, TEXT, DECK].into_iter().collect());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run_steps(3);
    let selected = h.state().selection.nodes();
    assert_eq!(selected.len(), 3, "{:?}", h.state().notice);
    for source in selected_deck_sources(&h) {
        assert!(selected.contains(&source));
        assert!(![IMAGE, TEXT].contains(&source));
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(h.state().session.document(), &original, "one undo removes the entire duplicate");

    h.state_mut().selection.replace([IMAGE, TEXT, DECK].into_iter().collect());
    let payload = crate::clipboard::checked_payload(&h.state().session, h.state().canvas, &h.state().selection).unwrap().unwrap();
    let text = crate::clipboard::to_text(&payload).unwrap();
    h.hover_at(Pos2::new(100.0, 500.0));
    h.event(egui::Event::Paste(text));
    h.run_steps(3);
    let selected = h.state().selection.nodes();
    assert_eq!(selected.len(), 3, "{:?}", h.state().notice);
    for source in selected_deck_sources(&h) {
        assert!(selected.contains(&source));
        assert!(![IMAGE, TEXT].contains(&source));
    }
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(h.state().session.document(), &original);

    h.state_mut().selection.only(DECK);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run_steps(3);
    assert_eq!(selected_deck_sources(&h), [IMAGE, TEXT], "copying only the deck keeps its external references");
}
