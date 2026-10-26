//! Input-to-empty creation (#950), through real pointer and keyboard events.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const TARGET: NodeId = NodeId::from_bytes([150; 16]);
const DROP: Pos2 = Pos2::new(320.0, 380.0);

fn harness(render: bool) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let ids: Vec<_> = app.session.document().canvases().get(&canvas).unwrap().nodes().keys().copied().collect();
    for id in ids {
        app.session.dispatch("node.remove", &json!({"canvas":canvas,"id":id})).unwrap();
    }
    app.session.dispatch("node.create", &json!({"canvas":canvas,"id":TARGET,"kind":"text.replace","pos":{"x":160,"y":-120}})).unwrap();
    app.centred = true;
    app.view = CanvasView { pan: Vec2::new(540.0, 300.0), zoom: 0.8 };
    let builder = Harness::builder().with_size(Vec2::new(1400.0, 900.0)).with_max_steps(160);
    let builder = if render { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    theme::apply(&h.ctx, ThemeKind::Dark);
    h.run();
    h
}

fn input(h: &Harness<'static, NodeCraftApp>) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&TARGET).unwrap();
    let rect = app.painted.get(&TARGET).copied().unwrap();
    let port = node.inputs.first().unwrap();
    let at = noodle::port_anchor(node, rect, Side::Input, &port.name).unwrap();
    app.view.to_screen(app.viewport.min, at)
}

fn open(h: &mut Harness<'static, NodeCraftApp>) {
    let from = input(h);
    super::tests::wire_drag(h, from, DROP);
    assert!(h.state().turn_into.is_some(), "dropping an unconnected input on empty canvas must offer compatible source nodes");
}

#[test]
fn input_to_empty_offers_compatible_outputs_and_creates_one_connected_node_with_one_undo() {
    let mut h = harness(false);
    let before = h.state().session.document().clone();
    open(&mut h);
    let menu = h.state().turn_into.as_ref().unwrap();
    assert!(menu.entries.iter().any(|entry| entry.kind == "source.text"));
    assert!(!menu.entries.iter().any(|entry| entry.kind == "source.image"));
    assert!(menu.entries.iter().any(|entry| entry.kind == nodecraft_nodes::batch::ID), "Batch exposes a scalar item output");
    assert_eq!(h.state().session.document(), &before, "opening a chooser never edits the graph");
    h.get_by_label("Connect from Text").click();
    h.run();
    let app = h.state();
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    assert_eq!(canvas.nodes().len(), 2);
    let source = canvas.nodes().values().find(|node| node.kind == "source.text").unwrap();
    let edge = canvas.edges().values().next().unwrap();
    assert_eq!((edge.from.node, edge.to.node), (source.id, TARGET));
    let dropped = app.view.to_world(app.viewport.min, DROP);
    let rect = app.painted.get(&source.id).unwrap();
    assert!((rect.right() - dropped.x).abs() < 1.0, "the new output side must meet the drop point");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "one undo removes both the node and wire");
}

#[test]
fn escape_from_the_input_chooser_leaves_document_and_history_unchanged() {
    let mut h = harness(false);
    let before = h.state().session.document().clone();
    let revision = h.state().session.revision();
    open(&mut h);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().turn_into.is_none());
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().session.revision(), revision);
}

#[test]
fn a_multi_output_nodes_chosen_port_meets_the_drop_point() {
    let mut h = harness(false);
    open(&mut h);
    h.get_by_label("Connect from Folder").click();
    h.run();
    let app = h.state();
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    let edge = canvas.edges().values().find(|edge| edge.to.node == TARGET).unwrap();
    let source = canvas.nodes().get(&edge.from.node).unwrap();
    assert!(source.outputs.len() > 1, "this fixture must not have a centered single output");
    let at = noodle::port_anchor(source, app.painted[&source.id], Side::Output, &edge.from.port).unwrap();
    let screen = app.view.to_screen(app.viewport.min, at);
    assert!(screen.distance(DROP) < 1.0, "chosen {} port is at {screen:?}, not {DROP:?}", edge.from.port);
}

fn add_source(h: &mut Harness<'static, NodeCraftApp>, kind: &str) -> Pos2 {
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([151; 16]);
    h.state_mut().session.dispatch("node.create", &json!({"canvas":canvas,"id":id,"kind":kind,"pos":{"x":-350,"y":-120}})).unwrap();
    h.run();
    let app = h.state();
    let node = app.session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
    let output = node.outputs.first().unwrap();
    app.view.to_screen(app.viewport.min, noodle::port_anchor(node, app.painted[&id], Side::Output, &output.name).unwrap())
}

#[test]
fn an_input_can_connect_directly_to_an_existing_compatible_output() {
    let mut h = harness(false);
    let output = add_source(&mut h, "source.text");
    let before = h.state().session.document().clone();
    let from = input(&h);
    super::tests::wire_drag(&mut h, from, output);
    assert!(h.state().turn_into.is_none());
    let app = h.state();
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    assert_eq!(canvas.nodes().len(), 2);
    assert_eq!(canvas.edges().len(), 1);
    assert_eq!(canvas.edges().values().next().unwrap().to.node, TARGET);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn incompatible_outputs_cards_and_overlays_do_not_create_a_node_or_wire() {
    let mut h = harness(false);
    let image = add_source(&mut h, "source.image");
    let before = h.state().session.document().clone();
    let card = h.state().view.rect_to_screen(h.state().viewport.min, h.state().painted[&TARGET]).center();
    let map = h.state().viewport.max - Vec2::new(120.0, 120.0);
    for to in [image, card, map] {
        let from = input(&h);
        super::tests::wire_drag(&mut h, from, to);
        assert!(h.state().turn_into.is_none());
        assert_eq!(h.state().session.document(), &before);
    }
}

#[test]
fn escape_during_an_input_drag_cancels_without_opening_a_chooser() {
    let mut h = harness(false);
    let before = h.state().session.document().clone();
    let from = input(&h);
    h.hover_at(from);
    h.drag_at(from);
    h.hover_at((from + DROP.to_vec2()) * 0.5);
    h.run();
    assert!(h.state().wiring.is_some());
    h.key_press(egui::Key::Escape);
    h.run();
    h.drop_at(DROP);
    h.run();
    assert!(h.state().wiring.is_none() && h.state().turn_into.is_none());
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn a_source_shortcut_with_native_key_and_text_events_creates_only_one_node() {
    let mut h = harness(false);
    let before = h.state().session.document().clone();
    open(&mut h);
    let revision = h.state().session.revision();
    h.input_mut().events.extend([
        egui::Event::Key { key: egui::Key::T, physical_key: Some(egui::Key::T), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Text("t".into()),
    ]);
    h.step();
    h.event(egui::Event::Key { key: egui::Key::T, physical_key: Some(egui::Key::T), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.state().turn_into.is_none());
    let app = h.state();
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    assert_eq!(canvas.nodes().len(), 2);
    assert_eq!(canvas.edges().len(), 1);
    let source = canvas.nodes().values().find(|node| node.kind == "source.text").unwrap();
    let events = app.session.events_since(revision).unwrap();
    assert_eq!(source.params.get("text"), Some(&Value::Text(String::new())), "the shortcut is not typed into the created editor; events: {events:?}");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "one undo removes the created source and wire; events: {events:?}");
}

#[test]
#[ignore = "manual public-safe capture; INPUT950_RENDER_DIR, optional INPUT950_BASELINE"]
fn render_input950_evidence() {
    let root = std::path::PathBuf::from(std::env::var_os("INPUT950_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    let baseline = std::env::var_os("INPUT950_BASELINE").is_some();
    let mut h = harness(true);
    let from = input(&h);
    super::tests::wire_drag(&mut h, from, DROP);
    assert_eq!(h.state().turn_into.is_some(), !baseline);
    h.render().unwrap().save(root.join(if baseline { "before.png" } else { "chooser.png" })).unwrap();
    if !baseline {
        h.get_by_label("Connect from Text").click();
        h.run();
        h.render().unwrap().save(root.join("connected.png")).unwrap();
    }
}
