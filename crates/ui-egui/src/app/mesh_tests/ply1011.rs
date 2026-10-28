use super::*;

#[test]
#[ignore = "actual native GPU; public-safe PLY format chooser evidence"]
fn ply1011_visual_format_choices() {
    let dir = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PLY1011_RENDER_DIR").expect("set evidence directory"));
    std::fs::create_dir_all(&dir).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let render = egui_kittest::wgpu::create_render_state(egui_kittest::wgpu::default_wgpu_setup(), Default::default());
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        app.install_mesh_renderer(&render);
        let mut h = Harness::builder()
            .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(render))
            .with_size(Vec2::new(1400.0, 900.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let source = NodeId::from_bytes([60; 16]);
        let mesh = nodecraft_media::decode_mesh(CUBE, &Default::default()).unwrap();
        let bytes = nodecraft_media::encode_ply(&mesh, 16 * 1024 * 1024).unwrap();
        let canvas = h.state().canvas;
        import_files(
            &mut h.state_mut().session,
            canvas,
            nodecraft_graph::Position { x: 200., y: -110. },
            300.,
            &[Incoming { name: "original-cube.ply", bytes: &bytes }],
            &[source],
        )
        .unwrap();
        let canvas = h.state().canvas;
        let convert = NodeId::from_bytes([69; 16]);
        let kinds = nodecraft_nodes::Registry::builtins().unwrap();
        let mut node = kinds.get(nodecraft_nodes::mesh::CONVERT).unwrap().instantiate_instance(convert, Default::default(), &Default::default()).unwrap();
        node.pos = nodecraft_graph::Position { x: 650.0, y: -110.0 };
        h.state_mut().session.dispatch("node.add", &json!({"canvas":canvas,"node":node})).unwrap();
        h.state_mut().session.dispatch("edge.connect", &json!({"canvas":canvas,"edge":{"id":nodecraft_types::EdgeId::from_bytes([69;16]),"from":{"node":source,"port":"out"},"to":{"node":convert,"port":"model"},"order":0}})).unwrap();
        let mut ready = false;
        for _ in 0..200 {
            h.run();
            if h.query_all_by_label_contains("3D model, 12 triangles, shaded view").next().is_some() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready, "the connected original is decoded for both cards");
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.minimap_toggle", &json!({})).unwrap();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Format").click();
        h.run_steps(3);
        assert!(h.query_by_label("OBJ").is_some());
        assert!(h.query_by_label("STL (geometry only)").is_some());
        assert!(h.query_by_label("PLY (ASCII RGBA)").is_some());
        {
            h.get_by_label("PLY (ASCII RGBA)").click();
            h.run_steps(10);
            assert_eq!(h.state().session.document().canvases()[&canvas].nodes()[&convert].params["format"], Value::Text("ply".into()));
            h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Format").click();
            h.run_steps(3);
        }
        h.render().unwrap().save(dir.join(format!("{theme:?}-format-after.png"))).unwrap();
    }
}

#[test]
fn ply1011_real_format_control_selects_ply_and_undoes_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = harness();
        h.state_mut().theme = theme;
        crate::theme::apply(&h.ctx, theme);
        let source = add_cube(&mut h);
        let canvas = h.state().canvas;
        let convert = NodeId::from_bytes([69; 16]);
        let kinds = nodecraft_nodes::Registry::builtins().unwrap();
        let mut node = kinds.get(nodecraft_nodes::mesh::CONVERT).unwrap().instantiate_instance(convert, Default::default(), &Default::default()).unwrap();
        node.pos = nodecraft_graph::Position { x: 650.0, y: -110.0 };
        h.state_mut().session.dispatch("node.add", &json!({"canvas":canvas,"node":node})).unwrap();
        h.state_mut().session.dispatch("edge.connect", &json!({"canvas":canvas,"edge":{"id":nodecraft_types::EdgeId::from_bytes([69;16]),"from":{"node":source,"port":"out"},"to":{"node":convert,"port":"model"},"order":0}})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        h.run_steps(3);
        let original = h.state().session.document().canvases()[&canvas].nodes()[&source].params["asset"].clone();
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Format").click();
        h.run_steps(2);
        h.get_by_label("PLY (ASCII RGBA)").click();
        h.run_steps(3);
        assert_eq!(h.state().session.document().canvases()[&canvas].nodes()[&convert].params["format"], Value::Text("ply".into()));
        assert_eq!(h.state().session.document().canvases()[&canvas].nodes()[&source].params["asset"], original);
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().session.document().canvases()[&canvas].nodes()[&convert].params["format"], Value::Text("glb".into()));
        h.state_mut().session.dispatch("edit.redo", &json!({})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().session.document().canvases()[&canvas].nodes()[&convert].params["format"], Value::Text("ply".into()));
    }
}
