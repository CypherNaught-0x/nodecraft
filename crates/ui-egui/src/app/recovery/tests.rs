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
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ADDED: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([80; 16]);

/// A session that adds a node and ends without closing its journal, as a crash would.
fn crash(dir: &Scratch) {
    let (mut session, canvas, _, _) = NodeCraftApp::storage_session(StorageSetup { recovery: dir.journal(), open: None }).unwrap();
    let node = nodecraft_graph::Node::new(ADDED, "source.text");
    session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap();
}

/// The next launch: the window with the startup question, if there is one.
fn relaunch(dir: &Scratch) -> Harness<'static, NodeCraftApp> {
    let (session, canvas, kept, _) = NodeCraftApp::storage_session(StorageSetup { recovery: dir.journal(), open: None }).unwrap();
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    app.recovery = kept.map(RecoveryOffer::read);
    let mut h = Harness::builder().with_size(egui::Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn has_added(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.state().session.document().canvases().values().any(|c| c.nodes().contains_key(&ADDED))
}

fn kept_files(dir: &Scratch) -> usize {
    std::fs::read_dir(&dir.0).unwrap().filter(|e| e.as_ref().is_ok_and(|e| e.file_name().to_string_lossy().contains("unrecovered"))).count()
}

#[test]
fn recover_brings_the_work_back_untitled_and_unsaved() {
    let dir = Scratch::new("recover");
    crash(&dir);
    let mut h = relaunch(&dir);
    assert!(h.query_by_label("Recover unsaved work?").is_some());
    assert!(h.query_by_label_contains("1 unsaved change to").is_some());
    assert!(!has_added(&h), "nothing changes before the person decides");
    assert_eq!(kept_files(&dir), 1, "the journal is kept until then");
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h), "the edit is back");
    let status = h.state().session.file_status();
    assert!(status.dirty && status.location.is_none(), "untitled and unsaved");
    assert_eq!(h.state().notice.as_deref(), Some("Recovered. Save the project to keep it."));
    assert_eq!(kept_files(&dir), 0);
    assert!(h.query_by_label("Recover unsaved work?").is_none());
}

#[test]
fn start_from_last_save_discards_it_and_says_nothing_more() {
    let dir = Scratch::new("discard");
    crash(&dir);
    let mut h = relaunch(&dir);
    h.get_by_label("Start from last save").click();
    h.run();
    assert!(!has_added(&h));
    assert_eq!(kept_files(&dir), 0, "discarded on the person's word");
    assert!(h.query_by_label("Recover unsaved work?").is_none());
}

#[test]
fn a_damaged_journal_offers_what_replays_and_an_unreadable_one_is_kept() {
    // Damaged: a stray tail after the last whole frame.
    let dir = Scratch::new("damaged");
    crash(&dir);
    let mut bytes = std::fs::read(dir.journal()).unwrap();
    bytes.extend_from_slice(b"\x00\x01garbage tail");
    std::fs::write(dir.journal(), &bytes).unwrap();
    let mut h = relaunch(&dir);
    assert!(h.query_by_label_contains("Part of the record was damaged").is_some());
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h), "the whole frames before the damage are recovered");
    // Unreadable: nothing replays; the file is kept and named.
    let dir = Scratch::new("unreadable");
    std::fs::write(dir.journal(), b"not a journal at all").unwrap();
    let mut h = relaunch(&dir);
    assert!(h.query_by_label_contains("can't be replayed").is_some());
    assert!(h.query_by_label_contains("session.journal.unrecovered-1").is_some());
    h.get_by_label("Continue").click();
    h.run();
    assert_eq!(kept_files(&dir), 1, "kept for the person to deal with");
    assert!(h.query_by_label("Recover unsaved work?").is_none());
}

#[test]
fn a_clean_previous_session_asks_nothing() {
    let dir = Scratch::new("clean");
    {
        let (mut session, _, _, _) = NodeCraftApp::storage_session(StorageSetup { recovery: dir.journal(), open: None }).unwrap();
        session.close_recovery(true).unwrap();
    }
    let h = relaunch(&dir);
    assert!(h.query_by_label("Recover unsaved work?").is_none());
    assert!(h.state().recovery.is_none());
}
