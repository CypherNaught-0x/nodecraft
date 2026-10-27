//! Viewer/capture proof for original typed results on the local ComfyUI generation cards.
//! These fixtures are not evidence that a live checkpoint has executed.
#![cfg(not(target_arch = "wasm32"))]
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::MESH_KIND;
use nodecraft_types::{AssetRef, CanvasId, EdgeId, GenerationId, MediaMeta, NodeId, ProjectId, Value};

const C: CanvasId = CanvasId::from_bytes([124; 16]);
const CARD: NodeId = NodeId::from_bytes([125; 16]);
const IMAGE: NodeId = NodeId::from_bytes([126; 16]);
const RESULT: GenerationId = GenerationId::from_bytes([127; 16]);
const OBJ: &[u8] = b"v -1 0 0\nv 1 0 0\nv 0 2 0\nf 1 2 3\n";
const GLTF: &[u8] = include_bytes!("../../../providers/src/comfyui/fixtures/triangle.gltf");

fn app(model: &str, mime: &str, bytes: &[u8], theme: crate::theme::ThemeKind) -> NodeCraftApp {
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([121; 16]), "Original mesh assets").unwrap()).unwrap();
    let mesh = Value::Media(AssetRef { hash: scratch.import_asset(bytes).unwrap(), mime: mime.parse().unwrap(), meta: MediaMeta::default() });
    let png = nodecraft_media::encode_png(4, 4, &[120; 64], &Default::default()).unwrap();
    let image = Value::Media(AssetRef {
        hash: scratch.import_asset(&png).unwrap(),
        mime: "image/png".parse().unwrap(),
        meta: MediaMeta { width: Some(4), height: Some(4), ..Default::default() },
    });
    let mut project = Project::new(ProjectId::from_bytes([122; 16]), "Original local Hunyuan result").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Local 3D result") }.apply(&mut project).unwrap();
    let mut source = scratch.kinds().get("source.image").unwrap().instantiate_instance(IMAGE, Default::default(), &[("asset".into(), image)].into()).unwrap();
    source.pos = Position { x: -280., y: 40. };
    Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
    let params = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Retained text from another model".into()))].into();
    let mut node = scratch.kinds().get(MESH_KIND).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    node.pos = Position { x: 150., y: 40. };
    node.history.push(nodecraft_graph::Generation::new(RESULT, vec![mesh]));
    node.active = Some(RESULT);
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    Change::Connect {
        canvas: C,
        edge: nodecraft_graph::Edge {
            id: EdgeId::from_bytes([128; 16]),
            from: nodecraft_graph::Endpoint { node: IMAGE, port: "out".into() },
            to: nodecraft_graph::Endpoint { node: CARD, port: "image".into() },
            order: 0,
        },
    }
    .apply(&mut project)
    .unwrap();
    let snapshot = nodecraft_format::Snapshot { project, assets: scratch.assets().clone(), epoch: 0 };
    let session = Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.theme = theme;
    app.view.pan = Vec2::new(360., 170.);
    app.centred = true;
    app
}
fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    let theme = app.theme;
    let mut h = Harness::builder().with_size(Vec2::new(1180., 850.)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}
fn wait(h: &mut Harness<'static, NodeCraftApp>, label: &str) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        h.run();
        if h.query_by_label(label).is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}
#[test]
fn comfy3d24_each_local_generation_card_shows_real_mesh_and_capture_pointer_adds_one_wired_photo() {
    let decoded = nodecraft_media::decode_mesh(OBJ, &Default::default()).unwrap();
    let glb = nodecraft_media::encode_glb(&decoded).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for model in ["comfyui.hunyuan3d-2", "comfyui.hunyuan3d-2.1"] {
            for (mime, bytes) in [("model/gltf-binary", glb.as_slice()), ("model/obj", OBJ), ("model/gltf+json", GLTF)] {
                let mut h = harness(app(model, mime, bytes, theme));
                assert!(wait(&mut h, "3D model, 1 triangles, shaded view"), "{model}/{mime}: {:?}", h.state().notice);
                assert!(h.state().meshes.bytes() > 0, "the asset was decoded by the real viewer cache");
                h.get_by_label("Shaded").click();
                assert!(wait(&mut h, "3D model, 1 triangles, unlit view"));
                let before = h.state().session.document().clone();
                let revision = h.state().session.revision();
                h.get_by_label("Photo").click();
                h.run_steps(3);
                let canvas = h.state().session.document().canvases().get(&C).unwrap();
                let photo = canvas.nodes().values().find(|node| node.kind == nodecraft_nodes::mesh::PHOTO).unwrap();
                assert!(canvas.edges().values().any(|edge| edge.from.node == CARD && edge.to.node == photo.id && edge.to.port == "model"));
                assert_eq!(photo.params.get("style"), Some(&Value::Text("unlit".into())));
                assert_eq!(h.state().session.revision(), revision + 1);
                h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
                h.run_steps(2);
                assert_eq!(h.state().session.document(), &before, "capture adds one ordinary undoable graph edit");
            }
        }
    }
}

#[test]
#[ignore = "native GPU original-result evidence; NODECRAFT_COMFY3D24_MESH_RENDER_ROOT"]
fn comfy3d24_native_original_mesh_result_rotates_on_screen() {
    let dir = std::path::PathBuf::from(std::env::var_os("NODECRAFT_COMFY3D24_MESH_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let render = egui_kittest::wgpu::create_render_state(egui_kittest::wgpu::default_wgpu_setup(), Default::default());
        let mut app = app("comfyui.hunyuan3d-2.1", "model/gltf+json", GLTF, theme);
        app.install_mesh_renderer(&render);
        let mut h = Harness::builder()
            .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(render))
            .with_size(Vec2::new(1180., 850.))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        assert!(wait(&mut h, "3D model, 1 triangles, shaded view"));
        assert!(h.state().mesh_gpu);
        let quiet = egui::pos2(1170., 835.);
        h.hover_at(quiet);
        h.run_steps(3);
        let area = h.get_by_label("3D model, 1 triangles, shaded view").rect();
        let before = h.render().unwrap();
        let from = area.center() + egui::vec2(0., -15.);
        h.hover_at(from);
        h.step();
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        for step in 1..=6 {
            h.event(egui::Event::PointerMoved(from + egui::vec2(step as f32 * 8., step as f32 * 3.)));
            h.step();
        }
        let end = from + egui::vec2(48., 18.);
        h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.hover_at(quiet);
        h.run_steps(3);
        assert_ne!(h.state().mesh_views.get(&CARD).unwrap().orbit, nodecraft_gpu::mesh::Orbit::default());
        assert_eq!(h.state().session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap().pos, Position { x: 150., y: 40. });
        let after = h.render().unwrap();
        let changed = (area.min.y.max(0.) as u32 + 4..area.max.y.min(850.) as u32 - 4)
            .flat_map(|y| (area.min.x.max(0.) as u32 + 4..area.max.x.min(1180.) as u32 - 4).map(move |x| (x, y)))
            .filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y))
            .count();
        assert!(changed > 25, "the actual triangle rendering changes under the pointer orbit ({changed} changed pixels)");
        after.save(dir.join(format!("{theme:?}-mesh-after.png"))).unwrap();
    }
}
