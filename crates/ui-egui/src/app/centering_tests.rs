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

struct PortKind(nodecraft_nodes::NodeDescriptor);
impl nodecraft_nodes::NodeKind for PortKind {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<nodecraft_nodes::ModeId> {
        None
    }
    fn plan(&self, _: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, Error> {
        Ok(vec![])
    }
}
fn tall_harness(count: usize) -> (Harness<'static, NodeCraftApp>, NodeId) {
    let id = NodeId::from_bytes([91; 16]);
    let canvas = nodecraft_types::CanvasId::from_bytes([92; 16]);
    let outputs = (0..count)
        .map(|i| nodecraft_graph::Port { name: format!("out{i}"), port_type: PortType { modality: nodecraft_types::Modality::Text, many: false } })
        .collect();
    let descriptor = nodecraft_nodes::NodeDescriptor {
        id: "test.tall".into(),
        label: "Tall".into(),
        category: nodecraft_nodes::Category::Source,
        inputs: vec![],
        outputs,
        quick_add: None,
        params: Default::default(),
    };
    let mut kinds = nodecraft_nodes::Registry::default();
    kinds.register(Box::new(PortKind(descriptor))).unwrap();
    let mut node = nodecraft_engine::create_cmds::instance(&kinds, "test.tall", id).unwrap();
    let t = &theme::DARK;
    let size = card::world_size(&node, Some(kinds.get("test.tall").unwrap().descriptor()), t);
    node.pos = nodecraft_graph::Position { x: 300.0, y: 170.0 - size.y };
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([93; 16]), "Tall").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    nodecraft_graph::Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1200.0, 900.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
    h.state_mut().view = CanvasView::default();
    h.run();
    (h, id)
}
fn pointer(h: &mut Harness<'static, NodeCraftApp>, at: Pos2, pressed: bool) {
    h.hover_at(at);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    h.run();
}
#[test]
fn tall_lower_ports_survive_broadphase_and_start_their_own_wire() {
    for count in [25, 32, 256] {
        let (mut h, id) = tall_harness(count);
        let app = h.state();
        let body = *app.painted.get(&id).unwrap_or_else(|| panic!("{count} ports: visible lower body was culled"));
        assert!(body.top() < -900.0);
        let screen = app.view.rect_to_screen(app.viewport.min, body);
        let at = noodle::anchor(screen, Side::Output, count - 1, count);
        assert!(app.viewport.contains(at), "{at:?} not in {:?}", app.viewport);
        pointer(&mut h, at, true);
        h.hover_at(at + Vec2::new(5.0, 0.0));
        h.run();
        assert!(matches!(&h.state().wiring, Some(wiring) if wiring.from.node == id && wiring.from.port == format!("out{}", count - 1)));
        pointer(&mut h, at + Vec2::new(5.0, 0.0), false);
    }
}
#[test]
fn tall_lower_body_marquee_uses_same_bounds_even_when_not_painted() {
    for count in [25, 32, 256] {
        let (mut h, id) = tall_harness(count);
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        let world = Rect::from_min_max(Pos2::new(250.0, 80.0), Pos2::new(400.0, 150.0));
        let rect = box_geometry(&BTreeMap::new(), &app.session, &mut admission::GeometryCache::default(), node, world, app.theme.tokens());
        assert!(rect.intersects(world), "unpainted {count}-port lower body must be resolved");
        let from = app.view.to_screen(app.viewport.min, world.min);
        let to = app.view.to_screen(app.viewport.min, world.max);
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
        h.run();
        h.hover_at(from + (to - from) * 0.5);
        h.run();
        h.hover_at(to);
        h.run();
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::SHIFT });
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        h.run();
        assert!(h.state().selection.contains(id), "{count} ports: actual marquee missed lower body");
        assert_eq!(h.state().session.revision(), 0, "selection never edits the graph");
    }
}

#[test]
fn imported_payloads_center_actual_text_and_image_geometry_in_one_undo() {
    use nodecraft_engine::import::{Incoming, import_files_positioned};
    let mut app = NodeCraftApp::headless().unwrap();
    let before = app.session.document().clone();
    let png = nodecraft_media::encode_png(1, 1, &[255; 4], &Default::default()).unwrap();
    let files = [Incoming { name: "note.txt", bytes: b"words" }, Incoming { name: "pic.png", bytes: &png }];
    let ids = [NodeId::from_bytes([94; 16]), NodeId::from_bytes([95; 16])];
    let t = app.theme.tokens();
    import_files_positioned(&mut app.session, app.canvas, nodecraft_graph::Position { x: 700.0, y: 400.0 }, 350.0, &files, &ids, &[], &|node, kinds| {
        NodeCraftApp::creation_position(node, kinds, t)
    })
    .unwrap();
    for (index, id) in ids.into_iter().enumerate() {
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        let rect = payload_rect(&app.session, node, t);
        assert!((rect.center() - Pos2::new(700.0 + index as f32 * 350.0, 400.0)).length() < 0.01);
    }
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.document(), &before);
}

#[test]
fn connected_router_and_batch_use_engine_modality_instantiation_and_single_undo() {
    for source_kind in ["source.image", "source.video", "source.audio", "source.mesh", "source.document"] {
        for kind in [nodecraft_nodes::router::ID, nodecraft_nodes::batch::ID] {
            let mut app = NodeCraftApp::headless().unwrap();
            let source = NodeId::from_bytes([96; 16]);
            app.session.dispatch("node.create", &json!({"canvas":app.canvas,"id":source,"kind":source_kind,"pos":{"x":0,"y":0}})).unwrap();
            let before = app.session.document().clone();
            let output = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&source).unwrap().outputs[0].clone();
            let at = Pos2::new(800.0, 400.0);
            let menu = TurnInto::new(app.session.kinds(), Endpoint { node: source, port: output.name }, output.port_type, at, at);
            app.create_connected(kind, &menu);
            assert!(app.notice.is_none(), "{source_kind} -> {kind}: {:?}", app.notice);
            let id = *app.selection.nodes().first().unwrap();
            let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
            assert_eq!(node.inputs[0].port_type.modality, output.port_type.modality);
            assert!((payload_rect(&app.session, node, app.theme.tokens()).left_center() - at).length() < 0.01);
            app.session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(app.session.document(), &before);
        }
    }
}

#[test]
fn repeated_history_extracts_step_by_actual_result_rectangles() {
    let initial = NodeCraftApp::headless().unwrap();
    let source = NodeId::from_bytes([97; 16]);
    let generation = nodecraft_types::GenerationId::from_bytes([98; 16]);
    let mut node = nodecraft_engine::create_cmds::instance(initial.session.kinds(), "source.text", source).unwrap();
    node.pos = nodecraft_graph::Position { x: 100.0, y: 100.0 };
    node.history.push(nodecraft_graph::Generation::new(generation, vec![Value::Text("extracted".into())]));
    node.active = Some(generation);
    let mut project = initial.session.document().clone();
    nodecraft_graph::Change::AddNode { canvas: initial.canvas, node }.apply(&mut project).unwrap();
    let mut app = NodeCraftApp::from_session(Session::new(project).unwrap(), initial.canvas).unwrap();
    let before = app.session.document().clone();
    let t = app.theme.tokens();
    let mut rects = vec![];
    for _ in 0..2 {
        let (command, params) = app.history_command(source, HistoryIntent::Extract { generation, port: None, item: None }, t).unwrap();
        let result = app.session.dispatch(command, &params).unwrap();
        let id: NodeId = serde_json::from_value(result["id"].clone()).unwrap();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        rects.push(payload_rect(&app.session, node, t));
    }
    assert!((rects[1].top() - rects[0].bottom() - t.card_padding * 2.0).abs() < 0.01, "compact extracts must not retain the old 252-point step");
    for _ in 0..2 {
        app.session.dispatch("edit.undo", &json!({})).unwrap();
    }
    assert_eq!(app.session.document(), &before);
}
