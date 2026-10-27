//! Original public scene; same helper can capture actual baseline and changed runtime source.
use super::*;
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use std::sync::{Arc, Mutex};
const VIDEO: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../engine/src/interchange_cmds/tests/fixtures/moving-marker.mp4"));

#[derive(Default)]
struct Held(Mutex<Option<SaveJob>>);
impl SaveRunner for Held {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}
fn scene(theme: crate::theme::ThemeKind, gpu: bool) -> Harness<'static, NodeCraftApp> {
    let builder = Harness::builder().with_size(Vec2::new(1100.0, 760.0));
    let builder = if gpu { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            app.export_close_request(ui.ctx());
            app.show(ui);
        },
        NodeCraftApp::headless().unwrap(),
    );
    h.state_mut().theme = theme;
    h.ctx.set_visuals(crate::theme::visuals(theme));
    h.run();
    with_timeline(&mut h, VIDEO, VIDEO);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":EDITOR,"title":"Practice cut"})).unwrap();
    open(&mut h);
    for id in [ONE, TWO] {
        dispatch(&mut h, "timeline.update", json!({"clip":auto(id),"set":{"out_us":1_000_000,"fade_in_us":200_000,"fade_out_us":200_000}}));
    }
    h.run_steps(3);
    h
}
fn start(h: &mut Harness<'static, NodeCraftApp>, path: std::path::PathBuf) -> Arc<Held> {
    let held = Arc::new(Held::default());
    h.state_mut().exports.runner = Some(held.clone());
    h.state_mut().exports.answer = Some(path);
    h.get_by_label("Export for another editor…").click();
    h.run_steps(4);
    held
}

#[test]
fn timeline_uses_a_real_background_receipt_and_cancel_button_in_both_themes() {
    let _serial = super::super::export_tests::EXPORT_TESTS.lock().unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(theme, false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("practice.zip");
        std::fs::write(&path, b"keep original").unwrap();
        let revision = h.state().session.revision();
        let held = start(&mut h, path.clone());
        assert!(h.state().export_pending());
        assert!(h.state().session.export_status().unwrap().result.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep original");
        h.get_by_label("Cancel export").click();
        h.run_steps(2);
        assert!(!h.state().session.export_status().unwrap().cancellable);
        held.0.lock().unwrap().take().unwrap().run();
        super::super::export_tests::finish(&mut h);
        assert_eq!(h.state().notice.as_deref(), Some("Export cancelled; no file was written."));
        assert_eq!(std::fs::read(&path).unwrap(), b"keep original");
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn replacing_the_session_keeps_the_old_timeline_receipt_until_quit_cleanup() {
    let _serial = super::super::export_tests::EXPORT_TESTS.lock().unwrap();
    let mut h = scene(crate::theme::ThemeKind::Dark, false);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("practice.zip");
    let held = start(&mut h, path.clone());
    let watch = h.state().session.export_watch().unwrap();
    let project = h.state().session.document().clone();
    h.state_mut().session = Session::new(project).unwrap();
    h.run_steps(2);
    assert!(h.state().export_pending(), "old worker lifetime is still tracked");
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.clear();
    h.run_steps(2);
    h.get_by_label("Cancel export and quit").click();
    h.run_steps(2);
    assert!(h.query_by_label("Cancelling export before quitting…").is_some());
    assert!(watch.status().result.is_none());
    held.0.lock().unwrap().take().unwrap().run();
    h.step();
    let commands = &h.output().viewport_output.get(&egui::ViewportId::ROOT).unwrap().commands;
    assert!(commands.contains(&egui::ViewportCommand::Close));
    assert!(!path.exists());
}

#[test]
#[ignore = "manual native GPU capture; needs NODECRAFT_INTERCHANGE643_RENDER_ROOT and SIDE"]
fn render_interchange643_before_after() {
    let _serial = super::super::export_tests::EXPORT_TESTS.lock().unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_INTERCHANGE643_RENDER_ROOT").unwrap());
    let side = std::env::var("NODECRAFT_INTERCHANGE643_SIDE").unwrap();
    assert!(matches!(side.as_str(), "before" | "after"));
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(theme, true);
        let dir = tempfile::tempdir().unwrap();
        let held = start(&mut h, dir.path().join("practice.zip"));
        h.run_steps(3);
        let theme = if theme == crate::theme::ThemeKind::Dark { "dark" } else { "light" };
        h.render().unwrap().save(root.join(format!("{side}-{theme}.png"))).unwrap();
        // Baseline synchronously completes; changed runtime leaves a queued job. The scene
        // does not hide UI or add a feature flag to the product to manufacture a baseline.
        if let Some(job) = held.0.lock().unwrap().take() {
            job.run();
        }
        super::super::export_tests::finish(&mut h);
    }
}
