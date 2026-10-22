//! Presenting (#122), driven through the card's context menu and real key and pointer events.
use super::mask_editor_tests::right_click;
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const IMAGE: NodeId = NodeId::from_bytes([61; 16]);
const TEXT: NodeId = NodeId::from_bytes([62; 16]);

/// A picture and, to its right, a text card, both selected.
fn with_cards() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let png = nodecraft_media::encode_png(16, 8, &[200; 16 * 8 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: -300.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[IMAGE]).unwrap();
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": TEXT, "kind": "source.text", "pos": {"x": 100, "y": -110}})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": TEXT, "name": "text", "value": Value::Text("Hello deck".into())})).unwrap();
    h.state_mut().selection.extend([IMAGE, TEXT], []);
    h.run();
    h
}

fn decks(h: &Harness<'static, NodeCraftApp>) -> Vec<NodeId> {
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    canvas.nodes().values().filter(|n| n.kind == nodecraft_nodes::deck::ID).map(|n| n.id).collect()
}

fn index(h: &Harness<'static, NodeCraftApp>) -> usize {
    h.state().presenter.as_ref().map(|p| p.index).unwrap_or_else(|| panic!("not presenting: {:?}", h.state().notice))
}

#[test]
fn presenting_a_selection_makes_a_deck_and_the_keys_move_through_it() {
    let mut h = with_cards();
    // A slow runner can drop the menu click when a background result lands in the same frame.
    for _ in 0..20 {
        if h.query_by_label("Present selection").is_none() {
            right_click(&mut h, IMAGE);
        }
        h.get_by_label("Present selection").click();
        h.run();
        if h.state().presenter.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let deck = decks(&h);
    assert_eq!(deck.len(), 1, "{:?}", h.state().notice);
    assert_eq!(index(&h), 0);
    let node = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&deck[0]).unwrap().clone();
    let slides = nodecraft_engine::deck_cmds::stored(&node).unwrap().slides;
    let title = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&IMAGE).unwrap().title.clone();
    assert_eq!(slides.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), [if title.is_empty() { "Image" } else { title.as_str() }, "Text"]);
    assert!(h.query_by_label(&format!("1 / 2 · {}", slides[0].name)).is_some(), "the place and the slide's name show");
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert_eq!(index(&h), 1);
    assert!(h.query_by_label("2 / 2 · Text").is_some());
    h.key_press(egui::Key::Space);
    h.run();
    assert_eq!(index(&h), 1, "the last slide stays");
    h.key_press(egui::Key::Home);
    h.run();
    assert_eq!(index(&h), 0);
    h.key_press(egui::Key::End);
    h.run();
    assert_eq!(index(&h), 1);
    h.get_by_label("Previous").click();
    h.run();
    assert_eq!(index(&h), 0);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().presenter.is_none());

    // The deck stays on the canvas, and presents again from its own menu.
    for _ in 0..20 {
        if h.query_by_label("Present").is_none() {
            right_click(&mut h, deck[0]);
        }
        h.get_by_label("Present").click();
        h.run();
        if h.state().presenter.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(index(&h), 0);
    // Removing the deck (undo) ends the presentation.
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert!(h.state().presenter.is_none());
    assert!(decks(&h).is_empty());
}

#[test]
#[ignore = "manual public-safe capture; DECK122_RENDER_DIR"]
fn deck122_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("DECK122_RENDER_DIR").unwrap());
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "deck122-dark"), (crate::theme::ThemeKind::Light, "deck122-light")] {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let (w, h_px) = (320u32, 200u32);
        let pixels: Vec<u8> = (0..h_px).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h_px) as u8, 170, 255])).collect();
        let png = nodecraft_media::encode_png(w, h_px, &pixels, &Default::default()).unwrap();
        let canvas = h.state().canvas;
        let at = nodecraft_graph::Position { x: -300.0, y: -110.0 };
        import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "gradient.png", bytes: &png }], &[IMAGE]).unwrap();
        let session = &mut h.state_mut().session;
        session.dispatch("node.create", &json!({"canvas": canvas, "id": TEXT, "kind": "source.text", "pos": {"x": 100, "y": -110}})).unwrap();
        let words = "A quiet harbour at dawn: fishing boats, gulls and a lighthouse in soft morning light.";
        session.dispatch("node.param_set", &json!({"canvas": canvas, "id": TEXT, "name": "text", "value": Value::Text(words.into())})).unwrap();
        h.state_mut().selection.extend([IMAGE, TEXT], []);
        h.run_steps(3);
        h.state_mut().present_selection();
        for (slide, suffix) in [(0, "image"), (1, "text")] {
            h.state_mut().presenter.as_mut().unwrap().index = slide;
            // Previews decode off the frame; give them time.
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                h.step();
            }
            h.render().unwrap().save(dir.join(format!("{name}-{suffix}.png"))).unwrap();
        }
    }
}
