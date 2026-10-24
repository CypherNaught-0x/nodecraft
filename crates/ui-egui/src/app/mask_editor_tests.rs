//! The mask editor (#102), driven through the card's context menu and real pointer strokes.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const IMAGE: NodeId = NodeId::from_bytes([41; 16]);

fn with_image() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let png = nodecraft_media::encode_png(16, 12, &[120; 16 * 12 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[IMAGE]).unwrap();
    h.run();
    h
}

/// Right-clicks just inside node `id`'s card, panning it into view first.
pub(super) fn right_click(h: &mut Harness<'static, NodeCraftApp>, id: NodeId) {
    let card = |app: &NodeCraftApp| {
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        let descriptor = app.session.kinds().get(&node.kind).ok().map(|k| k.descriptor());
        card::world_rect(node, descriptor, app.theme.tokens())
    };
    let world = card(h.state());
    let app = h.state_mut();
    app.view.pan = app.viewport.center() - app.viewport.min - world.center().to_vec2() * app.view.zoom;
    h.run_steps(3);
    let pos = h.state().view.rect_to_screen(h.state().viewport.min, card(h.state())).min + Vec2::splat(6.0);
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
    }
    h.run_steps(3);
}

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<nodecraft_graph::Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}

fn of_kind(h: &Harness<'static, NodeCraftApp>, kind: &str) -> Option<nodecraft_graph::Node> {
    nodes(h).into_iter().find(|n| n.kind == kind)
}

/// Opens the editor from the image card's menu.
fn opened() -> Harness<'static, NodeCraftApp> {
    let mut h = with_image();
    right_click(&mut h, IMAGE);
    h.get_by_label_contains("Image tools").hover();
    h.run();
    h.get_by_label("Inpaint…").click();
    h.run_steps(3);
    h.run_steps(3);
    assert!(h.state().mask_editor.is_some(), "{:?}", h.state().notice);
    h
}

/// Drags across the painting area from `from` to `to`, as fractions of it.
fn stroke(h: &mut Harness<'static, NodeCraftApp>, from: Vec2, to: Vec2) {
    let rect = h.get_by_label("Mask painting area").rect();
    let at = |f: Vec2| rect.min + rect.size() * f;
    let start = at(from);
    h.hover_at(start);
    h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 1..=10 {
        h.event(egui::Event::PointerMoved(at(from + (to - from) * (i as f32 / 10.0))));
        h.step();
    }
    let end = at(to);
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
}

fn strokes(h: &Harness<'static, NodeCraftApp>) -> nodecraft_actions::mask::Strokes {
    nodecraft_engine::mask_cmds::strokes(&of_kind(h, nodecraft_nodes::mask::ID).unwrap()).unwrap()
}

#[test]
fn the_menu_adds_expand_and_mask_cards_and_a_drag_paints_one_stroke() {
    let mut h = opened();
    assert!(of_kind(&h, "image.expand").is_some());
    stroke(&mut h, Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5));
    let painted = strokes(&h);
    assert_eq!(painted.strokes.len(), 1, "{:?}", h.state().notice);
    let points = &painted.strokes[0].points;
    assert!(points.len() >= 3, "{points:?}");
    assert!((points[0][0] - 0.2).abs() < 0.05 && (points[0][1] - 0.5).abs() < 0.05, "{points:?}");
    assert!(points.last().unwrap()[0] > 0.7, "{points:?}");
    assert!(h.state().mask_editor.as_ref().unwrap().overlay.is_some(), "the stroke is drawn over the image");

    h.get_by_label("Eraser").click();
    h.run();
    stroke(&mut h, Vec2::new(0.5, 0.5), Vec2::new(0.5, 0.5));
    let erased = strokes(&h);
    assert_eq!(erased.strokes.len(), 2);
    assert!(erased.strokes[1].erase);

    h.get_by_label("Undo stroke").click();
    h.run();
    assert_eq!(strokes(&h).strokes.len(), 1);
    h.get_by_label("Clear").click();
    h.run();
    assert!(strokes(&h).strokes.is_empty());
}

#[test]
fn create_edit_wires_a_generation_card_and_closes() {
    let mut h = opened();
    stroke(&mut h, Vec2::new(0.3, 0.3), Vec2::new(0.6, 0.6));
    h.state_mut().mask_editor.as_mut().unwrap().prompt = "a red door".into();
    h.run();
    h.get_by_label("Create edit").click();
    h.run();
    assert!(h.state().mask_editor.is_none(), "{:?}", h.state().notice);
    let edit = of_kind(&h, nodecraft_nodes::generation::KIND).expect("an edit card");
    assert_eq!(edit.params.get(nodecraft_nodes::generation::PROMPT), Some(&Value::Text("a red door".into())));
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    let ports: BTreeSet<String> = canvas.edges().values().filter(|e| e.to.node == edit.id).map(|e| e.to.port.clone()).collect();
    assert_eq!(ports, ["image".to_owned(), "mask".to_owned()].into());
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), vec![edit.id]);
}

#[test]
fn the_editor_reopens_from_the_mask_card_and_closes_when_the_mask_goes() {
    let mut h = opened();
    h.get_by_label("Close").click();
    h.run();
    assert!(h.state().mask_editor.is_none());
    let mask = of_kind(&h, nodecraft_nodes::mask::ID).unwrap().id;
    right_click(&mut h, mask);
    assert!(h.query_by_label_contains("Image tools").is_none(), "a mask isn't masked again");
    h.get_by_label("Paint mask…").click();
    h.run();
    assert_eq!(h.state().mask_editor.as_ref().map(|e| e.mask), Some(mask));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert!(h.state().mask_editor.is_none(), "undoing the cards closes the editor");
}

#[test]
fn margins_set_elsewhere_show_in_the_editor_and_the_frame_widens() {
    let mut h = opened();
    let expand = of_kind(&h, "image.expand").unwrap().id;
    let width = h.get_by_label("Mask painting area").rect().width();
    let canvas = h.state().canvas;
    h.state_mut()
        .session
        .dispatch("node.param_set", &json!({"canvas": canvas, "id": expand, "name": "right", "value": {"type": "unsigned", "value": "8"}}))
        .unwrap();
    h.run();
    use egui_kittest::kittest::NodeT;
    let right = h.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Right").accesskit_node().value();
    assert_eq!(right.as_deref(), Some("8 px"), "the margin shows the document's value");
    let height = h.get_by_label("Mask painting area").rect().height();
    // 16×12 becomes 24×12: the frame's aspect follows the margins.
    let rect = h.get_by_label("Mask painting area").rect();
    assert!((rect.width() / rect.height() - 2.0).abs() < 0.05, "{rect:?} was {width} wide");
    assert!(height > 0.0);
}

#[test]
#[ignore = "manual public-safe capture; MASK102_RENDER_DIR"]
fn mask102_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("MASK102_RENDER_DIR").unwrap());
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "mask102-dark.png"), (crate::theme::ThemeKind::Light, "mask102-light.png")] {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let (w, h_px) = (256u32, 160u32);
        let pixels: Vec<u8> = (0..h_px).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h_px) as u8, 160, 255])).collect();
        let png = nodecraft_media::encode_png(w, h_px, &pixels, &Default::default()).unwrap();
        let canvas = h.state().canvas;
        let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
        import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "gradient.png", bytes: &png }], &[IMAGE]).unwrap();
        h.run_steps(4);
        right_click(&mut h, IMAGE);
        h.get_by_label_contains("Image tools").hover();
        h.run();
        h.get_by_label("Inpaint…").click();
        h.run_steps(4);
        let expand = of_kind(&h, "image.expand").unwrap().id;
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas": canvas, "id": expand, "name": "right", "value": {"type": "unsigned", "value": "64"}}))
            .unwrap();
        h.run_steps(4);
        stroke(&mut h, Vec2::new(0.25, 0.35), Vec2::new(0.55, 0.6));
        h.state_mut().mask_editor.as_mut().unwrap().prompt = "a lighthouse on the horizon".into();
        // Previews decode off the frame; give them time.
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            h.step();
        }
        h.render().unwrap().save(dir.join(name)).unwrap();
    }
}

#[test]
fn typing_a_margin_keeps_the_painted_stroke_over_the_same_pixels() {
    let mut h = opened();
    stroke(&mut h, Vec2::new(0.25, 0.5), Vec2::new(0.5, 0.5));
    let before = strokes(&h).strokes[0].points.clone();
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Right").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Right").type_text("16");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    let expand = of_kind(&h, "image.expand").unwrap();
    assert_eq!(nodecraft_engine::mask_cmds::margins(&expand).right, 16, "{:?}", h.state().notice);
    // 16 wide becomes 32: every point is half as far across, and stays at the same height.
    let after = strokes(&h).strokes[0].points.clone();
    for (old, new) in before.iter().zip(&after) {
        assert!((new[0] - old[0] / 2.0).abs() < 1e-9 && (new[1] - old[1]).abs() < 1e-9, "{old:?} → {new:?}");
    }
    // One undo puts both back.
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(nodecraft_engine::mask_cmds::margins(&of_kind(&h, "image.expand").unwrap()).right, 0);
    assert_eq!(strokes(&h).strokes[0].points, before);
}

/// Imports another image of `w`×`h` and wires it under the Expand card in place of the first.
fn swap_image(h: &mut Harness<'static, NodeCraftApp>, w: u32, height: u32) {
    let other = NodeId::from_bytes([42; 16]);
    let png = nodecraft_media::encode_png(w, height, &vec![60; (w * height * 4) as usize], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: 200.0, y: 400.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "other.png", bytes: &png }], &[other]).unwrap();
    let expand = of_kind(h, "image.expand").unwrap().id;
    let document = h.state().session.document();
    let old = document.canvases().get(&canvas).unwrap().edges().values().find(|e| e.to.node == expand && e.to.port == "image").unwrap().id;
    let wire = json!({"id": EdgeId::from_bytes([43; 16]), "from": {"node": other, "port": "out"}, "to": {"node": expand, "port": "image"}, "order": 0});
    h.state_mut().session.dispatch("edge.rewire", &json!({"canvas": canvas, "edge": wire, "replace": [old]})).unwrap();
    h.run_steps(3);
}

#[test]
fn a_reshaped_image_badges_the_mask_and_the_editor_fits_the_strokes() {
    let mut h = opened();
    stroke(&mut h, Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5));
    assert_eq!(strokes(&h).over.map(|o| (o.width, o.height)), Some((16, 12)), "painting records the image");
    h.get_by_label("Close").click();
    h.run_steps(3);
    assert!(h.state().mask_editor.is_none());

    // 16×12 → 32×12: the strokes would stretch, so the card says so and opens the editor.
    swap_image(&mut h, 32, 12);
    let mask = of_kind(&h, nodecraft_nodes::mask::ID).unwrap().id;
    let world = {
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&mask).unwrap();
        let descriptor = app.session.kinds().get(&node.kind).ok().map(|k| k.descriptor());
        card::world_rect(node, descriptor, app.theme.tokens())
    };
    let app = h.state_mut();
    app.view.pan = app.viewport.center() - app.viewport.min - world.center().to_vec2() * app.view.zoom;
    // Let the canvas lay out the badge at its settled pan position before clicking it. A fixed
    // frame count can leave AccessKit with a stale rect on slower runners.
    let badge = "Mask image reshaped: keep or fit the strokes";
    let mut previous_rect = None;
    let mut badge_settled = false;
    for _ in 0..120 {
        h.step();
        let rect = h.query_by_label(badge).map(|node| node.rect());
        if rect.is_some() && rect == previous_rect {
            badge_settled = true;
            break;
        }
        previous_rect = rect;
    }
    assert!(badge_settled, "the reshaped-mask badge did not settle after the canvas pan");
    h.get_by_label(badge).click();
    // The banner's click handler applies the choice synchronously, so these frames only deliver
    // the click; unlike the wait above, nothing here is racing a background worker.
    h.run_steps(3);
    assert_eq!(h.state().mask_editor.as_ref().map(|e| e.mask), Some(mask));
    h.get_by_label("Fit to new image").click();
    h.run_steps(3);
    let fitted = strokes(&h);
    assert_eq!(fitted.over.map(|o| (o.width, o.height)), Some((32, 12)), "{:?}", h.state().notice);
    // The old image sits in the middle half of the new one.
    let x = fitted.strokes[0].points[0][0];
    assert!((x - (0.25 + 0.2 * 0.5)).abs() < 0.05, "{x}");
    assert!(h.query_by_label("Fit to new image").is_none(), "the banner goes once the strokes are fitted");
    assert!(h.query_by_label("Mask image reshaped: keep or fit the strokes").is_none());
}

#[test]
fn a_new_image_of_the_same_shape_offers_to_keep_the_strokes() {
    let mut h = opened();
    stroke(&mut h, Vec2::new(0.2, 0.5), Vec2::new(0.8, 0.5));
    let before = strokes(&h).strokes;
    swap_image(&mut h, 32, 24);
    assert!(h.query_by_label("Fit to new image").is_none(), "same shape: nothing to fit");
    h.get_by_label("Keep strokes").click();
    h.run_steps(3);
    let kept = strokes(&h);
    assert_eq!(kept.strokes, before);
    assert_eq!(kept.over.map(|o| (o.width, o.height)), Some((32, 24)));
    assert!(h.query_by_label("Keep strokes").is_none());
}
