use super::*;
use egui_kittest::{Harness, kittest::Queryable};
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

/// Steps frames until no file operation runs or waits on the dialogs.
fn settle(h: &mut Harness<'static, NodeCraftApp>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        h.step();
        if h.state().session.file_status().operation.is_none() && !h.state().files.owns_input() {
            h.run();
            return;
        }
        assert!(std::time::Instant::now() < deadline, "the file operation did not settle");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
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
    let has_60 =
        |app: &NodeCraftApp| app.session.document().canvases().values().any(|c| c.nodes().contains_key(&nodecraft_types::NodeId::from_bytes([60; 16])));
    assert!(!has_60(&opened), "the file opens with the first frame, not before the window exists");
    let mut h = harness(opened);
    settle(&mut h);
    assert!(has_60(h.state()), "{:?}", h.state().notice());
    assert!(h.state().notices.is_empty());
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
    assert!(!h.state().session.file_status().dirty, "⌘S saved: {:?}", h.state().notice());
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
        assert!(h.state().notices.is_empty());
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
    let mut h = harness(app(&dir.0, Some(dir.0.join("missing.ncraft"))));
    settle(&mut h);
    assert_eq!(
        h.state().notice(),
        Some(
            "missing.ncraft couldn't be opened: the selected project cannot be found or inspected; choose another file. The new project is open instead and can be saved."
        ),
        "the open failure is reported"
    );
    assert!(h.state().session.file_status().available, "and the window can still save");
    assert!(h.state().session.file_status().location.is_none());
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

/// A saved project at `dir/<name>` holding a Text node `n`, written by a session of its own.
fn saved_project(dir: &Path, name: &str, n: u8) -> PathBuf {
    let file = dir.join(name);
    let mut seed = app(&dir.join(format!("seed-{name}")), None);
    add_text(&mut seed, n);
    seed.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap()})).unwrap();
    seed.close_storage();
    file
}
fn holds(app: &NodeCraftApp, n: u8) -> bool {
    app.session.document().canvases().values().any(|c| c.nodes().contains_key(&nodecraft_types::NodeId::from_bytes([n; 16])))
}
/// The startup open's job, which the first frame hands to the runner.
fn held_job(runner: &Held) -> nodecraft_engine::save_runner::SaveJob {
    runner.0.lock().unwrap().take().expect("the first frame started the open on the runner")
}

#[test]
fn startup_open_shows_progress_from_the_first_frame_and_frames_keep_coming_while_it_is_held() {
    let dir = Scratch::new("startup-progress367");
    let file = saved_project(&dir.0, "big.ncraft", 90);
    let mut app = app(&dir.0, Some(file.clone()));
    let runner = Arc::new(Held::default());
    app.session.set_save_runner(runner.clone());
    let starter = app.session.document().clone();
    let mut h = harness(app);
    h.step();
    // The very first frame already shows the open, before it has read anything.
    assert!(h.query_by_label("Opening project…").is_some(), "progress shows from the first frame");
    assert!(h.query_by_label("Cancel").is_some());
    assert_eq!(h.state().window_title(), "Untitled • (opening…) — NodeCraft");
    assert!(runner.0.lock().unwrap().is_some(), "the open waits on the runner, not the frame");
    // While the runner holds the open, the event pump keeps running and asking for frames.
    let first = h.ctx.cumulative_frame_nr();
    for _ in 0..30 {
        h.step();
        assert!(h.output().viewport_output.get(&egui::ViewportId::ROOT).unwrap().repaint_delay <= std::time::Duration::from_millis(16));
    }
    assert_eq!(h.ctx.cumulative_frame_nr(), first + 30, "frames advanced while the open was held");
    assert!(h.query_by_label("Opening project…").is_some());
    assert_eq!(h.state().session.document(), &starter, "nothing is replaced until the open settles");
    // Input belongs to the progress window: a shortcut doesn't reach the canvas behind it.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Delete);
    h.step();
    assert_eq!(h.state().session.document(), &starter);
    held_job(&runner).run();
    settle(&mut h);
    assert!(holds(h.state(), 90), "the file is open: {:?}", h.state().notice());
    assert!(h.state().notices.is_empty());
    assert!(h.query_by_label("Opening project…").is_none());
    assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(file.clone())));
    assert_eq!(h.state().files.projects.recent(), vec![file.to_str().unwrap()]);
    assert_eq!(h.state().canvas, crate::app::sheet_view::home_canvas(h.state().session.document()).unwrap());
}

#[test]
fn cancelling_the_startup_open_keeps_the_new_project_and_says_so() {
    let dir = Scratch::new("startup-cancel367");
    let file = saved_project(&dir.0, "big.ncraft", 91);
    let mut app = app(&dir.0, Some(file.clone()));
    let runner = Arc::new(Held::default());
    app.session.set_save_runner(runner.clone());
    let starter = app.session.document().clone();
    let mut h = harness(app);
    h.step();
    h.get_by_label("Cancel").click();
    // The modal button sees the press in one frame and the release in the next.
    h.step();
    h.step();
    assert!(h.state().session.file_status().operation.is_some_and(|operation| operation.cancel_requested));
    assert!(h.query_by_label_contains("Cancelling").is_some(), "the window says the step finishes first");
    held_job(&runner).run();
    settle(&mut h);
    assert_eq!(h.state().session.document(), &starter, "the cancelled open replaced nothing");
    assert_eq!(h.state().notice(), Some("Opening big.ncraft was cancelled; the new project is open instead."));
    let status = h.state().session.file_status();
    assert!(status.available && status.location.is_none() && status.operation.is_none());
    assert!(h.state().files.projects.recent().is_empty());
    // The window works on: the file can still be opened by hand.
    h.state_mut().session.dispatch("file.open", &json!({"location": file.to_str().unwrap(), "discard_dirty": true})).unwrap();
    assert!(holds(h.state(), 91));
}

#[test]
fn a_startup_open_the_runner_refuses_runs_inline_and_still_opens() {
    struct Refuses;
    impl nodecraft_engine::save_runner::SaveRunner for Refuses {
        fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
            Err(job)
        }
    }
    let dir = Scratch::new("startup-refused367");
    let file = saved_project(&dir.0, "small.ncraft", 92);
    let mut app = app(&dir.0, Some(file));
    app.session.set_save_runner(Arc::new(Refuses));
    let mut h = harness(app);
    h.step();
    assert!(holds(h.state(), 92), "opened inline in the first frame: {:?}", h.state().notice());
    assert!(!h.state().files.owns_input());
    settle(&mut h);
    assert!(h.state().notices.is_empty());
}

#[test]
#[ignore = "writes review screenshots to NODECRAFT_REVIEW_EVIDENCE"]
fn render_startup_open367_evidence() {
    let output = PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let dir = Scratch::new("startup-evidence367");
    let file = saved_project(&dir.0, "Public-forest.ncraft", 93);
    let mut app = app(&dir.0, Some(file));
    let runner = Arc::new(Held::default());
    app.session.set_save_runner(runner.clone());
    let mut h = harness(app);
    h.step();
    h.render().unwrap().save(output.join("startup-first-frame.png")).unwrap();
    h.get_by_label("Cancel").click();
    h.step();
    h.step();
    h.render().unwrap().save(output.join("startup-cancelling.png")).unwrap();
    held_job(&runner).run();
    settle(&mut h);
    h.render().unwrap().save(output.join("startup-cancelled-notice.png")).unwrap();
}

#[test]
fn a_command_before_the_first_frame_is_refused_while_the_startup_open_runs_and_never_dropped() {
    let dir = Scratch::new("startup-race367");
    let file = saved_project(&dir.0, "raced.ncraft", 94);
    // As the desktop shell does: the startup open starts first in the frame's logic, then the
    // control channel, MCP and the agent are pumped.
    let mut first = app(&dir.0, Some(file.clone()));
    let runner = Arc::new(Held::default());
    first.session.set_save_runner(runner.clone());
    first.begin_startup_open();
    let canvas = first.canvas;
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([95; 16]), "source.text");
    let error = first.session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.message.contains("still opening") && error.message.contains("file.cancel"), "{}", error.message);
    held_job(&runner).run();
    let mut h = harness(first);
    settle(&mut h);
    assert!(holds(h.state(), 94), "the file opened: {:?}", h.state().notice());
    // A host that edits before starting the open: the edit is kept, not opened over.
    let dir = Scratch::new("startup-race-kept367");
    let file = saved_project(&dir.0, "kept.ncraft", 96);
    let mut second = app(&dir.0, Some(file));
    add_text(&mut second, 97);
    let edited = second.session.document().clone();
    let mut h = harness(second);
    settle(&mut h);
    assert_eq!(h.state().session.document(), &edited, "the edit survives");
    assert!(!holds(h.state(), 96));
    assert_eq!(
        h.state().notice(),
        Some("kept.ncraft wasn't opened at startup because this window's project already has changes. Save them, then use File > Open.")
    );
}

/// A project file in a format version this build doesn't know, as a newer NodeCraft would write.
fn future_format(path: &Path) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("project.json", stored).unwrap();
    zip.write_all(br#"{"version":99,"epoch":1,"project":{}}"#).unwrap();
    zip.finish().unwrap();
}

#[test]
fn a_newer_format_at_startup_keeps_the_advice_to_update() {
    let dir = Scratch::new("startup-future367");
    let file = dir.0.join("future.ncraft");
    future_format(&file);
    let mut h = harness(app(&dir.0, Some(file)));
    settle(&mut h);
    assert_eq!(
        h.state().notice(),
        Some(
            "future.ncraft couldn't be opened: unsupported project format version; update NodeCraft or use a compatible exporter. The new project is open instead and can be saved."
        )
    );
}
