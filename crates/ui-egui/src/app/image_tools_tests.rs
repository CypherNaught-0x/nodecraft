//! The "Image tools" submenu (#103) through the card's context menu.
use super::tests::harness;
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::import::{Incoming, import_files};

const IMAGE: NodeId = NodeId::from_bytes([41; 16]);

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

/// Right-clicks just inside node `id`'s card and opens its Image tools.
fn open_tools(h: &mut Harness<'static, NodeCraftApp>, id: NodeId) {
    let pos = {
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        let descriptor = app.session.kinds().get(&node.kind).ok().map(|k| k.descriptor());
        app.view.rect_to_screen(app.viewport.min, card::world_rect(node, descriptor, app.theme.tokens())).min + Vec2::splat(6.0)
    };
    h.hover_at(pos);
    h.run();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    h.get_by_label_contains("Image tools").hover();
    h.run();
    h.run();
}

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<nodecraft_graph::Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}

fn fed_by_image(h: &Harness<'static, NodeCraftApp>, id: NodeId) -> bool {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().edges().values().any(|e| e.from.node == IMAGE && e.to.node == id)
}

#[test]
fn the_menu_lists_every_tool_and_greys_the_ones_no_model_can_do() {
    let mut h = with_image();
    open_tools(&mut h, IMAGE);
    for label in ["Inpaint…", "Expand…", "Upscale", "Split into grid…", "Color grade", "Make Element…", "Open in 3D"] {
        assert!(h.query_by_label_contains(label).is_some_and(|n| !n.accesskit_node().is_disabled()), "{label} is offered");
    }
    for label in ["Remove background", "Split into layers"] {
        assert!(h.query_by_label_contains(label).is_some_and(|n| n.accesskit_node().is_disabled()), "{label} waits for a model");
    }
}

#[test]
fn color_grade_adds_a_wired_grade_card_in_one_undo_step() {
    let mut h = with_image();
    let before = h.state().session.document().clone();
    open_tools(&mut h, IMAGE);
    h.get_by_label("Color grade").click();
    h.run();
    let grade = nodes(&h).into_iter().find(|n| n.kind == "image.grade").expect("a grade card");
    assert!(fed_by_image(&h, grade.id));
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), [grade.id], "the new card is selected");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn upscale_adds_a_model_card_without_running_it() {
    let mut h = with_image();
    open_tools(&mut h, IMAGE);
    h.get_by_label_contains("Upscale").click();
    h.run();
    let card = nodes(&h).into_iter().find(|n| n.kind == nodecraft_nodes::generation::KIND).expect("an image card");
    assert_eq!(card.params.get(nodecraft_nodes::generation::MODEL), Some(&Value::Text("fal.esrgan".into())));
    assert!(fed_by_image(&h, card.id), "{:?}", h.state().notice);
}

#[test]
fn open_in_3d_adds_a_3d_card() {
    let mut h = with_image();
    open_tools(&mut h, IMAGE);
    h.get_by_label_contains("Open in 3D").click();
    h.run();
    let card = nodes(&h).into_iter().find(|n| n.kind == nodecraft_nodes::generation::MESH_KIND).expect("a 3D card");
    assert!(fed_by_image(&h, card.id), "{:?}", h.state().notice);
}

#[test]
#[ignore = "manual public-safe capture; TOOLS103_RENDER_DIR"]
fn tools103_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("TOOLS103_RENDER_DIR").unwrap());
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "tools103-dark.png"), (crate::theme::ThemeKind::Light, "tools103-light.png")] {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1100.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let (w, h_px) = (320u32, 200u32);
        let pixels: Vec<u8> = (0..h_px).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h_px) as u8, 170, 255])).collect();
        let png = nodecraft_media::encode_png(w, h_px, &pixels, &Default::default()).unwrap();
        let canvas = h.state().canvas;
        let at = nodecraft_graph::Position { x: -300.0, y: -200.0 };
        import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "gradient.png", bytes: &png }], &[IMAGE]).unwrap();
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            h.step();
        }
        open_tools(&mut h, IMAGE);
        h.get_by_label_contains("Remove background").hover();
        h.run_steps(4);
        h.render().unwrap().save(dir.join(name)).unwrap();
    }
}

/// A generation card whose run made `count` 8×6 images.
fn generated(count: usize) -> (Harness<'static, NodeCraftApp>, NodeId) {
    let canvas = CanvasId::from_bytes([43; 16]);
    let card = NodeId::from_bytes([44; 16]);
    let png = nodecraft_media::encode_png(8, 6, &[90; 8 * 6 * 4], &Default::default()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([47; 16]), "Generated").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let image =
        Value::Media(nodecraft_types::AssetRef { hash: session.import_asset(&png).unwrap(), mime: "image/png".parse().unwrap(), meta: Default::default() });
    let mut node = session.kinds().get("generation.image").unwrap().descriptor().instantiate(card, &BTreeMap::new()).unwrap();
    node.pos = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    let made = nodecraft_types::GenerationId::from_bytes([45; 16]);
    node.history.push(nodecraft_graph::Generation::new(made, vec![Value::List(vec![image; count])]));
    node.active = Some(made);
    let mut project = session.document().clone();
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.import_asset(&png).unwrap();
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    assert!(h.query_by_label_contains("Unknown node").is_none(), "the card draws as a generation card");
    (h, card)
}

fn enabled(h: &Harness<'static, NodeCraftApp>, label: &str) -> bool {
    !h.get_by_label_contains(label).accesskit_node().is_disabled()
}

#[test]
fn a_generated_image_can_be_split_into_a_grid_and_fed_to_a_model_tool() {
    let (mut h, card) = generated(1);
    open_tools(&mut h, card);
    assert!(enabled(&h, "Split into grid"), "a generated image splits, as before the menu");
    assert!(enabled(&h, "Upscale") && enabled(&h, "Open in 3D"), "one image feeds a model tool");
    assert!(!enabled(&h, "Color grade") && !enabled(&h, "Inpaint"), "these take a single image, not a list");
}

#[test]
fn a_card_that_made_several_images_greys_the_model_tools() {
    let (mut h, card) = generated(2);
    open_tools(&mut h, card);
    assert!(enabled(&h, "Split into grid"), "splits the first image");
    assert!(!enabled(&h, "Upscale") && !enabled(&h, "Open in 3D"));
}
