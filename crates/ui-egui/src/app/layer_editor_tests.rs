//! The Layer Editor (#101), driven through the card's context menu and real pointer and key
//! events on its stage.
use super::mask_editor_tests::right_click;
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_actions::layers::Layers;
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_types::EdgeId;

const IMAGE: NodeId = NodeId::from_bytes([51; 16]);
const EDITOR: NodeId = NodeId::from_bytes([52; 16]);

fn unsigned(n: u64) -> serde_json::Value {
    serde_json::to_value(Value::Unsigned(nodecraft_types::Unsigned(n))).unwrap()
}

/// An image wired into a 400×200 Layer Editor card.
fn with_editor(h: &mut Harness<'static, NodeCraftApp>) {
    let png = nodecraft_media::encode_png(16, 8, &[200; 16 * 8 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: -200.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[IMAGE]).unwrap();
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": EDITOR, "kind": nodecraft_nodes::layers::ID, "pos": {"x": 200, "y": -110}})).unwrap();
    for (name, value) in [("width", 400), ("height", 200)] {
        session.dispatch("node.param_set", &json!({"canvas": canvas, "id": EDITOR, "name": name, "value": unsigned(value)})).unwrap();
    }
    let edge = json!({"id": EdgeId::from_bytes([53; 16]), "from": {"node": IMAGE, "port": "out"}, "to": {"node": EDITOR, "port": nodecraft_nodes::layers::INPUT}, "order": 1});
    session.dispatch("edge.connect", &json!({"canvas": canvas, "edge": edge})).unwrap();
    h.run();
}

/// Opens the editor from the Layer Editor card's menu.
fn opened() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    with_editor(&mut h);
    right_click(&mut h, EDITOR);
    h.get_by_label("Edit layers…").click();
    h.run_steps(3);
    assert!(h.state().layer_editor.is_some(), "{:?}", h.state().notice);
    h
}

fn editor_node(h: &Harness<'static, NodeCraftApp>) -> nodecraft_graph::Node {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&EDITOR).unwrap().clone()
}

fn stored(h: &Harness<'static, NodeCraftApp>) -> Layers {
    nodecraft_engine::layer_cmds::stored(&editor_node(h)).unwrap()
}

fn layer(h: &Harness<'static, NodeCraftApp>, id: &str) -> nodecraft_actions::layers::Layer {
    stored(h).get(id).cloned().unwrap_or_else(|| panic!("no layer {id}: {:?}", h.state().notice))
}

/// Frame pixels on screen, as the stage last drew them.
fn screen(h: &Harness<'static, NodeCraftApp>, (x, y): (f64, f64)) -> Pos2 {
    let editor = h.state().layer_editor.as_ref().unwrap();
    editor.origin + Vec2::new((x * editor.scale) as f32, (y * editor.scale) as f32)
}

/// Drags from `from` to `to` (frame pixels) on the stage.
fn drag(h: &mut Harness<'static, NodeCraftApp>, from: (f64, f64), to: (f64, f64)) {
    let (start, end) = (screen(h, from), screen(h, to));
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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.75
}

fn add_text(h: &mut Harness<'static, NodeCraftApp>) {
    h.get_by_label("Add text").click();
    h.run_steps(3);
    assert_eq!(h.state().layer_editor.as_ref().unwrap().selected, ["text-1".to_owned()]);
}

#[test]
fn the_menu_opens_the_editor_and_a_drag_moves_a_layer_in_one_undo_step() {
    let mut h = opened();
    assert!(h.query_by_label("Layer stage").is_some());
    assert!(stored(&h).layers.is_empty(), "the image layer is automatic until something changes");
    add_text(&mut h);
    let text = layer(&h, "text-1");
    assert_eq!((text.x, text.y, text.width), (200.0, 100.0, 240.0));
    assert_eq!(stored(&h).layers.len(), 2, "adding stores the image layer under the text");

    drag(&mut h, (200.0, 100.0), (250.0, 140.0));
    let moved = layer(&h, "text-1");
    assert!(close(moved.x, 250.0) && close(moved.y, 140.0), "{moved:?} {:?}", h.state().notice);
    let image = stored(&h).layers[0].clone();
    assert_eq!((image.x, image.y), (200.0, 100.0), "only the selected layer moves");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!((layer(&h, "text-1").x, layer(&h, "text-1").y), (200.0, 100.0));

    // A press on the image, outside the text, selects the image instead.
    drag(&mut h, (20.0, 20.0), (20.0, 20.0));
    assert_eq!(h.state().layer_editor.as_ref().unwrap().selected, std::slice::from_ref(&image.id));
}

#[test]
fn a_drag_snaps_the_layer_centre_to_the_frame_centre() {
    let mut h = opened();
    add_text(&mut h);
    drag(&mut h, (200.0, 100.0), (240.0, 140.0));
    // 2 px from the middle in x snaps; 40 px in y doesn't.
    drag(&mut h, (240.0, 140.0), (202.0, 140.0));
    let moved = layer(&h, "text-1");
    assert!(close(moved.x, 200.0), "{moved:?}");
    assert!(close(moved.y, 140.0));
}

#[test]
fn keys_nudge_and_delete_the_selection_and_escape_closes() {
    let mut h = opened();
    add_text(&mut h);
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(layer(&h, "text-1").x, 201.0);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(layer(&h, "text-1").y, 90.0);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(stored(&h).get("text-1").is_none(), "{:?}", h.state().notice);
    assert!(h.state().layer_editor.is_some());
    // Undo works inside the editor.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert!(stored(&h).get("text-1").is_some());
    drag(&mut h, (200.0, 100.0), (200.0, 100.0));
    assert_eq!(h.state().layer_editor.as_ref().unwrap().selected, ["text-1"]);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().layer_editor.is_some(), "the first Escape clears the selection");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().layer_editor.is_none());
}

#[test]
fn handles_rotate_and_resize_one_layer_and_a_lock_holds_it() {
    let mut h = opened();
    h.get_by_label("Add shape").click();
    h.run_steps(2);
    h.get_by_label("Rectangle").click();
    h.run_steps(3);
    let square = layer(&h, "shape-1");
    assert_eq!((square.width, square.height), (60.0, 60.0));
    // The rotate handle sits above the top edge; dragging it to the right turns a quarter.
    let offset = f64::from(h.state().theme.tokens().layer_rotate_offset) / h.state().layer_editor.as_ref().unwrap().scale;
    drag(&mut h, (200.0, 70.0 - offset), (260.0, 100.0));
    assert!(close(layer(&h, "shape-1").rotation, 90.0), "{:?}", layer(&h, "shape-1"));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    // The bottom-right corner, dragged out, grows the box about the top-left one.
    drag(&mut h, (230.0, 130.0), (250.0, 150.0));
    let grown = layer(&h, "shape-1");
    assert!(close(grown.width, 80.0) && close(grown.height, 80.0), "{grown:?}");
    assert!(close(grown.x - grown.width / 2.0, 170.0) && close(grown.y - grown.height / 2.0, 70.0), "{grown:?}");

    h.get_by_label("Locked").click();
    h.run_steps(2);
    assert!(layer(&h, "shape-1").locked);
    drag(&mut h, (210.0, 110.0), (300.0, 150.0));
    assert_eq!(layer(&h, "shape-1"), Layer { locked: true, ..grown }, "a locked layer stays");
}

#[test]
fn fields_change_the_selected_layer_and_the_frame() {
    let mut h = opened();
    add_text(&mut h);
    let field = h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Text");
    field.click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Text").type_text(" here");
    h.run_steps(2);
    assert!(matches!(&layer(&h, "text-1").content, Content::Text { text, .. } if text == "Text"), "applied on letting go");
    // Clicking the stage's empty margin lets go of the field and clears the selection.
    let corner = h.get_by_label("Layer stage").rect().min + Vec2::splat(2.0);
    for pressed in [true, false] {
        h.event(egui::Event::PointerMoved(corner));
        h.event(egui::Event::PointerButton { pos: corner, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
    }
    h.run_steps(3);
    assert!(matches!(&layer(&h, "text-1").content, Content::Text { text, .. } if text == "Text here"), "{:?}", layer(&h, "text-1"));
    assert!(h.state().layer_editor.as_ref().unwrap().selected.is_empty());

    // With nothing selected the frame's fields show.
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Width").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Width").type_text("800");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(editor_node(&h).params.get("width"), Some(&Value::Unsigned(nodecraft_types::Unsigned(800))), "{:?}", h.state().notice);
}

use nodecraft_actions::layers::{Content, Layer};

#[test]
#[ignore = "manual public-safe capture; LAYERS101_RENDER_DIR"]
fn layers101_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("LAYERS101_RENDER_DIR").unwrap());
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "layers101-dark.png"), (crate::theme::ThemeKind::Light, "layers101-light.png")] {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1400.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let (w, h_px) = (320u32, 200u32);
        let pixels: Vec<u8> = (0..h_px).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h_px) as u8, 170, 255])).collect();
        let png = nodecraft_media::encode_png(w, h_px, &pixels, &Default::default()).unwrap();
        let canvas = h.state().canvas;
        let at = nodecraft_graph::Position { x: -200.0, y: -110.0 };
        import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "gradient.png", bytes: &png }], &[IMAGE]).unwrap();
        let session = &mut h.state_mut().session;
        session.dispatch("node.create", &json!({"canvas": canvas, "id": EDITOR, "kind": nodecraft_nodes::layers::ID, "pos": {"x": 200, "y": -110}})).unwrap();
        for (name, value) in [("width", 1200), ("height", 800)] {
            session.dispatch("node.param_set", &json!({"canvas": canvas, "id": EDITOR, "name": name, "value": unsigned(value)})).unwrap();
        }
        let edge = json!({"id": EdgeId::from_bytes([53; 16]), "from": {"node": IMAGE, "port": "out"}, "to": {"node": EDITOR, "port": nodecraft_nodes::layers::INPUT}, "order": 1});
        session.dispatch("edge.connect", &json!({"canvas": canvas, "edge": edge})).unwrap();
        let text = json!({"id": "title", "name": "Title", "x": 600, "y": 160, "width": 900, "height": 140, "content": {"kind": "text", "text": "Summer Sale", "size": 110, "align": "center", "color": "#ffffff"}});
        let badge = json!({"id": "badge", "name": "Badge", "x": 1000, "y": 620, "width": 240, "height": 240, "rotation": -12, "content": {"kind": "shape", "shape": "ellipse", "fill": "#e2483d", "stroke": "#ffffff", "stroke_width": 8}});
        for layer in [text, badge] {
            session.dispatch("layers.add", &json!({"canvas": canvas, "id": EDITOR, "layer": layer})).unwrap();
        }
        h.run_steps(3);
        right_click(&mut h, EDITOR);
        h.get_by_label("Edit layers…").click();
        h.run_steps(3);
        h.state_mut().layer_editor.as_mut().unwrap().selected = vec!["badge".into()];
        // Previews decode off the frame; give them time.
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            h.step();
        }
        h.render().unwrap().save(dir.join(name)).unwrap();
    }
}
