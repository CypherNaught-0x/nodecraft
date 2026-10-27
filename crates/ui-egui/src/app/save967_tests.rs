//! Pointer-driven exact-result saves (#967). These tests never call a provider or a decoder tool.
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_types::{AssetRef, GenerationId};

const NODE: NodeId = NodeId::from_bytes([167; 16]);
const RESULT: GenerationId = GenerationId::from_bytes([168; 16]);

fn scene(kind: &str, mime: &str, bytes: &[u8], gpu: bool) -> Harness<'static, NodeCraftApp> {
    scene_with(kind, &[(mime, bytes.to_vec())], gpu, |_, _| {})
}

fn scene_with(kind: &str, media: &[(&str, Vec<u8>)], gpu: bool, setup: impl FnOnce(&mut nodecraft_graph::Node, &[Value])) -> Harness<'static, NodeCraftApp> {
    let canvas = CanvasId::from_bytes([169; 16]);
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([170; 16]), "Scratch").unwrap()).unwrap();
    let values: Vec<Value> = media
        .iter()
        .map(|(mime, bytes)| Value::Media(AssetRef { hash: scratch.import_asset(bytes).unwrap(), mime: mime.parse().unwrap(), meta: Default::default() }))
        .collect();
    let mut project = Project::new(ProjectId::from_bytes([171; 16]), "Save one result").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = scratch.kinds().get(kind).unwrap().descriptor().instantiate(NODE, &BTreeMap::new()).unwrap();
    node.history.push(nodecraft_graph::Generation::new(RESULT, vec![values.first().unwrap().clone()]));
    node.active = Some(RESULT);
    setup(&mut node, &values);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    for (_, bytes) in media {
        session.import_asset(bytes).unwrap();
    }
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let builder = Harness::builder().with_size(Vec2::new(1120.0, 760.0));
    let builder = if gpu { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn image() -> Vec<u8> {
    nodecraft_media::encode_png(8, 6, &[120; 192], &Default::default()).unwrap()
}

fn card_point(h: &Harness<'static, NodeCraftApp>) -> Pos2 {
    let rect = h.state().view.rect_to_screen(h.state().viewport.min, *h.state().painted.get(&NODE).unwrap());
    let top = rect.left_top() + Vec2::splat(3.0);
    if h.state().viewport.contains(top) {
        top
    } else {
        // A tall 3D card can extend above the viewport. Avoid its edge port hit targets.
        rect.intersect(h.state().viewport).left_top() + Vec2::splat(2.0 * h.state().theme.tokens().card_padding)
    }
}

fn secondary(h: &mut Harness<'static, NodeCraftApp>, position: Pos2) {
    h.hover_at(position);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: position, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    h.run();
}

fn wait(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while h.query_by_label(label).is_none() {
        h.step();
        assert!(std::time::Instant::now() < deadline, "waiting for {label}");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn finish_file(h: &mut Harness<'static, NodeCraftApp>) {
    // Deliver the menu click before testing whether a dialog/worker is pending.
    h.run_steps(2);
    export_tests::finish(h);
}

#[test]
fn save967_each_media_card_writes_only_the_chosen_bytes() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    // The same tiny container headers used by the mock provider exercise saving independently
    // of installed playback tools. A mesh is an original complete triangle OBJ.
    for (kind, mime, extension, bytes) in [
        ("generation.image", "image/png", "png", image()),
        ("generation.audio", "audio/wav", "wav", b"RIFF\0\0\0\0WAVEfmt ".to_vec()),
        ("generation.video", "video/mp4", "mp4", b"\0\0\0\x10ftypisom\0\0\x02\0".to_vec()),
        ("generation.mesh", "model/obj", "obj", b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n".to_vec()),
    ] {
        let mut h = scene(kind, mime, &bytes, false);
        if kind == "generation.mesh" {
            wait(&mut h, "3D model, 1 triangles, shaded view");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("chosen.{extension}"));
        h.state_mut().exports.answer = Some(path.clone());
        let before = h.state().session.document().clone();
        let point = card_point(&h);
        secondary(&mut h, point);
        assert!(h.query_by_label("Save to folder…").is_some(), "{kind} context at {point:?}, popup {}", egui::Popup::is_any_open(&h.ctx));
        h.get_by_label("Save as…").click();
        finish_file(&mut h);
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{kind}: {:?}", h.state().notice);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no sidecars or siblings: {kind}");
        assert_eq!(h.state().session.document(), &before);
    }
}

#[test]
fn save967_image_preview_and_cancel_share_the_file_writer() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let bytes = image();
    let mut h = scene("generation.image", "image/png", &bytes, false);
    wait(&mut h, "Image 8×6");
    let dir = tempfile::tempdir().unwrap();
    h.get_by_label("Image 8×6").click_secondary();
    h.run();
    h.get_by_label("Save as…").click();
    finish_file(&mut h);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "cancel writes nothing");
    h.state_mut().exports.answer = Some(dir.path().join("image.png"));
    h.get_by_label("Image 8×6").click_secondary();
    h.run();
    h.get_by_label("Save as…").click();
    finish_file(&mut h);
    assert_eq!(std::fs::read(dir.path().join("image.png")).unwrap(), bytes);
}

#[test]
fn save967_no_result_is_disabled_with_an_explanation() {
    let mut h = scene("generation.image", "image/png", &image(), false);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("history.clear", &json!({"canvas":canvas,"id":NODE})).unwrap();
    h.run();
    let point = card_point(&h);
    secondary(&mut h, point);
    assert!(h.get_by_label("Save as…").accesskit_node().is_disabled());
    assert!(h.get_by_label("Save to folder…").accesskit_node().is_disabled());
    assert!(h.query_by_label("No saved media result. Generate or select a media result first.").is_some());
    assert!(!h.state().export_pending());
}

#[test]
fn save967_preview_menus_cover_audio_video_and_interactive_mesh() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    for (kind, mime, bytes, label) in [
        ("generation.audio", "audio/wav", b"RIFF\0\0\0\0WAVEfmt ".to_vec(), crate::audio::UNCONFIGURED),
        ("generation.video", "video/mp4", b"\0\0\0\x10ftypisom\0\0\x02\0".to_vec(), crate::video::UNCONFIGURED),
        ("generation.mesh", "model/obj", b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n".to_vec(), "3D model, 1 triangles, shaded view"),
    ] {
        let mut h = scene(kind, mime, &bytes, false);
        wait(&mut h, label);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.bin");
        h.state_mut().exports.answer = Some(path.clone());
        h.get_by_label(label).click_secondary();
        h.run();
        h.get_by_label("Save as…").click();
        finish_file(&mut h);
        assert_eq!(std::fs::read(path).unwrap(), bytes, "{kind}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn save967_gallery_pins_the_exact_flattened_item_and_output() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let first = image();
    let second = nodecraft_media::encode_png(6, 8, &[220; 192], &Default::default()).unwrap();
    let mut h = scene_with("generation.image", &[("image/png", first.clone()), ("image/png", second.clone())], false, |node, images| {
        node.outputs = vec![
            nodecraft_graph::Port { name: "other".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: false } },
            nodecraft_graph::Port { name: "pictures".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: true } },
        ];
        let generation = node.history.first_mut().unwrap();
        generation.outputs = vec![
            Value::List(vec![Value::Text("do not save me".into()), Value::Text("nor me".into())]),
            Value::List(vec![Value::List(vec![images[0].clone(); 5]), Value::List(vec![images[1].clone(), images[0].clone()])]),
        ];
        generation.items = [1, 2]
            .map(|item| vec![nodecraft_graph::ItemKey { batch: NodeId::from_bytes([172; 16]), item: nodecraft_types::ItemId::from_bytes([item; 16]) }])
            .to_vec();
    });
    wait(&mut h, "Image 6×8");
    let before = h.state().session.document().clone();
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().join("second-row.png"));
    h.get_by_label("Image 6×8").click_secondary();
    h.run();
    h.get_by_label("Save as…").click();
    finish_file(&mut h);
    assert_eq!(std::fs::read(dir.path().join("second-row.png")).unwrap(), second);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(h.state().session.document(), &before, "saving never changes history selection");
}

#[test]
fn save967_open_menu_keeps_its_historical_result_when_the_active_result_changes() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let second = nodecraft_media::encode_png(6, 8, &[220; 192], &Default::default()).unwrap();
    let newer = GenerationId::from_bytes([173; 16]);
    let original = image();
    let mut h = scene_with("generation.image", &[("image/png", original.clone()), ("image/png", second)], false, |node, media| {
        node.history.push(nodecraft_graph::Generation::new(newer, vec![media[1].clone()]));
    });
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().join("original.png"));
    let point = card_point(&h);
    secondary(&mut h, point);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("history.select", &json!({"canvas":canvas,"id":NODE,"generation":newer})).unwrap();
    h.run();
    h.get_by_label("Save as…").click();
    finish_file(&mut h);
    assert_eq!(std::fs::read(dir.path().join("original.png")).unwrap(), original);
}

#[test]
fn save967_nested_collection_card_menu_saves_one_exact_media_leaf() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let second = nodecraft_media::encode_png(6, 8, &[220; 192], &Default::default()).unwrap();
    let mut h = scene_with("video.frames", &[("image/png", image()), ("image/png", second.clone())], false, |node, media| {
        let generation = node.history.first_mut().unwrap();
        generation.outputs = vec![Value::List(vec![Value::List(vec![media[0].clone(), Value::Text("skip this non-media leaf".into()), media[1].clone()])])];
        assert!(generation.items.is_empty(), "ordinary collections are not batch-fanned");
    });
    let port = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&NODE).unwrap().outputs.first().unwrap().name.clone();
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().join("nested.png"));
    let point = card_point(&h);
    secondary(&mut h, point);
    h.get_by_label_contains("Save as…").click();
    h.run();
    h.get_by_label(&format!("{port}: item 3 of 3")).click();
    finish_file(&mut h);
    assert_eq!(std::fs::read(dir.path().join("nested.png")).unwrap(), second);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn save967_gallery_after_a_nested_collection_uses_the_export_leaf_index() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let second = nodecraft_media::encode_png(6, 8, &[220; 192], &Default::default()).unwrap();
    let mut h = scene_with("generation.image", &[("image/png", image()), ("image/png", second.clone())], false, |node, media| {
        node.outputs.first_mut().unwrap().port_type.many = true;
        node.history.first_mut().unwrap().outputs = vec![Value::List(vec![Value::List(vec![media[0].clone(); 3]), media[1].clone()])];
    });
    wait(&mut h, "Image 6×8");
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().join("visible.png"));
    h.get_by_label("Image 6×8").click_secondary();
    h.run();
    h.get_by_label("Save as…").click();
    finish_file(&mut h);
    assert_eq!(std::fs::read(dir.path().join("visible.png")).unwrap(), second);
}

#[test]
fn save967_replacing_a_project_revokes_an_open_result_menu() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    for (kind, toolbar) in [("generation.image", false), ("image.blur", true)] {
        let mut h = scene(kind, "image/png", &image(), false);
        if toolbar {
            h.state_mut().selection.only(NODE);
            h.run();
            h.get_by_label("Save result").click();
            h.run();
        } else {
            let point = card_point(&h);
            secondary(&mut h, point);
        }
        assert!(!h.get_by_label("Save as…").accesskit_node().is_disabled());
        // Matching node/generation ids in the replacement are not authority for the old menu.
        let other_bytes = nodecraft_media::encode_png(6, 8, &[220; 192], &Default::default()).unwrap();
        let other = scene(kind, "image/png", &other_bytes, false);
        let mut replacement = Session::new(other.state().session.document().clone()).unwrap();
        replacement.import_asset(&other_bytes).unwrap();
        h.state_mut().session = replacement;
        h.run_steps(3);
        if let Some(action) = h.query_by_label("Save as…") {
            assert!(action.accesskit_node().is_disabled(), "an old {kind} menu must never export from the replacement project");
        }
        assert!(!h.state().export_pending());
    }
}

#[test]
fn save967_closed_card_menus_retain_at_most_one_snapshot_across_projects() {
    let mut h = scene("generation.image", "image/png", &image(), false);
    for byte in 1..=12 {
        let bytes = image();
        let other = scene_with("generation.image", &[("image/png", bytes.clone())], false, |node, _| node.id = NodeId::from_bytes([byte; 16]));
        let mut replacement = Session::new(other.state().session.document().clone()).unwrap();
        replacement.import_asset(&bytes).unwrap();
        h.state_mut().session = replacement;
        h.run_steps(3);
        let id = NodeId::from_bytes([byte; 16]);
        let rect = h.state().view.rect_to_screen(h.state().viewport.min, *h.state().painted.get(&id).unwrap());
        secondary(&mut h, rect.left_top() + Vec2::splat(3.0));
        h.get_by_label("Save as…");
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(crate::result_save::pinned_snapshot_count(&h.ctx) <= 1, "menu snapshots must have a fixed aggregate bound");
    }
}

#[test]
fn save967_non_generator_hover_toolbar_uses_the_same_exact_file_action() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let bytes = image();
    let mut h = scene("image.blur", "image/png", &bytes, false);
    h.state_mut().selection.only(NODE);
    h.run();
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().join("blur.png"));
    h.get_by_label("Save result").click();
    h.run();
    h.get_by_label("Save as…").click();
    finish_file(&mut h);
    assert_eq!(std::fs::read(dir.path().join("blur.png")).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
#[cfg(unix)]
fn save967_exact_result_folder_action_uses_the_shared_batch_preview_and_worker() {
    let _serial = export_tests::EXPORT_TESTS.lock().unwrap();
    let bytes = image();
    let mut h = scene("generation.image", "image/png", &bytes, false);
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().to_path_buf());
    let point = card_point(&h);
    secondary(&mut h, point);
    h.get_by_label("Save to folder…").click();
    h.run_steps(3);
    assert!(h.query_by_label("Only the chosen historical result items.").is_some());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "preview is not permission to start copying");
    h.get_by_label("Save files").click();
    wait(&mut h, "Workflow result save finished");
    h.get_by_label("Done").click();
    h.run_steps(2);
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(files.len(), 2, "exactly one media file and the shared provenance manifest");
    assert_eq!(std::fs::read(files.iter().find(|path| path.extension().is_some_and(|ext| ext == "png")).unwrap()).unwrap(), bytes);
}

#[test]
#[ignore = "manual native GPU before/after capture; NODECRAFT_SAVE967_EVIDENCE and NODECRAFT_SAVE967_SIDE"]
fn save967_render_result_menu() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_SAVE967_EVIDENCE").unwrap());
    let side = std::env::var("NODECRAFT_SAVE967_SIDE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = scene("generation.image", "image/png", &image(), true);
        h.state_mut().theme = theme;
        theme::apply(&h.ctx, theme);
        wait(&mut h, "Image 8×6");
        let point = card_point(&h);
        secondary(&mut h, point);
        h.render().unwrap().save(root.join(format!("{side}-{theme:?}.png"))).unwrap();
    }
}
