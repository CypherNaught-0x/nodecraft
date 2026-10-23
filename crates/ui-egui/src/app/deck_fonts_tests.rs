//! Per-deck font commands and the shared editor/presenter, driven through actual controls.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_actions::deck::{self, Item};
use nodecraft_actions::layers::{Align, Content};
use nodecraft_types::Unsigned;
use serde_json::Value as Json;

const DECK: NodeId = NodeId::from_bytes([69; 16]);

fn text_layer(id: &str, name: &str, text: &str, font: &str, y: f64, size: f64) -> nodecraft_actions::layers::Layer {
    let mut layer = deck::source_layer(id, name, (320.0, y), (550.0, 100.0));
    layer.content =
        Content::Text { text: text.into(), font: font.into(), size, line_spacing: 1.0, tracking: 0.0, align: Align::Center, color: "#18283d".into() };
    layer
}

fn populate(app: &mut NodeCraftApp, family: &str) {
    let canvas = app.canvas;
    let session = &mut app.session;
    session.dispatch("deck.create", &json!({"canvas": canvas, "id": DECK, "pos": {"x": 0, "y": 0}, "title": "Public fonts", "nodes": []})).unwrap();
    for (name, value) in [("width", 640), ("height", 360)] {
        session.dispatch("node.param_set", &json!({"canvas": canvas, "id": DECK, "name": name, "value": Value::Unsigned(Unsigned(value))})).unwrap();
    }
    let layers = vec![
        text_layer("heading", "Headline", "A new chapter", family, 135.0, 48.0),
        text_layer("caption", "Caption", "Readable. Saved. Shared.", "Missing public font", 230.0, 28.0),
    ];
    session.dispatch("deck.slide_add", &json!({"canvas": canvas, "id": DECK, "slide": {"id": "intro", "name": "Introduction", "layers": layers}})).unwrap();
}

fn chosen_font(app: &NodeCraftApp) -> String {
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&DECK).unwrap();
    let deck = nodecraft_engine::deck_cmds::stored(node).unwrap();
    match &deck.slides[0].layers[0] {
        Item::Layer(nodecraft_actions::layers::Layer { content: Content::Text { font, .. }, .. }) => font.clone(),
        _ => panic!("headline text"),
    }
}

fn lito() -> Vec<u8> {
    let mut bytes = include_bytes!("../../../../assets/fonts/Lato-Regular.ttf").to_vec();
    for (from, to) in [(&b"\0L\0a\0t\0o"[..], &b"\0L\0i\0t\0o"[..]), (b"Lato", b"Lito")] {
        let mut at = 0;
        while let Some(found) = bytes[at..].windows(from.len()).position(|window| window == from) {
            bytes[at + found..at + found + from.len()].copy_from_slice(to);
            at += found + from.len();
        }
    }
    bytes
}

fn headless(theme: crate::theme::ThemeKind) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    app.theme = theme;
    populate(&mut app, "Original public font");
    harness_for(app, theme)
}

fn harness_for(app: NodeCraftApp, theme: crate::theme::ThemeKind) -> Harness<'static, NodeCraftApp> {
    let mut h = Harness::builder().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h.state_mut().open_deck_editor(DECK);
    h.run_steps(3);
    h.get_by_label("Headline").click();
    h.run_steps(3);
    h
}

fn listing(h: &mut Harness<'static, NodeCraftApp>) -> Json {
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("deck.list", &json!({"canvas": canvas, "id": DECK})).unwrap()
}

#[cfg(not(target_arch = "wasm32"))]
fn pending(h: &mut Harness<'static, NodeCraftApp>, revision: u64) {
    let ed = h.state_mut().deck_editor.as_mut().unwrap().layers_mut().unwrap();
    ed.picking = Some(super::layer_fonts::Picking::ready(Some("src-heading".into()), lito()));
    ed.picking_revision = Some(revision);
}

#[test]
fn deck693_pointer_font_choices_and_presenter_fallback_work_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = headless(theme);
        let canvas = h.state().canvas;
        nodecraft_engine::deck_cmds::fonts::import(&mut h.state_mut().session, canvas, DECK, &lito(), None).unwrap();
        h.run_steps(3);
        h.get_by_label("Font").click();
        h.run_steps(2);
        h.get_by_label("Lito").click();
        h.run_steps(3);
        assert_eq!(chosen_font(h.state()), "Lito");
        h.get_by_label("Undo").click();
        h.run_steps(3);
        assert_eq!(chosen_font(h.state()), "Original public font");
        h.get_by_label("Redo").click();
        h.run_steps(3);
        assert_eq!(chosen_font(h.state()), "Lito");
        let revision = h.state().session.revision();
        let hash: nodecraft_types::Blake3 = serde_json::from_value(listing(&mut h)["fonts"][0]["font"]["hash"].clone()).unwrap();
        h.get_by_label("Present").click();
        h.run_steps(3);
        assert!(h.query_by_label("Using Lato for Missing public font").is_some());
        assert!(h.state().presenter.as_ref().unwrap().font_files.bytes.get(&hash).is_some_and(Option::is_some));
        assert_eq!(h.state().session.revision(), revision, "presenting and fallback never edit text or history");
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn deck693_a_completed_font_pick_imports_then_assigns_as_two_valid_undo_steps() {
    let mut h = headless(crate::theme::ThemeKind::Dark);
    let revision = h.state().session.revision();
    pending(&mut h, revision);
    h.run_steps(3);
    assert_eq!(chosen_font(h.state()), "Lito");
    assert_eq!(h.state().session.revision(), revision + 2);
    assert!(h.state().notice.is_none(), "{:?}", h.state().notice);
    h.get_by_label("Undo").click();
    h.run_steps(3);
    assert_eq!(chosen_font(h.state()), "Original public font");
    assert_eq!(listing(&mut h)["fonts"].as_array().unwrap().len(), 1);
    h.get_by_label("Undo").click();
    h.run_steps(3);
    assert!(listing(&mut h)["fonts"].as_array().unwrap().is_empty());
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn deck693_pending_import_cannot_overtake_a_real_undo_or_a_closing_editor() {
    for close in [false, true] {
        let mut h = headless(crate::theme::ThemeKind::Dark);
        if close {
            // With a selected layer, Escape first clears that selection. Only a second
            // Escape closes the editor, so prepare that state before the read is ready.
            h.key_press(egui::Key::Escape);
            h.run_steps(3);
            assert!(h.state().deck_editor.as_ref().unwrap().layers().unwrap().selected.is_empty());
        }
        let canvas = h.state().canvas;
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas": canvas, "id": DECK, "name": "width", "value": Value::Unsigned(Unsigned(700))}))
            .unwrap();
        h.run_steps(3);
        let revision = h.state().session.revision();
        let assets = h.state().session.assets().len();
        pending(&mut h, revision);
        // Harness::key_press_modifiers runs a separate modifiers-only frame first. Put the
        // key and its modifiers directly in the same raw input as the ready read instead.
        let modifiers = if close { egui::Modifiers::NONE } else { egui::Modifiers::COMMAND };
        h.input_mut().events.push(egui::Event::ModifiersChanged(modifiers));
        h.input_mut().events.push(egui::Event::Key {
            key: if close { egui::Key::Escape } else { egui::Key::Z },
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
        h.run_steps(3);
        assert!(listing(&mut h)["fonts"].as_array().unwrap().is_empty());
        assert_eq!(h.state().session.assets().len(), assets, "a superseded pick never imports bytes");
        if close {
            assert!(h.state().deck_editor.is_none());
            assert_eq!(h.state().session.revision(), revision);
        } else {
            assert_eq!(listing(&mut h)["width"], 640);
            assert_eq!(chosen_font(h.state()), "Original public font");
            assert!(h.state().notice.as_deref().is_some_and(|message| message.contains("changed")));
        }
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn deck693_ready_font_pick_is_discarded_in_the_same_frame_as_a_thumbnail_switch() {
    let mut h = headless(crate::theme::ThemeKind::Dark);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("deck.slide_add", &json!({"canvas": canvas, "id": DECK, "slide": {"id": "next", "name": "Next", "layers": []}})).unwrap();
    h.run_steps(3);
    // The harness runs hover, press and release in separate frames. Make the read ready
    // between the press and release, so its first poll shares the real thumbnail click.
    let point = h.get_by_label("2 · Next").rect().center();
    h.hover_at(point);
    h.drag_at(point);
    h.run_steps(1);
    let revision = h.state().session.revision();
    let assets = h.state().session.assets().len();
    pending(&mut h, revision);
    h.drop_at(point);
    h.run_steps(3);
    assert_eq!(h.state().deck_editor.as_ref().unwrap().slide(), Some("next"));
    assert!(listing(&mut h)["fonts"].as_array().unwrap().is_empty());
    assert_eq!(chosen_font(h.state()), "Original public font");
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.assets().len(), assets, "switching slides drops the ready read before importing its bytes");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn deck693_reimport_reloads_a_missing_font_and_redraws_without_a_graph_revision() {
    let mut app = NodeCraftApp::headless().unwrap();
    populate(&mut app, "Lito");
    let hash = app.session.import_asset(&lito()).unwrap();
    let mut project = app.session.document().clone();
    let mut node = project.canvases().get(&app.canvas).unwrap().nodes().get(&DECK).unwrap().clone();
    let font = nodecraft_types::AssetRef { hash, mime: "font/ttf".parse().unwrap(), meta: Default::default() };
    node.config.insert(nodecraft_nodes::layers::FONTS_CONFIG.into(), Value::List(vec![Value::List(vec![Value::Text("Lito".into()), Value::Media(font)])]));
    nodecraft_graph::Change::SetNode { canvas: app.canvas, node }.apply(&mut project).unwrap();
    // A loaded reference whose payload has not arrived yet. Re-import supplies the same hash,
    // so neither adding it nor assigning its already-requested name changes the graph.
    app.session = nodecraft_engine::Session::new(project).unwrap();
    let mut h = harness_for(app, crate::theme::ThemeKind::Dark);
    let old_key = h.state().deck_editor.as_ref().unwrap().layers().unwrap().textures.get("src-heading").unwrap().key;
    let revision = h.state().session.revision();
    pending(&mut h, revision);
    h.run_steps(3);
    assert_eq!(h.state().session.revision(), revision);
    let editor = h.state().deck_editor.as_ref().unwrap().layers().unwrap();
    assert!(editor.font_files.bytes.get(&hash).is_some_and(Option::is_some));
    assert_ne!(editor.textures.get("src-heading").unwrap().key, old_key, "a formerly missing font invalidates the Lato fallback texture");
    assert!(h.state().notice.is_none(), "{:?}", h.state().notice);
}

#[test]
#[ignore = "actual GPU capture with a locally installed font; DECK693_RENDER_DIR/DECK693_FONT_FILE"]
fn deck693_visual() {
    let output = std::path::PathBuf::from(std::env::var_os("DECK693_RENDER_DIR").unwrap());
    let font_file = std::path::PathBuf::from(std::env::var_os("DECK693_FONT_FILE").unwrap());
    let baseline = std::env::var_os("DECK693_BASELINE").is_some();
    let bytes = std::fs::read(font_file).unwrap();
    let info = nodecraft_actions::layers::font_info(&bytes).unwrap();
    assert!(!nodecraft_actions::layers::same_name(&info.name, "Lato"));
    std::fs::create_dir_all(&output).unwrap();
    let mut app = NodeCraftApp::headless().unwrap();
    app.theme = crate::theme::ThemeKind::Dark;
    populate(&mut app, &info.name);
    if !baseline {
        let canvas = app.canvas;
        let hash = app.session.import_asset(&bytes).unwrap();
        let revision = app.session.revision();
        app.session
            .dispatch("deck.font_add", &json!({"canvas": canvas, "id": DECK, "font": {"hash": hash, "mime": info.mime}, "expected_revision": revision}))
            .unwrap();
    }
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h.state_mut().open_deck_editor(DECK);
    h.run_steps(3);
    h.get_by_label("Headline").click();
    h.run_steps(3);
    if baseline {
        assert!(h.query_by_label("Font").is_none());
        assert!(h.query_by_label(&format!("{}: decks draw text in Lato for now.", info.name)).is_some());
    } else {
        h.get_by_label("Font").click();
        h.run_steps(2);
        h.get_by_label("Lato").click();
        h.run_steps(3);
        assert_eq!(chosen_font(h.state()), "Lato");
        h.get_by_label("Font").click();
        h.run_steps(2);
        h.get_by_label(&info.name).click();
        h.run_steps(3);
        assert_eq!(chosen_font(h.state()), info.name);
        assert!(h.state().notice.is_none(), "{:?}", h.state().notice);
    }
    h.render().unwrap().save(output.join(if baseline { "before.png" } else { "after.png" })).unwrap();
    if !baseline {
        h.get_by_label("Present").click();
        h.run_steps(3);
        assert!(h.state().presenter.is_some());
        assert!(h.query_by_label("Using Lato for Missing public font").is_some());
        h.render().unwrap().save(output.join("after-fallback.png")).unwrap();
    }
}
