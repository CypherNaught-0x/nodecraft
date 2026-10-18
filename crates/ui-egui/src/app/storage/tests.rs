use super::*;
use egui_kittest::Harness;
use std::path::Path;

/// A temp folder per test, removed when the test ends.
struct Scratch(PathBuf);
impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nodecraft-storage-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(dir: &Path, open: Option<PathBuf>) -> NodeCraftApp {
    NodeCraftApp::with_services_and_storage(Default::default(), StorageSetup { recovery: dir.join("recovery"), open }).unwrap()
}

fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_size(egui::Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

fn add_text(app: &mut NodeCraftApp, n: u8) {
    let canvas = app.canvas;
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([n; 16]), "source.text");
    app.session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap();
}

#[test]
fn save_as_then_edit_then_open_restores_the_saved_project() {
    let dir = Scratch::new("roundtrip");
    let file = dir.0.join("forest.ncraft");
    let mut app = app(&dir.0, None);
    assert!(app.session.file_status().available, "storage is wired");
    assert_eq!(app.window_title(), "Untitled • — NodeCraft");
    add_text(&mut app, 50);
    app.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap()})).unwrap();
    let saved = app.session.document().clone();
    assert_eq!(app.window_title(), "forest.ncraft — NodeCraft");
    add_text(&mut app, 51);
    assert!(app.session.file_status().dirty);
    app.session.dispatch("file.open", &json!({"location": file.to_str().unwrap(), "discard_dirty": true})).unwrap();
    assert_eq!(app.session.document(), &saved, "the saved graph is back");
    assert!(!app.session.can_undo(), "undo starts fresh");
}

#[test]
fn a_project_path_at_startup_is_opened_and_cmd_s_saves_it() {
    let dir = Scratch::new("startup");
    let file = dir.0.join("start.ncraft");
    {
        let mut first = app(&dir.0.join("one"), None);
        add_text(&mut first, 60);
        first.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap()})).unwrap();
        first.close_storage();
    }
    let opened = app(&dir.0.join("two"), Some(file.clone()));
    assert!(opened.session.document().canvases().values().any(|c| c.nodes().contains_key(&nodecraft_types::NodeId::from_bytes([60; 16]))));
    let mut h = harness(opened);
    h.run();
    add_text(h.state_mut(), 61);
    assert!(h.state().session.file_status().dirty);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    h.run();
    assert!(!h.state().session.file_status().dirty, "⌘S saved: {:?}", h.state().notice);
    assert_eq!(h.state().window_title(), "start.ncraft — NodeCraft");
}

#[test]
fn an_untitled_project_says_how_to_give_it_a_file() {
    let dir = Scratch::new("untitled");
    let mut h = harness(app(&dir.0, None));
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    h.run();
    assert_eq!(h.state().notice.as_deref(), Some("This project has no file yet. Start NodeCraft with a project path to save to it."));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    h.run();
    assert_eq!(h.state().notice.as_deref(), Some("To open a project, start NodeCraft with its path."));
}

#[test]
fn quitting_closes_the_journal_only_when_everything_is_saved() {
    let dir = Scratch::new("quit");
    let file = dir.0.join("q.ncraft");
    let mut app = app(&dir.0, None);
    add_text(&mut app, 70);
    // Unsaved: the journal stays for recovery.
    app.close_storage();
    assert!(!app.session.file_status().recovery_required || app.session.file_status().dirty);
    app.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap()})).unwrap();
    app.close_storage();
    assert!(app.session.file_status().recovery_required, "closed after a clean save");
}

#[test]
fn a_second_window_cannot_take_the_same_journal() {
    let dir = Scratch::new("lock");
    let _first = app(&dir.0, None);
    let second = NodeCraftApp::with_services_and_storage(Default::default(), StorageSetup { recovery: dir.0.join("recovery"), open: None });
    assert!(second.is_err(), "the recovery journal is locked");
}

#[test]
fn an_earlier_sessions_journal_is_kept_not_overwritten() {
    let dir = Scratch::new("keep");
    let journal = dir.0.join("recovery");
    {
        // A session edits and "crashes": no clean close, so its journal stays.
        let mut crashed = app(&dir.0, None);
        add_text(&mut crashed, 80);
    }
    let before = std::fs::read(&journal).unwrap();
    assert!(!before.is_empty());
    let (_, _, kept) = NodeCraftApp::storage_session(StorageSetup { recovery: journal.clone(), open: None }).unwrap();
    let kept = kept.expect("the earlier journal was kept");
    assert_eq!(std::fs::read(&kept).unwrap(), before, "byte for byte");
    assert_eq!(kept.file_name().unwrap(), "recovery.unrecovered-1");
}
