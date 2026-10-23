//! Placement uses admitted compact-card geometry, independently of camera scale.
use super::*;
use egui_kittest::Harness;

#[test]
fn quick_create_centers_actual_cards_at_pointer_and_keeps_ids_and_single_undo() {
    for zoom in [0.5, 1.0, 2.0] {
        for (key, kind) in [(egui::Key::T, "source.text"), (egui::Key::I, "source.image"), (egui::Key::R, nodecraft_nodes::router::ID)] {
            let mut h = Harness::builder()
                .with_size(Vec2::new(1200.0, 900.0))
                .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
            h.state_mut().view.zoom = zoom;
            h.run();
            let pointer = Pos2::new(200.0, 700.0);
            h.hover_at(pointer);
            h.run();
            let app = h.state();
            let before = app.session.document().clone();
            let world = app.view.to_world(app.viewport.min, pointer);
            let t = app.theme.tokens();
            let legacy_pos = nodecraft_graph::Position { x: world.x - t.card_width * 0.5, y: world.y - t.card_min_height * 0.5 };
            let expected_id = ids::node_id(app.session.document(), (kind, app.session.revision(), legacy_pos.x.to_bits(), legacy_pos.y.to_bits())).unwrap();
            h.key_press(key);
            h.run();
            let app = h.state();
            let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&expected_id).unwrap();
            assert_eq!(node.kind, kind);
            let rect = payload_rect(&app.session, node, app.theme.tokens());
            assert!(rect.height() < app.theme.tokens().card_min_height, "{kind} must exercise compact geometry");
            assert!((rect.center() - world).length() < 0.01, "{kind} at {zoom}: {rect:?} != {world:?}");
            assert_eq!(app.selection.nodes(), &[expected_id].into_iter().collect());
            assert_eq!(app.session.revision(), 1);
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &before);
        }
    }
}

#[test]
fn connected_create_centers_actual_input_side_and_undoes_node_with_edge() {
    let mut app = NodeCraftApp::headless().unwrap();
    let before = app.session.document().clone();
    let source = app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().next().unwrap();
    let output = source.outputs.first().unwrap();
    let from = Endpoint { node: source.id, port: output.name.clone() };
    let world = Pos2::new(900.0, 550.0);
    let menu = TurnInto::new(app.session.kinds(), from, output.port_type, world, world);
    let t = app.theme.tokens();
    let legacy_y = world.y - t.card_min_height * 0.5;
    let seed = ("turn-into", nodecraft_nodes::router::ID, app.session.revision(), world.x.to_bits(), legacy_y.to_bits());
    let expected_id = ids::node_id(app.session.document(), seed).unwrap();
    let expected_edge = ids::edge_id(app.session.document(), seed).unwrap();
    app.create_connected(nodecraft_nodes::router::ID, &menu);
    let canvas = app.session.document().canvases().get(&app.canvas).unwrap();
    let node = canvas.nodes().get(&expected_id).unwrap();
    let rect = payload_rect(&app.session, node, app.theme.tokens());
    assert!(rect.height() < app.theme.tokens().card_min_height);
    assert!((rect.left_center() - world).length() < 0.01);
    assert_eq!(canvas.edges().get(&expected_edge).unwrap().to.node, expected_id);
    assert_eq!(app.session.revision(), 1);
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.document(), &before);
}
