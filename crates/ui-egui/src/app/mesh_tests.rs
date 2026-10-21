//! 3D models on cards (#111): decoded off the frame, turned by dragging, restyled from the card.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

const CUBE: &[u8] = b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 1 1 1\nv 0 1 1\n\
f 1 2 3 4\nf 5 8 7 6\nf 1 5 6 2\nf 2 6 7 3\nf 3 7 8 4\nf 5 1 4 8\n";

fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str) -> bool {
    for _ in 0..200 {
        h.run();
        if h.query_by_label(label).is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

fn add_cube(h: &mut Harness<'static, NodeCraftApp>) -> NodeId {
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([60; 16]);
    let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "cube.obj", bytes: CUBE }], &[id]).unwrap();
    id
}

#[test]
fn a_3d_card_shows_its_model_and_turns_it_without_touching_the_document() {
    let mut h = harness();
    let id = add_cube(&mut h);
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"), "the model is decoded and described");
    assert!(h.state().meshes.bytes() > 0);
    // No renderer in tests: the card writes what it holds instead of drawing it.
    assert!(!h.state().mesh_gpu);

    let revision = h.state().session.revision();
    let centre = h.get_by_label("3D model, 12 triangles, shaded view").rect().center();
    let start = egui::pos2(centre.x, centre.y - 20.0);
    h.hover_at(start);
    h.step();
    h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(start + egui::vec2(i as f32 * 6.0, i as f32 * 2.0)));
        h.step();
    }
    let end = start + egui::vec2(48.0, 16.0);
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
    let turned = h.state().mesh_views.get(&id).copied().unwrap_or_default();
    assert_ne!(turned.orbit, nodecraft_gpu::mesh::Orbit::default(), "dragging turns the model");
    assert_eq!(h.state().session.revision(), revision, "turning is view state, not an edit");
    let node = h.state().session.document().canvases().get(&h.state().canvas).and_then(|c| c.nodes().get(&id)).map(|n| n.pos);
    assert_eq!(node, Some(nodecraft_graph::Position { x: 200.0, y: -110.0 }), "the drag turned the model instead of moving the card");

    h.get_by_label("Shaded").click();
    assert!(wait_for(&mut h, "3D model, 12 triangles, unlit view"));
    h.get_by_label("Unlit").click();
    assert!(wait_for(&mut h, "3D model, 12 triangles, wireframe view"));
    h.get_by_label("Reset").click();
    h.run();
    assert_eq!(h.state().mesh_views.get(&id).map(|v| v.orbit), Some(nodecraft_gpu::mesh::Orbit::default()));
}

#[test]
fn a_broken_model_says_why() {
    let mut h = harness();
    let canvas = h.state().canvas;
    let bytes = b"# an OBJ with nothing in it\nv 1 2 3\n".to_vec();
    let hash = h.state_mut().session.import_asset(&bytes).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "model/obj".parse().unwrap(), meta: Default::default() };
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let supplied: nodecraft_types::ParamValues = [("asset".to_owned(), Value::Media(asset))].into_iter().collect();
    let mut node = kinds.get(nodecraft_nodes::mesh::SOURCE).unwrap().instantiate_instance(NodeId::from_bytes([61; 16]), Default::default(), &supplied).unwrap();
    node.pos = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap();
    assert!(wait_for(&mut h, "This 3D model is empty") || wait_for(&mut h, "Can't read this 3D model"));
}

#[test]
#[ignore = "needs a GPU; manual public-safe capture with MESH111_RENDER_DIR"]
fn mesh111_visual() {
    let dir = std::env::var_os("MESH111_RENDER_DIR").map(std::path::PathBuf::from);
    for (theme, style, name) in [
        (crate::theme::ThemeKind::Dark, nodecraft_gpu::mesh::Style::Shaded, "mesh111-dark-shaded.png"),
        (crate::theme::ThemeKind::Light, nodecraft_gpu::mesh::Style::Wireframe, "mesh111-light-wireframe.png"),
    ] {
        let render = egui_kittest::wgpu::create_render_state(egui_kittest::wgpu::default_wgpu_setup(), Default::default());
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        app.install_mesh_renderer(&render);
        let mut h = Harness::builder()
            .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(render))
            .with_size(Vec2::new(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let id = add_cube(&mut h);
        assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
        h.state_mut().mesh_views.entry(id).or_default().style = style;
        h.run_steps(3);
        let area = h.get_by_role(egui::accesskit::Role::Image).rect();
        let image = h.render().unwrap();
        let background = h.state().theme.tokens().canvas_bg;
        let differs = (area.min.y as u32 + 4..area.max.y as u32 - 4)
            .flat_map(|y| (area.min.x as u32 + 4..area.max.x as u32 - 4).map(move |x| (x, y)))
            .filter(|&(x, y)| image.get_pixel(x, y).0[..3] != [background.r(), background.g(), background.b()])
            .count();
        assert!(differs > 200, "{name}: the model is drawn over the preview's background ({differs} pixels)");
        if let Some(dir) = &dir {
            image.save(dir.join(name)).unwrap();
        }
    }
}
