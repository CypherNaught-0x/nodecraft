//! The startup keep-aside on real native storage (#1053), with a held spawner standing in for a
//! slow disk: the first frame shows its window and frames go on while it is held; "Decide
//! later", a failure and quitting keep every byte of the journal and release it; a damaged
//! earlier copy is skipped off the UI thread; the question is built from the first replay.
use super::*;
use crate::app::storage::StorageSetup;
use egui_kittest::{Harness, kittest::Queryable};
use std::time::{Duration, Instant};

/// A temp folder per test, removed when the test ends.
struct Scratch(PathBuf);
impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nodecraft-keep1053-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn journal(&self) -> PathBuf {
        self.0.join("session.journal")
    }
    /// Every file in the folder whose name says it is a kept copy, complete or partial.
    fn copies(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            // A replayed copy's lock file isn't a copy.
            .filter(|name| name.contains("unrecovered") && !name.ends_with(".lock"))
            .collect();
        names.sort();
        names
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ADDED: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([83; 16]);

/// A session that edits and "crashes" (no clean close): its journal holds the edit.
fn crash(dir: &Scratch) -> Vec<u8> {
    let mut app = NodeCraftApp::with_services_and_storage(Default::default(), StorageSetup { recovery: dir.journal(), open: None }).unwrap();
    let canvas = app.canvas;
    let node = nodecraft_graph::Node::new(ADDED, "source.text");
    app.session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": node})).unwrap();
    drop(app);
    let bytes = std::fs::read(dir.journal()).unwrap();
    assert!(!bytes.is_empty());
    bytes
}

/// Holds each job until the test runs it, as a slow disk would.
#[derive(Default, Clone)]
struct Held(Arc<Mutex<Vec<Job>>>);
impl Held {
    fn spawn(&self) -> Spawn {
        let held = self.0.clone();
        Arc::new(move |job| {
            held.lock().unwrap().push(job);
            Ok(())
        })
    }
    fn release(&self) {
        let jobs = std::mem::take(&mut *self.0.lock().unwrap());
        assert!(!jobs.is_empty(), "a keep-aside was started");
        jobs.into_iter().for_each(|job| job());
    }
    fn drop_jobs(&self) {
        self.0.lock().unwrap().clear();
    }
}

fn launch(dir: &Scratch, spawn: &Spawn) -> Harness<'static, NodeCraftApp> {
    let setup = StorageSetup { recovery: dir.journal(), open: None };
    let (app, kept, _) = NodeCraftApp::from_storage_setup_with(Default::default(), Some(setup), "unused", spawn).unwrap();
    assert!(kept.is_none(), "nothing is kept on the calling thread");
    Harness::builder().with_size(egui::Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

/// Steps frames (the keep-aside window repaints, so step rather than run them to rest) until
/// the keep-aside has settled.
fn settle(h: &mut Harness<'static, NodeCraftApp>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.state().keeping_aside() {
        h.step();
        assert!(Instant::now() < deadline, "the keep-aside never settled");
        std::thread::sleep(Duration::from_millis(1));
    }
    h.step();
    h.step();
}

fn owned(dir: &Scratch) -> bool {
    NativeStorage::new(dir.journal()).is_err_and(|error| error.kind == ErrorKind::Busy)
}

#[test]
fn the_first_frame_shows_the_keep_aside_and_frames_go_on_while_it_is_held() {
    let dir = Scratch::new("held");
    let before = crash(&dir);
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    // The first frame: the window is up, and nothing has been read or written yet.
    h.step();
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Dialog, "Keeping unsaved work safe…").is_some(), "the first frame shows the keep-aside");
    assert!(h.query_by_label("Decide later").is_some());
    let started = Instant::now();
    for _ in 0..30 {
        h.step();
    }
    assert!(started.elapsed() < Duration::from_secs(10), "a held keep-aside doesn't block frames");
    assert!(!h.state().session.file_status().available, "the session hasn't taken the journal over");
    assert!(owned(&dir), "a second window still finds the journal owned");
    assert_eq!(std::fs::read(dir.journal()).unwrap(), before, "the journal is untouched while it waits");
    assert!(dir.copies().is_empty());
    assert!(h.state().runs.is_none(), "runs wait for the storage");
    assert!(h.state().recovery_deciding(), "nothing that could replace the document runs meanwhile");
    let canvas = h.state().canvas;
    let refused = h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": nodecraft_graph::Node::new(ADDED, "source.text")}));
    assert_eq!(refused.unwrap_err().kind, ErrorKind::Busy, "no surface changes the sample project meanwhile");

    held.release();
    settle(&mut h);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Dialog, "Keeping unsaved work safe…").is_none());
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Dialog, "Recover unsaved work?").is_some(), "then the question is asked");
    assert!(h.query_by_label_contains("1 unsaved change to").is_some());
    assert!(h.state().session.file_status().available, "and the session owns the journal");
    assert_eq!(dir.copies(), ["session.journal.unrecovered-1"], "no partial copy is left");
    assert_eq!(std::fs::read(dir.0.join("session.journal.unrecovered-1")).unwrap(), before, "kept byte for byte");
    assert!(owned(&dir));
}

#[test]
fn recover_after_a_background_keep_aside_brings_the_work_back() {
    let dir = Scratch::new("recover");
    crash(&dir);
    let mut h = launch(&dir, &thread_spawn());
    settle(&mut h);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Dialog, "Recover unsaved work?").is_some());
    h.get_by_label("Recover").click();
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.state().session.file_status().operation.is_some() || h.state().recovery.is_some() {
        h.step();
        assert!(Instant::now() < deadline, "the recovery never settled");
        std::thread::sleep(Duration::from_millis(1));
    }
    h.step();
    assert!(h.state().session.document().canvases().values().any(|c| c.nodes().contains_key(&ADDED)), "the work is back");
    assert_eq!(dir.copies(), ["session.journal.unrecovered-1"], "kept until the work is saved");
}

#[test]
fn decide_later_keeps_every_byte_and_leaves_the_journal_for_the_next_launch() {
    let dir = Scratch::new("later");
    let before = crash(&dir);
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    h.step();
    h.get_by_label("Decide later").click();
    h.step();
    assert!(h.state().keep_aside.as_ref().is_some_and(|job| job.cancelling()), "the cancel was asked for");
    held.release();
    settle(&mut h);
    let notice = h.state().notice().map(str::to_owned).unwrap_or_default();
    assert!(notice.contains("offers it again the next time it starts"), "{notice}");
    assert!(!h.state().session.file_status().available, "this window can't save, and says so");
    assert_eq!(std::fs::read(dir.journal()).unwrap(), before, "the journal is as it was");
    assert!(dir.copies().is_empty(), "no copy, partial or not");
    assert!(!owned(&dir), "the journal is released");
    drop(h);
    // The next launch keeps it aside as usual.
    let mut next = launch(&dir, &thread_spawn());
    settle(&mut next);
    assert!(next.query_by_role_and_label(egui::accesskit::Role::Dialog, "Recover unsaved work?").is_some());
    assert_eq!(std::fs::read(dir.0.join("session.journal.unrecovered-1")).unwrap(), before);
}

#[test]
fn quitting_while_it_is_held_or_dropped_keeps_every_byte() {
    let dir = Scratch::new("quit");
    let before = crash(&dir);
    // Quitting with the job still held: it is cancelled and runs (as a thread would) meanwhile.
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    h.step();
    let release = {
        let held = held.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            held.release();
        })
    };
    h.state_mut().close_storage();
    release.join().unwrap();
    assert!(!h.state().keeping_aside());
    assert_eq!(std::fs::read(dir.journal()).unwrap(), before);
    assert!(dir.copies().is_empty());
    assert!(!owned(&dir), "the journal is released");
    drop(h);
    // A job dropped without running (its runner went away) says so and loses nothing.
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    h.step();
    held.drop_jobs();
    settle(&mut h);
    assert!(h.state().notice().is_some_and(|n| n.contains("stopped before it finished")), "{:?}", h.state().notice());
    assert_eq!(std::fs::read(dir.journal()).unwrap(), before);
    assert!(dir.copies().is_empty());
}

#[test]
fn quitting_after_it_finished_but_before_a_frame_took_it_in_decides_the_journal_as_usual() {
    let dir = Scratch::new("quit-late");
    let before = crash(&dir);
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    h.step();
    held.release();
    h.state_mut().close_storage();
    assert!(!h.state().keeping_aside());
    assert_eq!(std::fs::read(dir.0.join("session.journal.unrecovered-1")).unwrap(), before, "the copy is kept");
    drop(h);
    // The kept copy is offered next time, and the live journal isn't kept a second time.
    let mut next = launch(&dir, &thread_spawn());
    settle(&mut next);
    assert!(next.query_by_role_and_label(egui::accesskit::Role::Dialog, "Recover unsaved work?").is_some());
    assert_eq!(dir.copies(), ["session.journal.unrecovered-1"]);
}

#[test]
fn a_damaged_earlier_copy_is_skipped_off_the_ui_thread() {
    let dir = Scratch::new("damaged");
    let good = crash(&dir);
    // The work was kept earlier (slot 1) and the live journal is clean; a later copy (slot 2)
    // is damaged. Many damaged copies would take as long to skip: none is replayed on the UI
    // thread.
    std::fs::write(dir.0.join("session.journal.unrecovered-1"), &good).unwrap();
    std::fs::write(dir.0.join("session.journal.unrecovered-2"), b"not a journal").unwrap();
    std::fs::remove_file(dir.journal()).unwrap();
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    h.step();
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Dialog, "Keeping unsaved work safe…").is_some(), "the window is up before any copy is read");
    held.release();
    settle(&mut h);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Dialog, "Recover unsaved work?").is_some(), "the copy that replays is offered");
    assert_eq!(h.state().recovery.as_ref().map(|offer| offer.kept.clone()), Some(dir.0.join("session.journal.unrecovered-1")));
    assert_eq!(std::fs::read(dir.0.join("session.journal.unrecovered-2")).unwrap(), b"not a journal", "the damaged copy is left for the person");
}

#[test]
fn a_clean_start_needs_no_keep_aside() {
    let dir = Scratch::new("clean");
    let held = Held::default();
    let h = launch(&dir, &held.spawn());
    assert!(!h.state().keeping_aside());
    assert!(held.0.lock().unwrap().is_empty(), "nothing was spawned");
    assert!(h.state().session.file_status().available);
}

#[test]
fn a_journal_the_recovery_drive_cannot_copy_is_refused_and_kept_in_place() {
    let dir = Scratch::new("space");
    let before = crash(&dir);
    let mut storage = NativeStorage::new(dir.journal()).unwrap();
    storage.set_space_probe(Arc::new(|_| SpaceProbe { recovery_free: Some(1), ..SpaceProbe::default() }));
    let error = keep(&dir.journal(), &storage, &TransferCancel::default(), &Progress::default(), true).err().expect("refused");
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.message.contains("keep the last session's unsaved work safe") && error.message.contains("free"), "{}", error.message);
    assert!(!error.message.contains(dir.0.to_str().unwrap()), "sizes, never paths");
    drop(storage);
    assert_eq!(std::fs::read(dir.journal()).unwrap(), before);
    assert!(dir.copies().is_empty());
}

#[test]
fn a_cancel_lands_at_the_next_chunk_and_removes_the_partial_copy() {
    let dir = Scratch::new("chunk");
    let source = dir.0.join("big");
    let bytes: Vec<u8> = (0..(3 * COPY_CHUNK + 17)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &bytes).unwrap();
    let target = dir.0.join("big.unrecovered-1");
    let cancel = TransferCancel::default();
    let mut reports = Vec::new();
    let error = copy_aside(&source, &target, &cancel, &mut |copied| {
        reports.push(copied);
        // A cancel asked for while the first chunk was written.
        cancel.cancel();
    })
    .unwrap_err();
    assert!(TransferCancel::is_cancellation(&error));
    assert_eq!(reports, [COPY_CHUNK as u64], "it stopped at the next chunk");
    assert!(!target.exists() && !partial(&target).exists(), "nothing is left in the slot or beside it");
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    // Uncancelled, it copies every byte, through the partial name, and reports as it goes.
    let mut reports = Vec::new();
    copy_aside(&source, &target, &TransferCancel::default(), &mut |copied| reports.push(copied)).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert!(!partial(&target).exists());
    assert_eq!(reports.last().copied(), Some(bytes.len() as u64));
    // A partial copy a forced quit left behind is replaced, not appended to.
    std::fs::remove_file(&target).unwrap();
    std::fs::write(partial(&target), b"stale").unwrap();
    copy_aside(&source, &target, &TransferCancel::default(), &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
}

#[test]
fn the_question_is_built_from_the_replay_that_decided_to_keep_the_journal() {
    let dir = Scratch::new("one-replay");
    crash(&dir);
    let storage = NativeStorage::new(dir.journal()).unwrap();
    let kept = keep(&dir.journal(), &storage, &TransferCancel::default(), &Progress::default(), true).unwrap();
    let path = kept.kept.clone().unwrap();
    // The copy is damaged after it was written: a second replay of it would fail, so a question
    // that still offers the work came from the first replay.
    std::fs::write(&path, b"damaged after keeping").unwrap();
    let offer = kept.offer.unwrap();
    assert_eq!(offer.kept, path);
    let found = offer.found.as_ref().expect("the work is offered");
    assert_eq!(found.changes, 1);
    assert!(found.snapshot.project.canvases().values().any(|c| c.nodes().contains_key(&ADDED)));
}

#[test]
fn the_window_says_where_it_is() {
    assert!(describe(Step::Reading, false).0.contains("Reading"));
    let (said, fraction) = describe(Step::Copying { copied: 512 * 1024 * 1024, total: 2 * 1024 * 1024 * 1024 }, false);
    assert!(said.contains("512 MB of 2.0 GB"), "{said}");
    assert_eq!(fraction, Some(0.25));
    assert_eq!(describe(Step::Copying { copied: 0, total: 0 }, false).1, Some(1.0));
    assert!(describe(Step::Earlier, false).0.contains("earlier"));
    assert!(describe(Step::Copying { copied: 1, total: 2 }, true).0.starts_with("Stopping"));
}

#[test]
#[ignore = "writes review screenshots to NODECRAFT_REVIEW_EVIDENCE"]
fn render_keep_aside1053_evidence() {
    let output = PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let dir = Scratch::new("evidence");
    crash(&dir);
    let held = Held::default();
    let setup = StorageSetup { recovery: dir.journal(), open: None };
    let (app, _, _) = NodeCraftApp::from_storage_setup_with(Default::default(), Some(setup), "unused", &held.spawn()).unwrap();
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.step();
    h.render().unwrap().save(output.join("keep-aside-reading.png")).unwrap();
    // As a large journal's copy would show it part way.
    if let Some(job) = &h.state().keep_aside {
        job.progress.set(Step::Copying { copied: 1288 * 1024 * 1024, total: 3482 * 1024 * 1024 });
    }
    h.step();
    h.step();
    h.render().unwrap().save(output.join("keep-aside-copying.png")).unwrap();
    h.get_by_label("Decide later").click();
    h.step();
    held.release();
    settle(&mut h);
    h.render().unwrap().save(output.join("keep-aside-decided-later.png")).unwrap();
    drop(h);
    // The next launch keeps it aside and asks.
    let setup = StorageSetup { recovery: dir.journal(), open: None };
    let (app, _, _) = NodeCraftApp::from_storage_setup_with(Default::default(), Some(setup), "unused", &thread_spawn()).unwrap();
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    settle(&mut h);
    h.render().unwrap().save(output.join("keep-aside-then-asked.png")).unwrap();
}

/// A crashed session with media: images imported and placed, saved to `file` first when given.
fn crash_with_media(dir: &Scratch, file: Option<&Path>) -> Vec<nodecraft_types::Blake3> {
    let mut app = NodeCraftApp::with_services_and_storage(Default::default(), StorageSetup { recovery: dir.journal(), open: None }).unwrap();
    let canvas = app.canvas;
    let mut hashes = Vec::new();
    for n in 0..6u8 {
        let hash = app.session.import_asset(&vec![n; 256 * 1024 + usize::from(n)]).unwrap();
        let mut node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([100 + n; 16]), "source.text");
        node.params.insert(
            "content".into(),
            nodecraft_types::Value::Media(nodecraft_types::AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: Default::default() }),
        );
        app.session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": node})).unwrap();
        hashes.push(hash);
        if n == 2
            && let Some(file) = file
        {
            app.session.dispatch("file.save_as", &serde_json::json!({"location": file.to_str().unwrap()})).unwrap();
        }
    }
    let node = nodecraft_graph::Node::new(ADDED, "source.text");
    app.session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": node})).unwrap();
    // As the window's frames record it (`track_recovery`).
    if let Some(file) = file {
        storage::record_location(&dir.journal(), Some(file)).unwrap();
    }
    hashes
}

fn until_idle(h: &mut Harness<'static, NodeCraftApp>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while h.state().session.file_status().operation.is_some() || h.state().files.owns_input() {
        h.step();
        assert!(Instant::now() < deadline, "the file operation never settled: {:?}", h.state().notice());
        std::thread::sleep(Duration::from_millis(1));
    }
    h.step();
}

fn readable(app: &NodeCraftApp, hash: &nodecraft_types::Blake3) -> bool {
    app.session.assets().verified_content(hash, &TransferCancel::default(), &StreamMetrics::default()).is_ok()
}

/// The question's media live in the session's own workspace (the reused replay): recovering
/// them and saving, again and again, still fits its private quota.
#[test]
fn recovered_media_from_the_reused_replay_save_and_save_again() {
    let dir = Scratch::new("media-recover");
    let hashes = crash_with_media(&dir, None);
    let mut h = launch(&dir, &thread_spawn());
    settle(&mut h);
    h.get_by_label("Recover").click();
    h.step();
    until_idle(&mut h);
    assert!(h.state().recovery.is_none(), "{:?}", h.state().notice());
    assert!(hashes.iter().all(|hash| readable(h.state(), hash)));
    let file = dir.0.join("recovered.ncraft");
    for round in 0..3 {
        let saved = h.state_mut().session.dispatch("file.save_as", &serde_json::json!({"location": file.to_str().unwrap()}));
        assert!(saved.is_ok(), "save {round}: {saved:?}");
        h.step();
        h.step();
    }
    assert!(dir.copies().is_empty(), "the saved work's copy is released");
    assert!(hashes.iter().all(|hash| readable(h.state(), hash)));
}

/// "Start from last save" opens the file while the question's media still live in the
/// session's workspace: the open fits beside them.
#[test]
fn start_from_last_save_opens_beside_the_reused_replays_media() {
    let dir = Scratch::new("media-last-save");
    let file = dir.0.join("saved.ncraft");
    let hashes = crash_with_media(&dir, Some(&file));
    let mut h = launch(&dir, &thread_spawn());
    settle(&mut h);
    h.get_by_label("Start from last save").click();
    h.step();
    until_idle(&mut h);
    assert!(h.state().recovery.is_none(), "the open succeeded: {:?}", h.state().notice());
    assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(file)));
    assert!(hashes.iter().take(3).all(|hash| readable(h.state(), hash)), "the saved media open");
    assert!(dir.copies().is_empty(), "the discarded copy is gone");
}

#[test]
fn decide_later_while_only_looking_for_earlier_copies_still_takes_the_journal_over() {
    let dir = Scratch::new("later-earlier");
    let good = crash(&dir);
    std::fs::write(dir.0.join("session.journal.unrecovered-1"), &good).unwrap();
    std::fs::remove_file(dir.journal()).unwrap();
    let held = Held::default();
    let mut h = launch(&dir, &held.spawn());
    h.step();
    h.get_by_label("Decide later").click();
    h.step();
    held.release();
    settle(&mut h);
    assert!(h.state().session.file_status().available, "the live journal held nothing, so the window can save");
    assert!(h.state().recovery.is_none());
    assert!(h.state().notice().is_some_and(|n| n.contains("offered the next time")), "{:?}", h.state().notice());
    assert_eq!(std::fs::read(dir.0.join("session.journal.unrecovered-1")).unwrap(), good, "the copy stays");
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": nodecraft_graph::Node::new(ADDED, "source.text")})).unwrap();
}

/// #367: each replay of an earlier copy is charged, as the live journal's is. A copy whose replay
/// wouldn't fit stops the scan: the window still takes the journal over, says the earlier work
/// wasn't checked, and the copy stays.
#[test]
fn an_earlier_copy_whose_replay_would_not_fit_is_left_for_next_time() {
    let dir = Scratch::new("earlier-space");
    let good = crash(&dir);
    std::fs::write(dir.0.join("session.journal.unrecovered-1"), &good).unwrap();
    std::fs::remove_file(dir.journal()).unwrap();
    let mut storage = NativeStorage::new(dir.journal()).unwrap();
    storage.set_space_probe(Arc::new(|_| SpaceProbe { recovery_free: Some(1), ..SpaceProbe::default() }));
    let kept = keep(&dir.journal(), &storage, &TransferCancel::default(), &Progress::default(), true).unwrap();
    assert!(kept.skipped && kept.offer.is_none() && kept.kept.is_none());
    let why = kept.skipped_why.expect("the reason is kept for the notice");
    assert!(why.contains("free") && !why.contains(dir.0.to_str().unwrap()), "sizes, never paths: {why}");
    assert_eq!(std::fs::read(dir.0.join("session.journal.unrecovered-1")).unwrap(), good, "the copy is left as it was");
}
