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
    h.step();
    // The file is written on the save thread; the frame keeps polling until it lands.
    for _ in 0..10_000 {
        if !h.state().session.file_status().saving {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        h.step();
    }
    h.run();
    assert!(!h.state().session.file_status().dirty, "⌘S saved: {:?}", h.state().notice);
    assert_eq!(h.state().window_title(), "start.ncraft — NodeCraft");
}

#[test]
fn cancelling_untitled_save_and_open_is_silent() {
    let dir = Scratch::new("untitled");
    let mut h = harness(app(&dir.0, None));
    h.run();
    let original = h.state().session.document().clone();
    for key in [egui::Key::S, egui::Key::O] {
        h.key_press_modifiers(egui::Modifiers::COMMAND, key);
        h.run();
        assert!(h.state().notice.is_none());
        assert_eq!(h.state().session.document(), &original);
        assert!(h.state().session.file_status().location.is_none());
    }
}

#[test]
fn quitting_closes_the_journal_only_when_everything_is_saved() {
    let dir = Scratch::new("quit");
    let journal = dir.0.join("recovery");
    let file = dir.0.join("q.ncraft");
    // Unsaved edits: the journal stays on disk for recovery.
    {
        let mut edited = app(&dir.0, None);
        add_text(&mut edited, 70);
        edited.close_storage();
    }
    assert!(std::fs::metadata(&journal).is_ok_and(|m| m.len() > 0), "unsaved work keeps its journal");
    let _ = std::fs::remove_file(&journal);
    // Saved: the journal is closed.
    {
        let mut saved = app(&dir.0, None);
        add_text(&mut saved, 71);
        saved.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap()})).unwrap();
        saved.close_storage();
        assert!(saved.session.file_status().recovery_required, "closed after a clean save");
    }
    // Nothing done: closed too, so the next launch has nothing to keep.
    {
        let mut untouched = app(&dir.0, None);
        untouched.close_storage();
    }
    let (_, _, kept, _) = NodeCraftApp::storage_session(StorageSetup { recovery: journal.clone(), open: None }).unwrap();
    assert!(kept.is_none(), "an untouched session leaves nothing to recover");
}

#[test]
fn launching_without_edits_never_keeps_a_journal() {
    let dir = Scratch::new("relaunch");
    let journal = dir.0.join("recovery");
    for _ in 0..3 {
        // Start and quit without the clean-close (a crash right after launch): no edits, no copy.
        let (_, _, kept, _) = NodeCraftApp::storage_session(StorageSetup { recovery: journal.clone(), open: None }).unwrap();
        assert!(kept.is_none());
    }
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().filter(|e| e.as_ref().is_ok_and(|e| e.file_name().to_string_lossy().contains("unrecovered"))).count(), 0);
}

#[test]
fn a_bad_startup_path_keeps_saving_working_and_says_why() {
    let dir = Scratch::new("badpath");
    let (session, _, _, error) =
        NodeCraftApp::storage_session(StorageSetup { recovery: dir.0.join("recovery"), open: Some(dir.0.join("missing.ncraft")) }).unwrap();
    assert!(error.is_some(), "the open failure is reported");
    assert!(session.file_status().available, "and the window can still save");
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
    let (_, _, kept, _) = NodeCraftApp::storage_session(StorageSetup { recovery: journal.clone(), open: None }).unwrap();
    let kept = kept.expect("the earlier journal was kept");
    assert_eq!(std::fs::read(&kept).unwrap(), before, "byte for byte");
    assert_eq!(kept.file_name().unwrap(), "recovery.unrecovered-1");
}

#[test]
fn only_confirmed_absence_or_zero_length_can_discard_without_replay() {
    use std::io;
    for (length, expected) in [
        (Err(io::Error::from(io::ErrorKind::PermissionDenied)), true),
        (Err(io::Error::other("metadata temporarily unavailable")), true),
        (Err(io::Error::from(io::ErrorKind::NotFound)), false),
        (Ok(0), false),
    ] {
        let replayed = std::cell::Cell::new(false);
        let keep = holds_work_with_metadata(length, || {
            replayed.set(true);
            Err(Error::new(ErrorKind::Conflict, "replay unavailable"))
        });
        assert_eq!(keep, expected);
        assert!(!replayed.get(), "metadata errors must preserve work independently of replay");
    }
    assert!(holds_work_with_metadata(Ok(1), || Err(Error::new(ErrorKind::Conflict, "replay unavailable"))));
}

#[test]
fn file_picker_actions_are_discoverable_for_an_untitled_project() {
    let dir = Scratch::new("picker-palette407");
    let app = app(&dir.0, None);
    let entries = app.palette_entries();
    for id in ["file.open", "file.save", "file.save_as"] {
        assert!(entries.iter().any(|entry| entry.target == crate::palette::Target::Command(id)), "missing {id}");
    }
}

#[test]
fn location_sidecar_reads_are_capped_before_and_after_metadata() {
    use std::io::Read;
    let dir = Scratch::new("location-cap-450");
    let journal = dir.0.join("journal");
    let path = location_file(&journal);
    std::fs::write(&path, vec![b'a'; MAX_LOCATION_BYTES]).unwrap();
    assert_eq!(recorded_location(&journal).unwrap().to_str().unwrap().len(), MAX_LOCATION_BYTES);
    std::fs::write(&path, vec![b'a'; MAX_LOCATION_FILE_BYTES + 1]).unwrap();
    assert!(recorded_location(&journal).is_none());
    std::fs::write(&path, [0xff]).unwrap();
    assert!(recorded_location(&journal).is_none());
    std::fs::write(&path, "public-λ.ncraft\n").unwrap();
    assert_eq!(recorded_location(&journal), Some(PathBuf::from("public-λ.ncraft")));
    struct Growing {
        supplied: usize,
    }
    impl Read for Growing {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            out.fill(b'a');
            self.supplied += out.len();
            Ok(out.len())
        }
    }
    let mut growth = Growing { supplied: 0 };
    assert!(bounded_location(&mut growth).is_none());
    assert_eq!(growth.supplied, MAX_LOCATION_FILE_BYTES + 1);
    assert!(bounded_location(std::io::Cursor::new(vec![b'\n'; MAX_LOCATION_FILE_BYTES + 100])).is_none());
}
#[test]
fn kept_inventory_is_canonical_bounded_and_does_not_claim_chronology() {
    let dir = Scratch::new("inventory-450");
    let journal = dir.0.join("session.journal");
    for n in 1..=MAX_KEPT + 10 {
        std::fs::write(dir.0.join(format!("session.journal.unrecovered-{n}")), b"retained").unwrap();
    }
    let kept = kept_journals(&journal);
    assert_eq!(kept.len(), MAX_KEPT as usize);
    assert!(kept.iter().all(|path| path.file_name().unwrap().to_str().unwrap().rsplit('-').next().unwrap().parse::<u32>().unwrap() <= MAX_KEPT));
    assert!(dir.0.join("session.journal.unrecovered-100").exists(), "unknown slots remain untouched");
}

/// Holds a file job until the test hands it to a thread.
#[derive(Default)]
struct Held(Mutex<Option<nodecraft_engine::save_runner::SaveJob>>);
impl nodecraft_engine::save_runner::SaveRunner for Held {
    fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}

#[test]
fn quitting_during_a_background_open_cancels_it_and_keeps_the_open_project() {
    let dir = Scratch::new("quit-open367");
    let target = dir.0.join("target.ncraft");
    {
        let other = NodeCraftApp::with_services_and_storage(Default::default(), StorageSetup { recovery: dir.0.join("seed"), open: None }).unwrap();
        let mut other = other;
        add_text(&mut other, 70);
        other.session.dispatch("file.save_as", &json!({"location": target.to_str().unwrap()})).unwrap();
        other.close_storage();
    }
    let current = dir.0.join("current.ncraft");
    let mut app = app(&dir.0, None);
    app.session.dispatch("file.save_as", &json!({"location": current.to_str().unwrap()})).unwrap();
    let before = app.session.document().clone();
    let runner = Arc::new(Held::default());
    app.session.set_save_runner(runner.clone());
    let opening = app.session.dispatch("file.open", &json!({"location": target.to_str().unwrap(), "background": true})).unwrap();
    assert_eq!(opening["opening"], true);
    // The worker gets to the job only after quitting has begun, as a slow disk would.
    let job = runner.0.lock().unwrap().take().unwrap();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        job.run();
    });
    app.close_storage();
    worker.join().unwrap();
    assert_eq!(app.session.document(), &before, "the cancelled open replaced nothing");
    assert_eq!(app.session.file_status().location, Some(nodecraft_format::StorageLocation::Path(current)));
    assert!(app.session.file_status().operation.is_none());
}
