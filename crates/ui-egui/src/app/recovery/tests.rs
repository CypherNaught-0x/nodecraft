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
    h.state_mut().session.dispatch("node.create", &json!({"canvas": canvas, "id": ADDED, "kind": "source.text", "pos": {"x": 260.0, "y": 40.0}})).unwrap();
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
    assert!(h.query_by_label_contains("1 unsaved change to a.ncraft can be recovered").is_some());
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
    let more = nodecraft_types::NodeId::from_bytes([81; 16]);
    h.state_mut().session.dispatch("node.create", &json!({"canvas": canvas, "id": more, "kind": "source.text", "pos": {"x": 0.0, "y": 400.0}})).unwrap();
    h.run();
    drop(h);
    let mut h = launch(&dir, None);
    assert_eq!(dir.kept_files(), 2);
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h) && h.state().session.document().canvases().values().any(|c| c.nodes().len() > 1));
    // Resolving one selected copy does not prove any other snapshot is subsumed.
    drop(h);
    let mut h = launch(&dir, None);
    let selected = h.state().recovery.as_ref().unwrap().kept.clone();
    let others: Vec<_> = storage::kept_journals(&dir.journal())
        .into_iter()
        .filter(|path| path != &selected)
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(!others.is_empty());
    h.get_by_label("Start from last save").click();
    h.run();
    assert!(!selected.exists());
    for (path, bytes) in others {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
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
    assert_eq!(h.state().notice.as_deref(), Some("Started from the last save; the selected unsaved recovery copy was discarded."));
    assert_eq!(dir.kept_files(), 0, "discarded on the person's word");
    assert!(!asked(&h));
    // An untitled project's work is simply discarded.
    let dir = Scratch::new("discard-untitled");
    crash(&dir, false);
    let mut h = launch(&dir, None);
    h.get_by_label("Start from last save").click();
    h.run();
    assert!(!has_added(&h));
    assert_eq!(h.state().notice.as_deref(), Some("The selected unsaved recovery copy was discarded."));
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
    h.get_by_label("Recovery stopped at an incomplete or damaged record. Only complete changes before that point are included.");
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

#[test]
#[ignore = "manual public-safe PR evidence"]
fn render_recovery_review_evidence() {
    let folder = PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let dir = Scratch::new("evidence");
    crash(&dir, true);
    let (session, canvas, kept, _) = NodeCraftApp::storage_session(StorageSetup { recovery: dir.journal(), open: None }).unwrap();
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    app.attach_recovery(dir.journal(), kept);
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h.render().unwrap().save(folder.join("prompt.png")).unwrap();
    h.get_by_label("Recover").click();
    h.run();
    h.run();
    h.render().unwrap().save(folder.join("recovered.png")).unwrap();
}

#[test]
fn genuine_comments_v2_journal_is_offered_recovered_and_saved_by_the_desktop() {
    let dir = Scratch::new("classic-v2");
    let bytes = include_bytes!("../../../../format/tests/fixtures/v2/comments-v2.ncrj");
    std::fs::write(dir.journal(), bytes).unwrap();
    let expected = nodecraft_format::journal::recover(bytes).unwrap().snapshot.project;
    let mut h = launch(&dir, None);
    assert!(asked(&h));
    assert_eq!(h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().changes, 3);
    h.get_by_label("Recover").click();
    h.run();
    assert_eq!(*h.state().session.document(), expected);
    assert!(h.state().session.document().canvases().values().any(|canvas| !canvas.comments().is_empty()));
    h.state_mut().session.dispatch("file.save_as", &json!({"location":dir.project().to_str().unwrap()})).unwrap();
    h.run();
    assert_eq!(nodecraft_format::decode(&std::fs::read(dir.project()).unwrap()).unwrap().project, expected);
    assert_eq!(dir.kept_files(), 0);
}

#[test]
fn native_offer_keeps_private_asset_ranges_after_its_reader_and_source_are_gone() {
    use nodecraft_format::{ArchivePlan, CheckpointPlan, NativeStorage, ProjectStorage};
    let dir = Scratch::new("native-source-lifetime");
    let path = dir.journal();
    let mut snapshot = nodecraft_format::decode(include_bytes!("../../../../format/tests/fixtures/v2/comments-v2.ncraft")).unwrap();
    let hash = *snapshot.assets.hashes().next().unwrap();
    let expected = snapshot.assets.get(&hash).unwrap().to_vec();
    {
        let mut native = NativeStorage::new(path.clone()).unwrap();
        snapshot.assets = snapshot.assets.in_context(native.asset_context().unwrap(), &Default::default(), &Default::default()).unwrap();
        let archive = ArchivePlan::prepare(&snapshot, native.capability(), &Default::default(), &Default::default()).unwrap();
        native.checkpoint_plan(&CheckpointPlan::new(archive).unwrap(), &Default::default(), &Default::default()).unwrap();
    }
    drop(snapshot);
    let offer = RecoveryOffer::read(path.clone());
    let found = offer.found.as_ref().unwrap();
    assert!(found.snapshot.assets.get(&hash).is_none());
    std::fs::remove_file(path).unwrap();
    let reference = nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() };
    let lease = found.snapshot.assets.open_verified(&reference, &Default::default(), &Default::default()).unwrap();
    assert_eq!(lease.read_small(1024, &Default::default()).unwrap(), expected);
}

/// Two unrelated crashes share the desktop's one app-global journal folder.
fn mixed_project_crashes(dir: &Scratch) -> (PathBuf, Vec<u8>, nodecraft_graph::Project) {
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_types::{CanvasId, ProjectId};
    let mut other = Project::new(ProjectId::from_bytes([99; 16]), "Public project B").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([1; 16]), "Canvas B") }.apply(&mut other).unwrap();
    let b = dir.0.join("b.ncraft");
    std::fs::write(&b, nodecraft_format::encode(&Snapshot { project: other, assets: Default::default(), epoch: 0 }).unwrap()).unwrap();
    crash(dir, true);
    let mut h = launch(dir, None);
    assert!(asked(&h));
    let a_copy = h.state().recovery.as_ref().unwrap().kept.clone();
    let a_bytes = std::fs::read(&a_copy).unwrap();
    // The control/MCP path dispatches directly against Session, even with the UI offer pending.
    h.state_mut().session.dispatch("file.open", &json!({"location":b.to_str().unwrap(),"discard_dirty":true})).unwrap();
    add_node(&mut h);
    let expected_b = h.state().session.document().clone();
    assert_eq!(expected_b.id(), ProjectId::from_bytes([99; 16]));
    drop(h); // B crashes; A's earlier kept copy has never been recovered or discarded.
    (a_copy, a_bytes, expected_b)
}

#[test]
fn resolving_project_b_recover_and_save_keeps_project_a_exact_copy() {
    let dir = Scratch::new("cross-project-save-450");
    let (a_copy, a_bytes, expected_b) = mixed_project_crashes(&dir);
    let mut h = launch(&dir, None);
    assert_eq!(h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().snapshot.project, expected_b);
    let selected = h.state().recovery.as_ref().unwrap().kept.clone();
    h.get_by_label("Recover").click();
    h.run();
    h.state_mut().session.dispatch("file.save", &json!({})).unwrap();
    h.run();
    assert!(a_copy.exists(), "A's never-recovered work must survive saving B");
    assert_eq!(std::fs::read(&a_copy).unwrap(), a_bytes);
    assert!(!selected.exists(), "the explicitly recovered and saved copy can be removed");
    h.state_mut().close_storage();
    drop(h);
    let h = launch(&dir, None);
    assert_eq!(h.state().recovery.as_ref().unwrap().kept, a_copy);
    assert_ne!(h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().snapshot.project, expected_b);
}

#[test]
fn resolving_project_b_start_last_save_keeps_project_a_exact_copy() {
    let dir = Scratch::new("cross-project-discard-450");
    let (a_copy, a_bytes, expected_b) = mixed_project_crashes(&dir);
    let mut h = launch(&dir, None);
    assert_eq!(h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().snapshot.project, expected_b);
    let selected = h.state().recovery.as_ref().unwrap().kept.clone();
    h.get_by_label("Start from last save").click();
    h.run();
    assert!(a_copy.exists(), "discarding B does not discard A");
    assert_eq!(std::fs::read(&a_copy).unwrap(), a_bytes);
    assert!(!selected.exists());
    assert_eq!(h.state().session.document().id(), expected_b.id());
    assert!(!has_added(&h), "B's saved state is opened");
}

#[test]
fn recovered_unsaved_copy_survives_programmatic_discard_open_of_same_file() {
    let dir = Scratch::new("discard-open-450");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    let copy = h.state().recovery.as_ref().unwrap().kept.clone();
    let bytes = std::fs::read(&copy).unwrap();
    h.get_by_label("Recover").click();
    h.run();
    assert!(has_added(&h));
    h.state_mut().session.dispatch("file.open", &json!({"location":dir.project().to_str().unwrap(),"discard_dirty":true})).unwrap();
    h.run();
    assert!(!has_added(&h));
    assert!(copy.exists(), "clean named state is not proof of saving recovered work");
    assert_eq!(std::fs::read(copy).unwrap(), bytes);
}

#[test]
fn recovery_modal_owns_command_wheel_zoom() {
    let dir = Scratch::new("modal-zoom-450");
    crash(&dir, false);
    let mut h = launch(&dir, None);
    let view = h.state().view;
    let doc = h.state().session.document().clone();
    h.input_mut().events.extend([
        egui::Event::PointerMoved(egui::pos2(80.0, 120.0)),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: egui::vec2(0.0, -40.0),
            modifiers: egui::Modifiers::COMMAND,
        },
    ]);
    h.step();
    assert!(asked(&h));
    assert_eq!(h.state().view, view, "derived zoom belongs to the recovery prompt");
    assert_eq!(h.state().session.document(), &doc);
}

#[test]
fn full_kept_slots_refuse_before_replacing_the_newest_live_journal() {
    let dir = Scratch::new("full-slots-450");
    crash(&dir, true);
    let live = std::fs::read(dir.journal()).unwrap();
    for n in 1..=storage::MAX_KEPT {
        std::fs::write(dir.0.join(format!("session.journal.unrecovered-{n}")), format!("old retained {n}")).unwrap();
    }
    let result = NodeCraftApp::storage_session(StorageSetup { recovery: dir.journal(), open: None });
    assert!(result.is_err(), "full retention must not silently checkpoint over the newest work");
    assert_eq!(std::fs::read(dir.journal()).unwrap(), live);
    for n in 1..=storage::MAX_KEPT {
        assert_eq!(std::fs::read_to_string(dir.0.join(format!("session.journal.unrecovered-{n}"))).unwrap(), format!("old retained {n}"));
    }
}

#[test]
fn incomplete_header_payload_and_checksum_damage_share_truthful_warning_and_prefix() {
    use nodecraft_format::journal::RecoveryDamageReason;
    for (kind, reason) in
        [Some(RecoveryDamageReason::IncompleteHeader), Some(RecoveryDamageReason::IncompletePayload), Some(RecoveryDamageReason::ChecksumMismatch), None]
            .into_iter()
            .enumerate()
    {
        let dir = Scratch::new(&format!("damage-wording-{kind}-450"));
        let mut h = launch(&dir, None);
        add_node(&mut h);
        let prefix = std::fs::metadata(dir.journal()).unwrap().len() as usize;
        let canvas = h.state().canvas;
        let second = nodecraft_types::NodeId::from_bytes([81; 16]);
        h.state_mut().session.dispatch("node.create", &json!({"canvas":canvas,"id":second,"kind":"source.text","pos":{"x":0.0,"y":400.0}})).unwrap();
        drop(h);
        let mut bytes = std::fs::read(dir.journal()).unwrap();
        match kind {
            0 => bytes.truncate(prefix + 7),
            1 => bytes.truncate(bytes.len() - 1),
            2 => *bytes.last_mut().unwrap() ^= 1,
            _ => {}
        }
        std::fs::write(dir.journal(), bytes).unwrap();
        // Read the damage before the offer pins its native context/lock.
        let replay = storage::replay_kept(&dir.journal()).unwrap();
        assert_eq!(replay.damage.map(|damage| damage.reason), reason);
        drop(replay);
        let mut h = launch(&dir, None);
        let warning = h.query_by_label("Recovery stopped at an incomplete or damaged record. Only complete changes before that point are included.");
        assert_eq!(warning.is_some(), reason.is_some());
        assert!(h.query_by_label_contains("The very last change").is_none());
        h.get_by_label("Recover").click();
        h.run();
        assert!(has_added(&h), "complete prefix survives {kind}");
        assert_eq!(h.state().session.document().canvases().values().any(|canvas| canvas.nodes().contains_key(&second)), reason.is_none());
    }
}

#[test]
#[ignore = "manual public-safe #450 evidence; requires NODECRAFT_REVIEW_EVIDENCE and NODECRAFT_450_EXPECT_RETAINED"]
fn render_recovery_safety450_evidence() {
    let folder = PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let dir = Scratch::new("damage-evidence-450");
    crash(&dir, false);
    let mut bytes = std::fs::read(dir.journal()).unwrap();
    bytes.extend_from_slice(b"partial");
    std::fs::write(dir.journal(), bytes).unwrap();
    let mut h = launch(&dir, None);
    h.render().unwrap().save(folder.join("incomplete-record.png")).unwrap();
    drop(h);
    let dir = Scratch::new("failed-open-evidence-450");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    let selected = h.state().recovery.as_ref().unwrap().kept.clone();
    std::fs::remove_file(dir.project()).unwrap();
    h.get_by_label("Start from last save").click();
    h.run();
    let retained = std::env::var("NODECRAFT_450_EXPECT_RETAINED").unwrap() == "1";
    assert_eq!(selected.exists(), retained);
    h.render().unwrap().save(folder.join("failed-last-save.png")).unwrap();
    println!("Actual failed last-save open: selected safety copy retained={retained}");
}

#[test]
fn same_uuid_and_path_divergent_copies_are_not_implicit_ancestors() {
    let dir = Scratch::new("same-uuid-fork-450");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    let older = h.state().recovery.as_ref().unwrap().kept.clone();
    let original = std::fs::read(&older).unwrap();
    let old_project = h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().snapshot.project.clone();
    h.state_mut().session.dispatch("file.open", &json!({"location":dir.project().to_str().unwrap(),"discard_dirty":true})).unwrap();
    let canvas = h.state().canvas;
    h.state_mut()
        .session
        .dispatch("node.create", &json!({"canvas":canvas,"id":nodecraft_types::NodeId::from_bytes([81;16]),"kind":"source.text","pos":{"x":0,"y":0}}))
        .unwrap();
    h.run();
    drop(h);
    let mut h = launch(&dir, None);
    let selected = h.state().recovery.as_ref().unwrap().kept.clone();
    let fork = h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().snapshot.project.clone();
    assert_eq!(fork.id(), old_project.id());
    assert_ne!(fork, old_project);
    assert_eq!(storage::recorded_location(&older), storage::recorded_location(&selected));
    h.get_by_label("Recover").click();
    h.run();
    h.state_mut().session.dispatch("file.save", &json!({})).unwrap();
    h.run();
    assert_eq!(std::fs::read(older).unwrap(), original);
    assert!(!selected.exists());
}

#[test]
fn multiple_same_uuid_open_save_calls_between_frames_do_not_save_the_recovered_document() {
    let dir = Scratch::new("many-open-save-450");
    crash(&dir, true);
    let other = dir.0.join("same-uuid-other.ncraft");
    std::fs::copy(dir.project(), &other).unwrap();
    let mut h = launch(&dir, None);
    let copy = h.state().recovery.as_ref().unwrap().kept.clone();
    let bytes = std::fs::read(&copy).unwrap();
    h.get_by_label("Recover").click();
    h.run();
    for path in [&other, &dir.project()] {
        h.state_mut().session.dispatch("file.open", &json!({"location":path.to_str().unwrap(),"discard_dirty":true})).unwrap();
        h.state_mut().session.dispatch("file.save", &json!({})).unwrap();
    }
    h.run();
    assert_eq!(std::fs::read(copy).unwrap(), bytes);
    assert!(h.state().recovered.is_none(), "detached tracking keeps the disk copy");
}

#[test]
fn recovered_replacement_and_save_as_between_frames_keeps_the_original_copy() {
    let dir = Scratch::new("recover-replacement-450");
    crash(&dir, false);
    let mut h = launch(&dir, None);
    let copy = h.state().recovery.as_ref().unwrap().kept.clone();
    let bytes = std::fs::read(&copy).unwrap();
    h.get_by_label("Recover").click();
    h.run();
    let mut replacement = Snapshot { project: h.state().session.document().clone(), assets: h.state().session.assets().clone(), epoch: 0 };
    nodecraft_graph::Change::SetProjectTitle { title: "Different adopted work".into() }.apply(&mut replacement.project).unwrap();
    h.state_mut().session.recover(replacement, None).unwrap();
    h.state_mut().session.dispatch("file.save_as", &json!({"location":dir.project().to_str().unwrap()})).unwrap();
    h.run();
    assert_eq!(std::fs::read(copy).unwrap(), bytes);
}

#[test]
fn failed_last_save_open_keeps_the_exact_selected_copy() {
    let dir = Scratch::new("failed-open-450");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    let copy = h.state().recovery.as_ref().unwrap().kept.clone();
    let bytes = std::fs::read(&copy).unwrap();
    let doc = h.state().session.document().clone();
    std::fs::remove_file(dir.project()).unwrap();
    h.get_by_label("Start from last save").click();
    h.run();
    assert_eq!(std::fs::read(copy).unwrap(), bytes);
    assert_eq!(h.state().session.document(), &doc);
    assert!(h.state().notice.as_ref().unwrap().contains("kept"));
}

#[test]
fn pending_offer_never_overwrites_an_intervening_unsaved_recovery() {
    let dir = Scratch::new("pending-adoption-450");
    crash(&dir, true);
    let mut h = launch(&dir, None);
    let copy = h.state().recovery.as_ref().unwrap().kept.clone();
    let bytes = std::fs::read(&copy).unwrap();
    let mut replacement = Snapshot { project: h.state().session.document().clone(), assets: h.state().session.assets().clone(), epoch: 0 };
    nodecraft_graph::Change::SetProjectTitle { title: "Intervening unsaved work".into() }.apply(&mut replacement.project).unwrap();
    let expected = replacement.project.clone();
    h.state_mut().session.recover(replacement, None).unwrap();
    h.get_by_label("Start from last save").click();
    h.run();
    assert_eq!(h.state().session.document(), &expected);
    assert_eq!(std::fs::read(copy).unwrap(), bytes);
}

#[test]
fn modal_navigation_inputs_and_choice_frame_do_not_leak_to_the_canvas() {
    for (kind, event) in [
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: egui::vec2(0.0, -40.0),
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: egui::vec2(0.0, -40.0),
            modifiers: egui::Modifiers::COMMAND,
        },
        egui::Event::Zoom(1.2),
        egui::Event::Key { key: egui::Key::Plus, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
    ]
    .into_iter()
    .enumerate()
    {
        let dir = Scratch::new(&format!("modal-input-{kind}-450"));
        crash(&dir, false);
        let mut h = launch(&dir, None);
        let view = h.state().view;
        let document = h.state().session.document().clone();
        h.input_mut().events.extend([egui::Event::PointerMoved(egui::pos2(80.0, 120.0)), event]);
        h.step();
        assert_eq!(h.state().view, view, "pending input {kind}");
        assert_eq!(h.state().session.document(), &document);
        // The harness queues one genuine pointer event per frame, including hover before
        // press, just as its other modal-button regressions do. Do not settle idle frames.
        h.get_by_label("Recover").click();
        h.step();
        assert!(h.state().recovery.is_none(), "choice input {kind}");
        assert_eq!(h.state().view.zoom, view.zoom);
        let settled = h.state().view;
        for _ in 0..12 {
            h.step();
            assert_eq!(h.state().view, settled, "no deferred modal wheel input {kind}");
        }
        let revision = h.state().session.revision();
        h.input_mut().events.extend([egui::Event::PointerMoved(egui::pos2(80.0, 120.0)), egui::Event::Zoom(1.1)]);
        h.step();
        assert!(h.state().view.zoom > settled.zoom, "fresh navigation resumes");
        assert_eq!(h.state().session.revision(), revision);
    }
}

#[test]
fn native_streaming_offer_above_classic_asset_cap_recovers_and_saves_without_resident_fallback() {
    use nodecraft_format::{
        ArchivePlan, AssetStore, CheckpointPlan, NativeStorage, ProjectStorage,
        asset_source::{AssetRange, MemorySource},
    };
    use nodecraft_graph::{Canvas, Change, Node, Project};
    use nodecraft_types::{AssetRef, CanvasId, NodeId, ProjectId, Value};
    use std::sync::Arc;
    let dir = Scratch::new("native-large-450");
    let cancel = Default::default();
    let metrics = Default::default();
    let length = nodecraft_format::MAX_ASSET_BYTES + 1;
    let mut native = NativeStorage::new(dir.journal()).unwrap();
    let mut assets = AssetStore::with_context(native.asset_context().unwrap());
    let source = AssetRange::new(Arc::new(MemorySource::new(Arc::from(vec![b'x'; length]))), 0, length as u64).unwrap();
    let hash = assets.import_stream(&source, &cancel, &metrics).unwrap();
    let reference = AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() };
    let mut project = Project::new(ProjectId::from_bytes([97; 16]), "Public native recovery").unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(NodeId::from_bytes([96; 16]), "unknown.public.kind");
    node.config.insert("content".into(), Value::Media(reference.clone()));
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let snapshot = Snapshot { project: project.clone(), assets, epoch: 0 };
    let archive = ArchivePlan::prepare(&snapshot, native.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive).unwrap();
    native.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let change = nodecraft_graph::Changeset::new(vec![Change::SetProjectTitle { title: "Public unsaved native recovery".into() }]);
    let admitted = nodecraft_format::journal::AdmittedReplay::new(change.clone(), &snapshot.assets).unwrap();
    let recorded = nodecraft_format::RecordedAssets::new(checkpoint.asset_lengths()).unwrap();
    let (frame, _) = admitted.into_plan(checkpoint.identity(), 1, &recorded, native.capability(), &cancel, &metrics).unwrap();
    native.append_plan(&frame, &cancel, &metrics).unwrap();
    native.flush().unwrap();
    change.apply(&mut project).unwrap();
    drop((native, snapshot, checkpoint, frame));
    let mut h = launch(&dir, None);
    assert!(asked(&h));
    assert!(h.state().recovery.as_ref().unwrap().found.as_ref().unwrap().snapshot.assets.get(&hash).is_none());
    h.get_by_label("Recover").click();
    h.run();
    assert_eq!(h.state().session.document(), &project);
    let lease = h.state().session.assets().open_verified(&reference, &cancel, &metrics).unwrap();
    assert_eq!(lease.byte_len(), length as u64);
    let mut first = [0; 16];
    std::io::Read::read_exact(&mut lease.reader(metrics.clone()), &mut first).unwrap();
    assert_eq!(first, [b'x'; 16]);
    drop(lease);
    h.state_mut().session.dispatch("file.save_as", &json!({"location":dir.project().to_str().unwrap()})).unwrap();
    h.run();
    assert_eq!(dir.kept_files(), 0);
    h.state_mut().close_storage();
    drop(h);
    let mut reopened = NativeStorage::new(dir.0.join("separate-recovery")).unwrap();
    let (loaded, _, _) = reopened.prepare_open_snapshot(&StorageLocation::Path(dir.project()), &cancel, &metrics).unwrap().into_parts();
    assert_eq!(loaded.project, project);
    assert!(loaded.assets.get(&hash).is_none());
    let lease = loaded.assets.open_verified(&reference, &cancel, &metrics).unwrap();
    assert_eq!(lease.byte_len(), length as u64);
}
