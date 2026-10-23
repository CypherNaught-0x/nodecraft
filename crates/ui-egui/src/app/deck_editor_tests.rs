//! Editing a deck's slides (#694), driven through the card's context menu and real pointer and
//! key events on the slide sorter and the stage.
use super::mask_editor_tests::right_click;
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_actions::deck::{Deck, Item};

const TEXT: NodeId = NodeId::from_bytes([71; 16]);
const DECK: NodeId = NodeId::from_bytes([72; 16]);

/// A text card and a deck of one slide showing it.
fn with_deck() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let canvas = h.state().canvas;
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": TEXT, "kind": "source.text", "pos": {"x": -300, "y": -110}})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": TEXT, "name": "text", "value": Value::Text("Hello deck".into())})).unwrap();
    let params = json!({"canvas": canvas, "id": DECK, "pos": {"x": 100, "y": -110}, "title": "Talk", "nodes": [TEXT], "fields": []});
    session.dispatch("deck.create", &params).unwrap();
    h.run();
    h
}

fn open(h: &mut Harness<'static, NodeCraftApp>) {
    // A slow runner can drop the menu click when a background result lands in the same frame.
    for _ in 0..20 {
        if h.query_by_label("Edit slides…").is_none() {
            right_click(h, DECK);
        }
        h.get_by_label("Edit slides…").click();
        h.run();
        if h.state().deck_editor.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(h.state().deck_editor.is_some(), "{:?}", h.state().notice);
}

fn stored(h: &Harness<'static, NodeCraftApp>) -> Deck {
    let node = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&DECK).unwrap().clone();
    nodecraft_engine::deck_cmds::stored(&node).unwrap()
}

fn names(h: &Harness<'static, NodeCraftApp>) -> Vec<String> {
    stored(h).slides.iter().map(|s| s.name.clone()).collect()
}

fn picked(h: &Harness<'static, NodeCraftApp>) -> String {
    h.state().deck_editor.as_ref().and_then(|e| e.slide()).unwrap_or_default().to_owned()
}

fn layer_ids(h: &Harness<'static, NodeCraftApp>, slide: usize) -> Vec<String> {
    stored(h).slides[slide].layers.iter().filter_map(|i| i.id().map(str::to_owned)).collect()
}

/// Frame pixels on screen, as the stage last drew them.
fn screen(h: &Harness<'static, NodeCraftApp>, (x, y): (f64, f64)) -> Pos2 {
    let editor = h.state().deck_editor.as_ref().and_then(|e| e.layers()).unwrap();
    editor.origin + Vec2::new((x * editor.scale) as f32, (y * editor.scale) as f32)
}

fn drag_between(h: &mut Harness<'static, NodeCraftApp>, start: Pos2, end: Pos2) {
    h.hover_at(start);
    h.step();
    h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(start + (end - start) * (i as f32 / 8.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn slides_are_added_named_reordered_and_removed_from_the_sorter() {
    let mut h = with_deck();
    open(&mut h);
    assert_eq!(names(&h), ["Text"]);
    assert!(h.query_by_label("1 · Text").is_some(), "the thumbnail shows the slide's place and name");
    assert!(h.query_by_label("Layer stage").is_some());

    h.get_by_label("Add slide").click();
    h.run_steps(3);
    assert_eq!(stored(&h).slides.len(), 2, "{:?}", h.state().notice);
    assert_eq!(picked(&h), stored(&h).slides[1].id, "the new slide comes after the picked one and is picked");

    h.get_by_label("Name").click();
    h.run();
    h.get_by_label("Name").type_text("Closing");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(names(&h), ["Text", "Closing"], "{:?}", h.state().notice);

    // Up, then a thumbnail dragged below the other one.
    h.get_by_label("Up").click();
    h.run_steps(3);
    assert_eq!(names(&h), ["Closing", "Text"]);
    let (from, to) = (h.get_by_label("1 · Closing").rect().center(), h.get_by_label("2 · Text").rect());
    drag_between(&mut h, from, Pos2::new(to.center().x, to.max.y - 2.0));
    assert_eq!(names(&h), ["Text", "Closing"], "{:?}", h.state().notice);

    // Click picks; Remove takes the picked slide, and undo brings it back.
    h.get_by_label("1 · Text").click();
    h.run_steps(2);
    assert_eq!(picked(&h), stored(&h).slides[0].id);
    h.get_by_label("Remove").click();
    h.run_steps(3);
    assert_eq!(names(&h), ["Closing"]);
    assert_eq!(picked(&h), stored(&h).slides[0].id, "its neighbour is picked");
    h.get_by_label("Undo").click();
    h.run_steps(3);
    assert_eq!(names(&h), ["Text", "Closing"]);
}

fn export(h: &mut Harness<'static, NodeCraftApp>, item: &str, answer: std::path::PathBuf) {
    h.state_mut().exports.answer = Some(answer);
    h.get_by_label("Export").click();
    h.run();
    h.get_by_label(item).click();
    h.run_steps(3);
}

#[test]
fn slides_export_as_a_pdf_or_pngs_from_the_editor() {
    let mut h = with_deck();
    open(&mut h);
    h.get_by_label("Add slide").click();
    h.run_steps(3);
    let dir = tempfile::tempdir().unwrap();

    export(&mut h, "Slides as PDF…", dir.path().join("talk.pdf"));
    let pdf = std::fs::read(dir.path().join("talk.pdf")).unwrap_or_else(|_| panic!("{:?}", h.state().notice));
    assert_eq!(nodecraft_media::probe_document(&pdf, &Default::default()).unwrap().pages, 2, "one page per slide");
    assert_eq!(h.state().notice.as_deref(), Some("Exported talk.pdf."));

    // PNGs go into a folder, numbered past files already there.
    std::fs::write(dir.path().join("Talk-1.png"), b"mine").unwrap();
    export(&mut h, "Slides as PNGs…", dir.path().to_path_buf());
    assert_eq!(h.state().notice.as_deref(), Some("Exported 2 files."));
    assert_eq!(std::fs::read(dir.path().join("Talk-1.png")).unwrap(), b"mine");
    for name in ["Talk-2.png", "Talk-3.png"] {
        let (_, image) = nodecraft_media::decode_image(&std::fs::read(dir.path().join(name)).unwrap(), &Default::default()).unwrap();
        assert_eq!((image.width(), image.height()), (1920, 1080), "slides are drawn at the deck's size");
    }

    export(&mut h, "This slide as PNG…", dir.path().join("one.png"));
    assert!(dir.path().join("one.png").exists(), "{:?}", h.state().notice);
    assert!(h.state().deck_editor.is_some(), "the editor stays open");
}

#[test]
fn a_slide_takes_text_a_drag_in_one_undo_step_and_delete() {
    let mut h = with_deck();
    open(&mut h);
    let source = layer_ids(&h, 0);
    assert_eq!(source.len(), 1, "the slide shows the text card");
    h.get_by_label("Add text").click();
    h.run_steps(3);
    let ids = layer_ids(&h, 0);
    assert_eq!(ids.len(), 2, "{:?}", h.state().notice);
    let text = ids[1].clone();
    let before = stored(&h).slides[0].layers[1].layer().unwrap().clone();

    let (from, to) = (screen(&h, (before.x, before.y)), screen(&h, (before.x + 50.0, before.y + 40.0)));
    drag_between(&mut h, from, to);
    let moved = stored(&h).slides[0].layers[1].layer().unwrap().clone();
    assert!((moved.x - before.x - 50.0).abs() < 1.0 && (moved.y - before.y - 40.0).abs() < 1.0, "{moved:?} {:?}", h.state().notice);
    h.get_by_label("Undo").click();
    h.run_steps(3);
    let back = stored(&h).slides[0].layers[1].layer().unwrap().clone();
    assert_eq!((back.x, back.y), (before.x, before.y), "the drag is one undo step");

    // Delete takes the selected layer; Escape then clears the selection, then closes.
    h.get_by_label("Redo").click();
    h.run_steps(3);
    assert_eq!(h.state().deck_editor.as_ref().and_then(|e| e.layers()).unwrap().selected, [text]);
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    assert_eq!(layer_ids(&h, 0), source);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().deck_editor.is_none(), "Escape with nothing selected closes");
}

#[test]
fn each_edit_is_checked_against_the_revision_before_it() {
    let mut h = with_deck();
    open(&mut h);
    let (canvas, slide) = (h.state().canvas, stored(&h).slides[0].id.clone());
    let add = |id: &str| ("deck.slide_add", json!({"canvas": canvas, "id": DECK, "slide": {"id": id, "name": "", "layers": []}, "index": 1}));

    // Two edits in one frame: the second is checked against the revision the first made.
    let listed = h.state().session.revision();
    let rename = ("deck.slide_update", json!({"canvas": canvas, "id": DECK, "slide": "s-new", "name": "Second"}));
    h.state_mut().deck_run(listed, vec![add("s-new"), rename]);
    assert_eq!(names(&h), ["Text", "Second"], "{:?}", h.state().notice);

    // An edit made from a listing older than the document is refused and says so.
    let stale = h.state().session.revision();
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas, "id": TEXT, "name": "text", "value": Value::Text("Changed".into())})).unwrap();
    h.state_mut().deck_run(stale, vec![("deck.slide_remove", json!({"canvas": canvas, "id": DECK, "slide": slide}))]);
    assert_eq!(names(&h), ["Text", "Second"], "a stale edit changes nothing");
    assert_eq!(h.state().notice.as_deref(), Some("The deck changed meanwhile; try again."));
}

#[test]
fn undo_brings_back_the_last_slide_and_a_drag_outside_the_sorter_cancels() {
    let mut h = with_deck();
    open(&mut h);
    h.get_by_label("Remove").click();
    h.run_steps(3);
    assert!(stored(&h).slides.is_empty());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(names(&h), ["Text"], "undo works with no slide to edit");

    h.get_by_label("Add slide").click();
    h.run_steps(3);
    h.get_by_label("Name").click();
    h.run();
    h.get_by_label("Name").type_text("Closing");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(names(&h), ["Text", "Closing"], "{:?}", h.state().notice);

    // Released over the stage: nothing moves, and no drop line stays behind.
    let from = h.get_by_label("2 · Closing").rect().center();
    let stage = h.get_by_label("Layer stage").rect().center();
    drag_between(&mut h, from, stage);
    assert_eq!(names(&h), ["Text", "Closing"]);
    assert!(h.state().deck_editor.as_ref().is_some_and(|e| e.moving.is_none()));

    // Dragged up, above the first slide.
    let (from, to) = (h.get_by_label("2 · Closing").rect().center(), h.get_by_label("1 · Text").rect());
    drag_between(&mut h, from, Pos2::new(to.center().x, to.min.y + 2.0));
    assert_eq!(names(&h), ["Closing", "Text"], "{:?}", h.state().notice);
}

#[test]
fn a_source_layer_is_added_from_the_canvas_and_layers_it_cant_read_are_kept() {
    let mut h = with_deck();
    // A newer NodeCraft stored a video layer on the first slide.
    let canvas = h.state().canvas;
    let mut project = h.state().session.document().clone();
    let mut node = project.canvases().get(&canvas).unwrap().nodes().get(&DECK).unwrap().clone();
    let mut deck = stored(&h);
    let video = json!({"id": "v", "name": "Video", "x": 10, "y": 10, "width": 100, "height": 100, "content": {"kind": "video", "clip": "abc"}});
    deck.slides[0].layers.insert(0, Item::Unsupported(video.clone()));
    node.config.insert(nodecraft_nodes::deck::CONFIG.into(), Value::Text(serde_json::to_string(&deck).unwrap()));
    nodecraft_graph::Change::SetNode { canvas, node }.apply(&mut project).unwrap();
    h.state_mut().session = nodecraft_engine::Session::new(project).unwrap();
    h.run();
    open(&mut h);
    assert!(h.query_by_label("1 layer here needs a newer NodeCraft; it stays as it is.").is_some());

    // Two text cards to choose from now: add the other one as a second source.
    let other = NodeId::from_bytes([73; 16]);
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": other, "kind": "source.text", "pos": {"x": -300, "y": 200}})).unwrap();
    session.dispatch("node.title", &json!({"canvas": canvas, "id": other, "title": "Notes"})).unwrap();
    h.run();
    h.get_by_label("Add source").click();
    h.run();
    let label = h
        .state()
        .session
        .document()
        .canvases()
        .get(&canvas)
        .unwrap()
        .nodes()
        .get(&other)
        .map(|n| if n.title.trim().is_empty() { n.kind.clone() } else { n.title.clone() })
        .unwrap();
    h.get_by_label(&label).click();
    h.run_steps(3);
    let deck = stored(&h);
    let layers = &deck.slides[0].layers;
    assert_eq!(layers.len(), 3, "{:?}", h.state().notice);
    assert_eq!(layers[0], Item::Unsupported(video.clone()), "the unreadable layer stays first, untouched");
    let added = layers[2].layer().unwrap();
    assert!(matches!(&added.content, nodecraft_actions::layers::Content::Source { origin, .. } if *origin == other.to_string()));

    // Lowering a layer moves it past the readable one below; the unreadable one stays put.
    h.get_by_label("Lower").click();
    h.run_steps(3);
    let deck = stored(&h);
    let ids: Vec<_> = deck.slides[0].layers.iter().map(|i| i.id().unwrap().to_owned()).collect();
    assert_eq!(ids, ["v", &added.id, layers[1].id().unwrap()], "{:?}", h.state().notice);
}

#[test]
#[ignore = "manual public-safe capture; DECK694_RENDER_DIR"]
fn deck694_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("DECK694_RENDER_DIR").unwrap());
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "deck694-dark"), (crate::theme::ThemeKind::Light, "deck694-light")] {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let canvas = h.state().canvas;
        let session = &mut h.state_mut().session;
        let words = "A quiet harbour at dawn: fishing boats, gulls and a lighthouse in soft morning light.";
        session.dispatch("node.create", &json!({"canvas": canvas, "id": TEXT, "kind": "source.text", "pos": {"x": -300, "y": -110}})).unwrap();
        session.dispatch("node.param_set", &json!({"canvas": canvas, "id": TEXT, "name": "text", "value": Value::Text(words.into())})).unwrap();
        session
            .dispatch("deck.create", &json!({"canvas": canvas, "id": DECK, "pos": {"x": 100, "y": -110}, "title": "Harbour", "nodes": [TEXT], "fields": []}))
            .unwrap();
        h.run();
        h.state_mut().open_deck_editor(DECK);
        h.run();
        h.get_by_label("Add text").click();
        h.run_steps(3);
        h.get_by_label("Add slide").click();
        h.run_steps(3);
        h.get_by_label("Add shape").click();
        h.run();
        h.get_by_label("Ellipse").click();
        h.run_steps(3);
        h.get_by_label("1 · Text").click();
        h.run_steps(3);
        h.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
    }
}

/// The editor's Export menu, and the PNG and PDF it writes, for the PR (#692).
#[test]
#[ignore = "renders with wgpu into DECK692_RENDER_DIR"]
fn deck692_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("DECK692_RENDER_DIR").unwrap());
    let app = NodeCraftApp::headless().unwrap();
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, h.state().theme);
    let canvas = h.state().canvas;
    let session = &mut h.state_mut().session;
    let words = "A quiet harbour at dawn: fishing boats, gulls and a lighthouse in soft morning light.";
    session.dispatch("node.create", &json!({"canvas": canvas, "id": TEXT, "kind": "source.text", "pos": {"x": -300, "y": -110}})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": TEXT, "name": "text", "value": Value::Text(words.into())})).unwrap();
    session
        .dispatch("deck.create", &json!({"canvas": canvas, "id": DECK, "pos": {"x": 100, "y": -110}, "title": "Harbour", "nodes": [TEXT], "fields": []}))
        .unwrap();
    h.run();
    h.state_mut().open_deck_editor(DECK);
    h.run();
    h.get_by_label("Add text").click();
    h.run_steps(3);
    h.get_by_label("Add shape").click();
    h.run();
    h.get_by_label("Ellipse").click();
    h.run_steps(3);
    export(&mut h, "This slide as PNG…", dir.join("Harbour.png"));
    export(&mut h, "Slides as PDF…", dir.join("Harbour.pdf"));
    h.get_by_label("Export").click();
    h.run_steps(3);
    h.render().unwrap().save(dir.join("deck692-menu.png")).unwrap();
}
