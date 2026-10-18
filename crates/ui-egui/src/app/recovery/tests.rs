use super::*;
use crate::app::storage::StorageSetup;
use egui_kittest::{Harness, kittest::Queryable};
use serde_json::json;

/// A temp folder per test, removed when the test ends.
struct Scratch(PathBuf);
impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nodecraft-recovery-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn journal(&self) -> PathBuf {
        self.0.join("session.journal")
    }
    fn project(&self) -> PathBuf {
        self.0.join("a.ncraft")
    }
    fn kept_files(&self) -> usize {
        storage::kept_journals(&self.journal()).len()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ADDED: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([80; 16]);

/// A launch as the desktop app does it: storage over the folder's journal, the startup question
/// if there is one, and a first frame.
fn launch(dir: &Scratch, open: Option<PathBuf>) -> Harness<'static, NodeCraftApp> {
    let (session, canvas, kept, _) = NodeCraftApp::storage_session(StorageSetup { recovery: dir.journal(), open }).unwrap();
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    app.attach_recovery(dir.journal(), kept);
    let mut h = Harness::builder().with_size(egui::Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn add_node(h: &mut Harness<'static, NodeCraftApp>) {
    let canvas = h.state().canvas;
    let node = nodecraft_graph::Node::new(ADDED, "source.text");
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap();
    h.run();
}

/// A session that adds a node and ends without closing its journal, as a crash would. With
/// `saved`, the project was saved to a file first.
fn crash(dir: &Scratch, saved: bool) {
    let mut h = launch(dir, None);
    if saved {
        let location = dir.project().to_str().unwrap().to_owned();
        h.state_mut().session.dispatch("file.save_as", &json!({"location": location})).unwrap();
        h.run();
    }
    add_node(&mut h);
}

fn has_added(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.state().session.document().canvases().values().any(|c| c.nodes().contains_key(&ADDED))
}

fn asked(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.query_by_label("Recover unsaved work?").is_some()
}

#[test]
fn recover_brings_the_work_back_unsaved_at_its_file_and_saving_drops_the_copy() {
    let dir = Scratch::new("recover");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    assert!(asked(&h));
    assert!(h.query_by_label_contains("1 unsaved change to").is_some());
    assert!(h.query_by_label_contains("as a.ncraft").is_some(), "names the file it goes back to");
    assert!(!has_added(&h), "nothing changes before the person decides");
    assert_eq!(dir.kept_files(), 1, "the journal is kept until then");
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h), "the edit is back");
    let status = h.state().session.file_status();
    assert!(status.dirty, "unsaved");
    assert_eq!(status.location, Some(StorageLocation::Path(dir.project())), "at the file its session had open");
    assert!(h.state().notice.as_deref().is_some_and(|n| n.starts_with("Recovered")));
    assert!(!asked(&h));
    assert_eq!(dir.kept_files(), 1, "kept until it's saved");
    // ⌘S saves it there, and only then is the copy dropped.
    h.state_mut().session.dispatch("file.save", &json!({})).unwrap();
    h.run();
    assert_eq!(dir.kept_files(), 0);
    assert!(!storage::location_file(&dir.journal().with_file_name("session.journal.unrecovered-1")).exists());
}

#[test]
fn recovered_work_not_yet_saved_survives_a_quit_and_is_offered_again() {
    let dir = Scratch::new("quit");
    crash(&dir, false);
    let mut h = launch(&dir, None);
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h));
    // A clean quit before saving.
    h.state_mut().close_storage();
    drop(h);
    let mut h = launch(&dir, None);
    assert!(asked(&h), "the recovered work is offered again, not lost");
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h));
    // And a crash after recovering, with more edits, offers the newer copy, which holds both.
    let canvas = h.state().canvas;
    let more = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([81; 16]), "source.text");
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": more})).unwrap();
    h.run();
    drop(h);
    let mut h = launch(&dir, None);
    assert_eq!(dir.kept_files(), 2);
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h) && h.state().session.document().canvases().values().any(|c| c.nodes().len() > 1));
    // Discarding it later takes the older copy too.
    drop(h);
    let mut h = launch(&dir, None);
    h.get_by_label("Start from last save").click();
    h.run();
    assert_eq!(dir.kept_files(), 0);
}

#[test]
fn start_from_last_save_opens_that_file_and_says_so() {
    let dir = Scratch::new("discard");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    h.get_by_label("Start from last save").click();
    h.run();
    assert!(!has_added(&h));
    let status = h.state().session.file_status();
    assert!(!status.dirty);
    assert_eq!(status.location, Some(StorageLocation::Path(dir.project())), "the last save is open");
    assert_eq!(h.state().notice.as_deref(), Some("Started from the last save; the unsaved changes were discarded."));
    assert_eq!(dir.kept_files(), 0, "discarded on the person's word");
    assert!(!asked(&h));
    // An untitled project's work is simply discarded.
    let dir = Scratch::new("discard-untitled");
    crash(&dir, false);
    let mut h = launch(&dir, None);
    h.get_by_label("Start from last save").click();
    h.run();
    assert!(!has_added(&h));
    assert_eq!(h.state().notice.as_deref(), Some("The unsaved work was discarded."));
}

#[test]
fn keys_and_paste_wait_while_the_question_is_open() {
    let dir = Scratch::new("keys");
    crash(&dir, false);
    let mut h = launch(&dir, None);
    let before = h.state().session.document().clone();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Delete);
    h.event(egui::Event::Paste("a pasted note".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    h.run();
    assert_eq!(h.state().session.document(), &before, "nothing reached the canvas");
    assert!(!h.state().session.can_undo());
    assert!(asked(&h));
    // Recovery still works afterwards.
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h));
}

#[test]
fn a_damaged_journal_offers_what_replays_and_an_unreadable_one_is_kept() {
    // Damaged: a stray tail after the last whole frame.
    let dir = Scratch::new("damaged");
    crash(&dir, false);
    let mut bytes = std::fs::read(dir.journal()).unwrap();
    bytes.extend_from_slice(b"\x00\x01garbage tail");
    std::fs::write(dir.journal(), &bytes).unwrap();
    let mut h = launch(&dir, None);
    assert!(asked(&h));
    assert!(h.query_by_label_contains("can't be recovered").is_some(), "says what is lost");
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h), "the whole frames before the damage are recovered");
    // Unreadable: nothing replays; the file is kept and named, and not asked about again.
    let dir = Scratch::new("unreadable");
    std::fs::write(dir.journal(), b"not a journal at all").unwrap();
    let mut h = launch(&dir, None);
    assert!(h.query_by_label_contains("can't be replayed").is_some());
    assert!(h.query_by_label_contains("session.journal.unrecovered-1").is_some());
    h.get_by_label("Continue").click();
    h.run();
    assert_eq!(dir.kept_files(), 1, "kept for the person to deal with");
    assert!(!asked(&h));
    h.state_mut().close_storage();
    drop(h);
    let h = launch(&dir, None);
    assert!(!asked(&h), "shown once");
    assert_eq!(dir.kept_files(), 1);
}

#[test]
fn a_clean_previous_session_asks_nothing() {
    let dir = Scratch::new("clean");
    let mut h = launch(&dir, None);
    h.state_mut().close_storage();
    drop(h);
    let h = launch(&dir, None);
    assert!(!asked(&h));
    assert!(h.state().recovery.is_none());
}

#[test]
fn the_open_file_is_recorded_beside_the_journal() {
    let dir = Scratch::new("location");
    let mut h = launch(&dir, None);
    assert_eq!(storage::recorded_location(&dir.journal()), None, "untitled");
    let location = dir.project().to_str().unwrap().to_owned();
    h.state_mut().session.dispatch("file.save_as", &json!({"location": location})).unwrap();
    h.run();
    assert_eq!(storage::recorded_location(&dir.journal()), Some(dir.project()));
}
