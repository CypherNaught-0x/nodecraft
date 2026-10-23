//! Result export from the chip menu (#557), with background completion and cancellation (#587).
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const NODE: NodeId = NodeId::from_bytes([61; 16]);
const IMAGE: nodecraft_types::GenerationId = nodecraft_types::GenerationId::from_bytes([62; 16]);
pub(super) static EXPORT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An image node whose only result is a small PNG, with the bytes.
fn with_image() -> (Harness<'static, NodeCraftApp>, Vec<u8>) {
    let canvas = CanvasId::from_bytes([60; 16]);
    let png = nodecraft_media::encode_png(4, 4, &[120; 64], &Default::default()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([63; 16]), "Export").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let session = Session::new(project.clone()).unwrap();
    let mut node = session.kinds().get("generation.image").unwrap().descriptor().instantiate(NODE, &BTreeMap::new()).unwrap();
    // Content-addressed, so a scratch session names the asset the real one will store.
    let hash = Session::new(Project::new(ProjectId::from_bytes([64; 16]), "Scratch").unwrap()).unwrap().import_asset(&png).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    node.history.push(nodecraft_graph::Generation::new(IMAGE, vec![Value::Media(asset)]));
    node.active = Some(IMAGE);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.import_asset(&png).unwrap();
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let builder = Harness::builder().with_size(Vec2::new(1000.0, 700.0));
    let builder = if std::env::var_os("NODECRAFT_EXPORT587_EVIDENCE").is_some() { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    (h, png)
}

fn begin_export(h: &mut Harness<'static, NodeCraftApp>, answer: Option<std::path::PathBuf>) {
    h.state_mut().exports.answer = answer;
    h.get_by_label("Result 1 of 1, active").click_secondary();
    h.run();
    h.get_by_label("Export…").click();
    h.step();
    h.step();
}
pub(super) fn finish(h: &mut Harness<'static, NodeCraftApp>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().export_pending() {
        h.step();
        assert!(std::time::Instant::now() < deadline, "export completes: {:?}", h.state().notice);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    h.run();
}
fn export(h: &mut Harness<'static, NodeCraftApp>, answer: Option<std::path::PathBuf>) {
    begin_export(h, answer);
    finish(h);
}

#[test]
fn the_chip_menu_exports_a_result_where_the_dialog_says() {
    let _serial = EXPORT_TESTS.lock().unwrap();
    let (mut h, png) = with_image();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.png");
    let revision = h.state().session.revision();

    export(&mut h, None);
    assert!(!path.exists(), "a cancelled dialog writes nothing");
    assert_eq!(h.state().notice, None);

    export(&mut h, Some(path.clone()));
    assert_eq!(std::fs::read(&path).unwrap(), png, "{:?}", h.state().notice);
    assert_eq!(h.state().notice.as_deref(), Some("Exported take.png."));
    // The dialog asked before replacing, so a file there is replaced.
    std::fs::write(&path, b"older").unwrap();
    export(&mut h, Some(path.clone()));
    assert_eq!(std::fs::read(&path).unwrap(), png);
    assert_eq!(h.state().session.revision(), revision, "exporting doesn't edit the project");

    // A folder that went away says so instead of writing elsewhere.
    export(&mut h, Some(dir.path().join("gone").join("take.png")));
    assert_eq!(h.state().notice.as_deref(), Some("the export folder doesn't exist"));
}

#[test]
fn only_media_results_offer_export() {
    assert_eq!(crate::history::suggested_name("image/png"), "result.png");
    assert_eq!(crate::history::suggested_name("model/gltf-binary"), "result.glb");
    assert_eq!(crate::history::suggested_name("application/x-unknown"), "result.bin");
    let node = nodecraft_graph::Node::new(NODE, "generation.text");
    let text = nodecraft_graph::Generation::new(IMAGE, vec![Value::Text("words".into())]);
    assert!(crate::history::export_targets(&node, &text).is_empty());
}

#[test]
fn saving_every_item_numbers_them_in_a_folder_past_files_already_there() {
    let _serial = EXPORT_TESTS.lock().unwrap();
    let (mut h, png) = with_image();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("result-2.png"), b"mine").unwrap();
    let canvas = h.state().canvas;
    let save_all = |h: &mut Harness<'static, NodeCraftApp>| {
        h.state_mut().exports.answer = Some(dir.path().to_path_buf());
        let targets = vec![(Some("out".to_owned()), None, "result.png".to_owned()); 2];
        h.state_mut().start_export_all(canvas, NODE, IMAGE, targets);
        h.step();
        finish(h);
    };
    save_all(&mut h);
    assert_eq!(h.state().notice.as_deref(), Some("Exported 2 files."));
    assert_eq!(std::fs::read(dir.path().join("result-1.png")).unwrap(), png);
    assert_eq!(std::fs::read(dir.path().join("result-2.png")).unwrap(), b"mine", "a folder pick never replaces a file");
    assert_eq!(std::fs::read(dir.path().join("result-3.png")).unwrap(), png);
    // Saving into the same folder again adds files after the ones there.
    save_all(&mut h);
    assert_eq!(h.state().notice.as_deref(), Some("Exported 2 files."));
    assert_eq!(std::fs::read(dir.path().join("result-5.png")).unwrap(), png);
    assert_eq!(super::export::numbered("clip", 3), "clip-3");
}

#[derive(Default)]
struct Held(std::sync::Mutex<Option<nodecraft_engine::save_runner::SaveJob>>);
impl nodecraft_engine::save_runner::SaveRunner for Held {
    fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}
#[test]
fn a_pending_export_keeps_canvas_keys_live_and_its_real_cancel_button_preserves_the_target() {
    let _serial = EXPORT_TESTS.lock().unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let (mut h, _) = with_image();
        h.state_mut().theme = theme;
        h.ctx.set_visuals(crate::theme::visuals(theme));
        let runner = std::sync::Arc::new(Held::default());
        h.state_mut().exports.runner = Some(runner.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("public-result.png");
        std::fs::write(&path, b"existing file").unwrap();
        begin_export(&mut h, Some(path.clone()));
        assert!(h.state().export_pending());
        assert_eq!(std::fs::read(&path).unwrap(), b"existing file");
        let revision = h.state().session.revision();
        h.hover_at(egui::pos2(700.0, 350.0));
        h.key_press(egui::Key::T);
        h.step();
        assert_eq!(h.state().session.revision(), revision + 1, "ordinary canvas editing does not settle the export");
        h.key_press(egui::Key::Escape);
        h.step();
        if let Some(root) = std::env::var_os("NODECRAFT_EXPORT587_EVIDENCE") {
            let root = std::path::PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            // Paint the same fresh frame whose visible progress and enabled Cancel we check.
            for _ in 0..3 {
                h.step();
            }
            let _ = h.get_by_label("Preparing export…");
            let name = if theme == crate::theme::ThemeKind::Dark { "dark" } else { "light" };
            h.render().unwrap().save(root.join(format!("after-export-progress-{name}.png"))).unwrap();
        }
        h.get_by_label("Cancel export").click();
        h.step();
        assert!(!h.state().session.export_status().unwrap().cancellable);
        assert!(h.state().export_pending(), "cancel waits for a terminal worker outcome");
        runner.0.lock().unwrap().take().unwrap().run();
        finish(&mut h);
        assert_eq!(std::fs::read(&path).unwrap(), b"existing file");
        assert_eq!(h.state().notice.as_deref(), Some("Export cancelled; no file was written."));
        assert_eq!(h.state().session.revision(), revision + 1, "Cancel never leaks a canvas edit");
    }
}
