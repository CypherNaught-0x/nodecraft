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
fn mesh928_a_3d_card_saves_one_drag_and_restores_it_after_undo_and_reopen() {
    let mut h = harness();
    let id = add_cube(&mut h);
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"), "the model is decoded and described");
    assert!(h.state().meshes.bytes() > 0);
    // No renderer in tests: the card writes what it holds instead of drawing it.
    assert!(!h.state().mesh_gpu);

    let revision = h.state().session.revision();
    let before = h.state().session.document().clone();
    let centre = h.get_by_label("3D model, 12 triangles, shaded view").rect().center();
    let start = egui::pos2(centre.x, centre.y - 20.0);
    h.hover_at(start);
    h.step();
    h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(start + egui::vec2(i as f32 * 6.0, i as f32 * 2.0)));
        h.step();
        assert_eq!(h.state().session.revision(), revision, "the drag is a local preview until release");
    }
    let end = start + egui::vec2(48.0, 16.0);
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
    let turned = h.state().mesh_views.get(&id).copied().unwrap_or_default();
    assert_ne!(turned.orbit, nodecraft_gpu::mesh::Orbit::default(), "dragging turns the model");
    assert_eq!(h.state().session.revision(), revision + 1, "one command commits the whole drag");
    let after = h.state().session.document().clone();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run_steps(2);
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().mesh_views.get(&id).unwrap().orbit, nodecraft_gpu::mesh::Orbit::default());
    h.state_mut().session.dispatch("edit.redo", &json!({})).unwrap();
    h.run_steps(2);
    assert_eq!(h.state().session.document(), &after);
    assert_eq!(h.state().mesh_views.get(&id).unwrap().orbit, turned.orbit);
    // The same serialized project drives a fresh viewer after reopening; no renderer cache is authority.
    let reopened: nodecraft_graph::Project = serde_json::from_slice(&serde_json::to_vec(&after).unwrap()).unwrap();
    let saved_node = reopened.canvases().get(&h.state().canvas).unwrap().nodes().get(&id).unwrap();
    let mut fresh = crate::mesh_view::MeshView::default();
    fresh.bind(reopened.id(), h.state().canvas, saved_node);
    assert_eq!(fresh.orbit, turned.orbit);
    let archive =
        nodecraft_format::encode(&nodecraft_format::Snapshot { project: after.clone(), assets: h.state().session.assets().clone(), epoch: 0 }).unwrap();
    let snapshot = nodecraft_format::decode(&archive).unwrap();
    let reopened_session = Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
    let reopened_app = NodeCraftApp::from_session(reopened_session, h.state().canvas).unwrap();
    let mut reopened = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), reopened_app);
    assert!(wait_for(&mut reopened, "3D model, 12 triangles, shaded view"));
    assert_eq!(reopened.state().mesh_views.get(&id).unwrap().orbit, turned.orbit, "a real .ncraft round trip restores the rendered camera");
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
fn mesh928_alt_scroll_is_one_edit_and_external_view_commands_replace_the_preview() {
    let mut h = harness();
    let id = add_cube(&mut h);
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
    let before = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let at = h.get_by_label("3D model, 12 triangles, shaded view").rect().center();
    h.hover_at(at);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    let pan = h.state().view.pan;
    for _ in 0..4 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: Vec2::new(0.0, 8.0),
            modifiers: egui::Modifiers::ALT,
        });
        h.step();
        assert_eq!(h.state().session.revision(), revision);
    }
    h.run_steps(120);
    assert!(h.state().mesh_views.get(&id).unwrap().orbit.zoom > 1.0);
    assert_eq!(h.state().view.pan, pan, "Alt-scroll is the model's: the canvas doesn't pan by it too (#413)");
    assert_eq!(h.state().session.revision(), revision + 1);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run_steps(2);
    assert_eq!(h.state().session.document(), &before);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("mesh.view_set", &json!({"canvas":canvas,"id":id,"view":{"yaw":180,"pitch":0,"zoom":4,"style":"wireframe"}})).unwrap();
    h.run_steps(2);
    let view = h.state().mesh_views.get(&id).unwrap();
    assert_eq!(view.orbit, nodecraft_gpu::mesh::Orbit { yaw: std::f32::consts::PI, pitch: 0.0, zoom: 4.0 });
    assert_eq!(view.style, nodecraft_gpu::mesh::Style::Wireframe);
}

#[test]
fn mesh928_a_file_snapshot_finishes_pending_scroll_and_stale_preview_loses_to_commands() {
    let mut h = harness();
    let id = add_cube(&mut h);
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
    let at = h.get_by_label("3D model, 12 triangles, shaded view").rect().center();
    h.hover_at(at);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, 12.0),
        modifiers: egui::Modifiers::ALT,
    });
    h.step();
    let ctx = h.ctx.clone();
    assert!(h.state_mut().commit_mesh_views(&ctx, true));
    let canvas = h.state().canvas;
    let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
    assert!(nodecraft_nodes::mesh::view::State::read(&node.params).unwrap().zoom > 1.0);
    h.step();
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, 12.0),
        modifiers: egui::Modifiers::ALT,
    });
    h.step();
    h.state_mut().session.dispatch("mesh.view_set", &json!({"canvas":canvas,"id":id,"view":{"yaw":90,"pitch":0,"zoom":2,"style":"unlit"}})).unwrap();
    let authoritative = h.state().session.document().clone();
    // Finish without painting the card: this is also how an off-screen pending view is checked.
    assert!(h.state_mut().commit_mesh_views(&ctx, true));
    assert_eq!(h.state().session.document(), &authoritative);
    assert_eq!(h.state().mesh_views.get(&id).unwrap().orbit.zoom, 2.0);
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
            .with_size(Vec2::new(1400.0, 900.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        let id = add_cube(&mut h);
        assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
        let canvas = h.state().canvas;
        let mut view = nodecraft_nodes::mesh::view::State::default();
        view.style = match style {
            nodecraft_gpu::mesh::Style::Shaded => nodecraft_nodes::mesh::view::Style::Shaded,
            nodecraft_gpu::mesh::Style::Unlit => nodecraft_nodes::mesh::view::Style::Unlit,
            nodecraft_gpu::mesh::Style::Wireframe => nodecraft_nodes::mesh::view::Style::Wireframe,
        };
        h.state_mut().session.dispatch("mesh.view_set", &json!({"canvas":canvas,"id":id,"view":view})).unwrap();
        h.run_steps(3);
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.minimap_toggle", &json!({})).unwrap();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
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

#[test]
fn the_capture_buttons_add_a_wired_photo_or_view_sheet_from_the_cards_view_as_one_undo_step() {
    let mut h = harness();
    let id = add_cube(&mut h);
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
    h.get_by_label("Shaded").click();
    assert!(wait_for(&mut h, "3D model, 12 triangles, unlit view"));
    let turned = nodecraft_gpu::mesh::Orbit { yaw: 1.0, pitch: 0.5, zoom: 2.0 };
    h.state_mut().mesh_views.entry(id).or_default().orbit = turned;
    let count = |h: &Harness<'static, NodeCraftApp>| h.state().session.document().canvases().get(&h.state().canvas).map(|c| (c.nodes().len(), c.edges().len()));
    let before = count(&h);

    h.get_by_label("Photo").click();
    h.run_steps(3);
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap().clone();
    let photo = canvas.nodes().values().find(|n| n.kind == nodecraft_nodes::mesh::PHOTO).expect("a 3D photo card");
    let number = |name: &str| match photo.params.get(name) {
        Some(nodecraft_types::Value::Number(n)) => n.get(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!((number("yaw"), number("pitch"), number("zoom")), (57.0, 29.0, 2.0), "the card's view, in degrees");
    assert_eq!(photo.params.get("style"), Some(&nodecraft_types::Value::Text("unlit".into())));
    assert!(canvas.edges().values().any(|e| e.from.node == id && e.to.node == photo.id && e.to.port == "model"));
    assert!(h.state().selection.contains(photo.id), "the new card is selected");
    assert!(h.state().mesh_views.get(&id).is_some_and(|v| v.capture.is_none()), "the request is taken once");

    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(count(&h), before, "one undo step removes the card and its connection");

    h.get_by_label("Views").click();
    h.run_steps(3);
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert!(canvas.nodes().values().any(|n| n.kind == nodecraft_nodes::mesh::VIEWS));
}
#[test]
fn the_turntable_button_seeds_a_video_card_from_the_view_and_undo_removes_both_edits() {
    let mut h = harness();
    let id = add_cube(&mut h);
    let before = {
        let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
        (canvas.nodes().len(), canvas.edges().len())
    };
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
    h.state_mut().mesh_views.entry(id).or_default().orbit = nodecraft_gpu::mesh::Orbit { yaw: 1.0, pitch: 0.5, zoom: 2.0 };
    h.get_by_label("Turntable").click();
    h.run_steps(3);
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    let video = canvas.nodes().values().find(|n| n.kind == nodecraft_nodes::mesh::TURNTABLE).unwrap();
    let saved = video.clone();
    assert_eq!(video.outputs.first().unwrap().port_type.modality, nodecraft_types::Modality::Video);
    assert_eq!(video.params.get("start"), Some(&Value::Number(nodecraft_types::Number::new(57.0).unwrap())));
    assert!(canvas.edges().values().any(|e| e.from.node == id && e.to.node == video.id));
    h.state_mut().mesh_views.entry(id).or_default().orbit = Default::default();
    assert_eq!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&saved.id), Some(&saved));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert_eq!((canvas.nodes().len(), canvas.edges().len()), before);
}

#[test]
#[ignore = "actual GPU; public-safe turntable UI evidence in NODECRAFT_TURNTABLE_RENDER_DIR"]
fn turntable575_visual() {
    let render = egui_kittest::wgpu::create_render_state(egui_kittest::wgpu::default_wgpu_setup(), Default::default());
    let mut services = nodecraft_engine::commands::Services::default();
    let runs = super::runs::Runs::with_inline_maker(&mut services, Box::new(|_, _| panic!("a local turntable never requests a provider"))).unwrap();
    let mut app = NodeCraftApp::with_services(services).unwrap();
    runs.install(&mut app.session);
    app.runs = Some(runs);
    app.install_mesh_renderer(&render);
    let ffmpeg = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    app.set_media_tools(Some((&ffmpeg, &ffprobe)));
    let mut h = Harness::builder()
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(render))
        .with_size(Vec2::new(1600.0, 1200.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let id = add_cube(&mut h);
    assert!(wait_for(&mut h, "3D model, 12 triangles, shaded view"));
    h.get_by_label("Turntable").click();
    h.run_steps(3);
    let video = h
        .state()
        .session
        .document()
        .canvases()
        .get(&h.state().canvas)
        .unwrap()
        .nodes()
        .values()
        .find(|n| n.kind == nodecraft_nodes::mesh::TURNTABLE)
        .unwrap()
        .id;
    let canvas = h.state().canvas;
    for (name, value) in [
        ("width", Value::Unsigned(nodecraft_types::Unsigned(64))),
        ("height", Value::Unsigned(nodecraft_types::Unsigned(48))),
        ("fps", Value::Unsigned(nodecraft_types::Unsigned(12))),
        ("duration", Value::Number(nodecraft_types::Number::new(1.0).unwrap())),
    ] {
        h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":video,"name":name,"value":value})).unwrap();
    }
    h.state_mut().request_generate(video).unwrap();
    h.run();
    h.get_by_label("Generate now").click();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.step();
        let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&video).unwrap();
        ready = node.history.iter().find(|g| Some(g.id) == node.active).is_some_and(|g| {
            g.outputs.iter().any(|value| {
                matches!(value, Value::Media(asset) if asset.mime.essence() == "video/webm"
                && asset.meta.width == Some(64) && asset.meta.height == Some(48) && asset.meta.duration_ms == Some(1000))
            })
        }) && h.query_by_label("Video, 0:01").is_some();
        if ready {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(ready, "the pointer-created card renders and shows its ordinary video: {:?}", h.state().notice);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.minimap_toggle", &json!({})).unwrap();
    // Keep the saved camera and full card below the toolbar without shrinking the evidence text.
    h.state_mut().view.zoom = 0.9;
    h.state_mut().view.pan = Vec2::new(430.0, 260.0);
    h.run_steps(3);
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert!(canvas.nodes().values().any(|n| n.kind == nodecraft_nodes::mesh::TURNTABLE));
    assert!(canvas.edges().values().any(|e| e.from.node == id));
    if let Some(dir) = std::env::var_os("NODECRAFT_TURNTABLE_RENDER_DIR") {
        h.render().unwrap().save(std::path::PathBuf::from(dir).join("turntable-after.png")).unwrap();
    }
}
