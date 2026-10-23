use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::ProjectId;

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const SOUND: NodeId = NodeId::from_bytes([62; 16]);
const IMAGE: NodeId = NodeId::from_bytes([63; 16]);
const OLD: NodeId = NodeId::from_bytes([64; 16]);
const OTHER: NodeId = NodeId::from_bytes([65; 16]);

fn harness(moving: bool) -> Harness<'static, NodeCraftApp> {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([60; 16]), "Legacy Timeline").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind, x, y) in
        [(SOUND, "source.audio", 0.0, 0.0), (IMAGE, "source.image", 0.0, 350.0), (OLD, "video.timeline", 450.0, 0.0), (OTHER, "video.timeline", 450.0, 450.0)]
    {
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
        node.pos = nodecraft_graph::Position { x, y };
        if id == OLD {
            node.inputs.truncate(1);
            node.title = "Older Timeline".into();
        }
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    if moving {
        Change::Connect {
            canvas: C,
            edge: nodecraft_graph::Edge {
                id: EdgeId::from_bytes([66; 16]),
                from: Endpoint { node: SOUND, port: "out".into() },
                to: Endpoint { node: OTHER, port: "audio".into() },
                order: 0,
            },
        }
        .apply(&mut project)
        .unwrap();
    }
    // A saved legacy project opens without rewriting its stored inputs.
    let project = serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
    let session = Session::new(project).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1200.0, 1100.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, C).unwrap());
    h.run();
    h
}
fn point(h: &Harness<'static, NodeCraftApp>, id: NodeId, side: Side, port: &str) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap();
    let rect = *app.painted.get(&id).unwrap();
    let ports = match side {
        Side::Input => app.input_ports.get(node),
        Side::Output => &node.outputs,
    };
    let at = ports.iter().position(|input| input.name == port).unwrap();
    app.view.to_screen(app.viewport.min, noodle::anchor(rect, side, at, ports.len()))
}
fn saved_inputs(h: &Harness<'static, NodeCraftApp>) -> Vec<String> {
    h.state().session.document().canvases().get(&C).unwrap().nodes().get(&OLD).unwrap().inputs.iter().map(|port| port.name.clone()).collect()
}

#[test]
fn dragging_to_each_new_input_updates_the_legacy_card_and_undo_restores_it() {
    for (source, port) in [(SOUND, "audio"), (IMAGE, "stills")] {
        let mut h = harness(false);
        let before = h.state().session.document().clone();
        assert_eq!(saved_inputs(&h), ["clips"]);
        assert_eq!(h.query_all_by_label("audio input").count(), 2);
        assert_eq!(h.query_all_by_label("stills input").count(), 2);
        let shown = h.state().input_ports.get(before.canvases().get(&C).unwrap().nodes().get(&OLD).unwrap());
        assert_eq!(shown.iter().map(|input| input.name.as_str()).collect::<Vec<_>>(), ["clips", "audio", "stills"]);
        let from = point(&h, source, Side::Output, "out");
        let to = point(&h, OLD, Side::Input, port);
        super::tests::wire_drag(&mut h, from, to);
        assert_eq!(saved_inputs(&h), ["clips", "audio", "stills"], "notice: {:?}", h.state().notice);
        let canvas = h.state().session.document().canvases().get(&C).unwrap();
        assert!(canvas.edges().values().any(|e| e.from.node == source && e.to.node == OLD && e.to.port == port));
        let after = h.state().session.document().clone();
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document(), &before);
        assert!(!h.state().session.can_undo());
        assert_eq!(saved_inputs(&h), ["clips"]);
        // Previewing the new ports never reapplies the undone migration.
        h.run();
        assert_eq!(h.state().session.document(), &before);
        h.state_mut().session.dispatch("edit.redo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document(), &after);
    }
}

#[test]
fn picking_up_a_wire_can_drop_on_a_legacy_cards_new_input() {
    let mut h = harness(true);
    let before = h.state().session.document().clone();
    let from = point(&h, OTHER, Side::Input, "audio");
    let to = point(&h, OLD, Side::Input, "audio");
    super::tests::wire_drag(&mut h, from, to);
    assert_eq!(saved_inputs(&h), ["clips", "audio", "stills"], "notice: {:?}", h.state().notice);
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    assert_eq!(canvas.edges().len(), 1);
    assert_eq!(canvas.edges().get(&EdgeId::from_bytes([66; 16])).unwrap().to.node, OLD);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert!(!h.state().session.can_undo());
}
