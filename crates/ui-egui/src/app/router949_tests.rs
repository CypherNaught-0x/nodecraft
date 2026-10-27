//! Router choices preserve the original output shape, through real pointer drags (#949).
use super::*;
use egui_kittest::Harness;

const SOURCE: NodeId = NodeId::from_bytes([149; 16]);
const ROUTER: NodeId = NodeId::from_bytes([150; 16]);
const DROP: Pos2 = Pos2::new(1020.0, 450.0);

fn scene(render: bool, theme: ThemeKind) -> Harness<'static, NodeCraftApp> {
    scene_for(render, theme, "text.split")
}
fn scene_for(render: bool, theme: ThemeKind, source: &str) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let old: Vec<_> = app.session.document().canvases().get(&canvas).unwrap().nodes().keys().copied().collect();
    for id in old {
        app.session.dispatch("node.remove", &json!({"canvas":canvas,"id":id})).unwrap();
    }
    for (id, kind, x) in [(SOURCE, source, -470), (ROUTER, "structure.router", -30)] {
        app.session.dispatch("node.create", &json!({"canvas":canvas,"id":id,"kind":kind,"pos":{"x":x,"y":-80}})).unwrap();
    }
    app.session
        .dispatch(
            "edge.connect",
            &json!({"canvas":canvas,"edge":{"id":EdgeId::from_bytes([151;16]),"from":{"node":SOURCE,"port":"out"},"to":{"node":ROUTER,"port":"in"},"order":0}}),
        )
        .unwrap();
    app.centred = true;
    app.view = CanvasView { pan: Vec2::new(540.0, 340.0), zoom: 0.8 };
    app.theme = theme;
    let builder = Harness::builder().with_size(Vec2::new(1400.0, 1000.0)).with_max_steps(160);
    let builder = if render { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    theme::apply(&h.ctx, theme);
    h.run();
    h
}
fn open(h: &mut Harness<'static, NodeCraftApp>, id: NodeId) -> Vec<String> {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
    let start = app.view.to_screen(app.viewport.min, noodle::port_anchor(node, app.painted[&id], Side::Output, "out").unwrap());
    super::tests::wire_drag(h, start, DROP);
    let menu = h.state().turn_into.as_ref().expect("a port drag opens the actual chooser");
    menu.entries.iter().map(|entry| entry.kind.clone()).collect()
}
#[test]
fn pointer_router_choices_match_original_list_output() {
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = scene(false, theme);
        let direct = open(&mut h, SOURCE);
        h.key_press(egui::Key::Escape);
        h.run();
        let routed = open(&mut h, ROUTER);
        assert_eq!(routed, direct, "a Router must not add scalar-only actions to a list output");
        assert!(!routed.iter().any(|id| id == "text.replace"));
    }
}
#[test]
#[ignore = "manual public-safe GPU captures; NODECRAFT_ROUTER949_EVIDENCE"]
fn router949_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("NODECRAFT_ROUTER949_EVIDENCE").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let baseline = std::env::var_os("NODECRAFT_ROUTER949_BASELINE").is_some();
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = scene(true, theme);
        let direct = open(&mut h, SOURCE);
        h.key_press(egui::Key::Escape);
        h.run();
        let routed = open(&mut h, ROUTER);
        if baseline {
            assert_ne!(routed, direct);
        } else {
            assert_eq!(routed, direct);
        }
        h.run();
        let theme = if theme == ThemeKind::Dark { "dark" } else { "light" };
        let side = if baseline { "before" } else { "after" };
        h.render().unwrap().save(dir.join(format!("{side}-{theme}.png"))).unwrap();
    }
}

#[test]
fn pointer_router_choices_match_original_scalar_output() {
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = scene_for(false, theme, "source.text");
        let direct = open(&mut h, SOURCE);
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(open(&mut h, ROUTER), direct);
        assert!(direct.iter().any(|id| id == "text.replace"));
    }
}
#[test]
fn empty_router_port_drag_explains_missing_provenance_instead_of_widening() {
    let mut h = scene(false, ThemeKind::Dark);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas":canvas,"id":EdgeId::from_bytes([151;16])})).unwrap();
    h.run();
    let app = h.state();
    let node = app.session.document().canvases().get(&canvas).unwrap().nodes().get(&ROUTER).unwrap();
    let start = app.view.to_screen(app.viewport.min, noodle::port_anchor(node, app.painted[&ROUTER], Side::Output, "out").unwrap());
    super::tests::wire_drag(&mut h, start, DROP);
    assert!(h.state().turn_into.is_none());
    assert!(h.state().notice.as_deref().is_some_and(|why| why.contains("Router") && why.contains("no resolved source")));
}
