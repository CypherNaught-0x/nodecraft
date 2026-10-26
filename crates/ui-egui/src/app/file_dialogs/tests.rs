use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use std::{cell::Cell, collections::VecDeque, rc::Rc};

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nodecraft-file407-{}-{sequence}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn app(&self, journal: &str) -> NodeCraftApp {
        NodeCraftApp::with_services_and_storage(Default::default(), storage::StorageSetup { recovery: self.0.join(journal), open: None }).unwrap()
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}
// Native saves now complete on a real worker. Simulated frames must yield wall-clock time;
// the fixed deadline fails rather than hiding a stuck publication or retrying an operation.
fn settle(h: &mut Harness<'static, NodeCraftApp>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        h.step();
        if !h.state().session.file_status().saving
            && h.output().viewport_output.get(&egui::ViewportId::ROOT).unwrap().repaint_delay != std::time::Duration::ZERO
        {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "file workflow did not settle within the fixture deadline");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn add(app: &mut NodeCraftApp, n: u8) {
    let node = nodecraft_graph::Node::new(NodeId::from_bytes([n; 16]), "source.text");
    app.session.dispatch("node.add", &json!({"canvas":app.canvas,"node":node})).unwrap();
}
fn stub(app: &mut NodeCraftApp, outcomes: Vec<Option<PathBuf>>) -> Rc<Cell<usize>> {
    let calls = Rc::new(Cell::new(0));
    let count = calls.clone();
    let mut outcomes = VecDeque::from(outcomes);
    app.files.picker = Box::new(move |_| {
        count.set(count.get() + 1);
        Ok(Box::pin(std::future::ready(Ok(outcomes.pop_front().expect("unexpected picker")))))
    });
    calls
}
fn saved(dir: &Scratch, name: &str, id: u8) -> PathBuf {
    let path = dir.file(name);
    let mut app = dir.app(&format!("seed-{id}"));
    add(&mut app, id);
    app.session.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
    app.close_storage();
    path
}
#[test]
fn untitled_save_as_and_existing_save_use_real_atomic_files_and_bounded_recents() {
    let dir = Scratch::new("save");
    let first = dir.file("forest.custom");
    let second = dir.file("forest-copy.ncraft");
    let mut app = dir.app("journal");
    add(&mut app, 41);
    let calls = stub(&mut app, vec![Some(first.clone()), Some(second.clone())]);
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h);
    assert!(first.is_file());
    assert_eq!(calls.get(), 1);
    assert!(!h.state().session.file_status().dirty);
    add(h.state_mut(), 42);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h);
    assert_eq!(calls.get(), 1);
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S);
    settle(&mut h);
    assert_eq!(calls.get(), 2);
    assert!(second.is_file());
    assert_eq!(h.state().files.projects.recent(), vec![second.to_str().unwrap(), first.to_str().unwrap()]);
    let expected = h.state().session.document().clone();
    h.state_mut().session.dispatch("file.open", &json!({"location":second.to_str().unwrap(),"discard_dirty":true})).unwrap();
    assert_eq!(h.state().session.document(), &expected);
}
#[test]
fn clean_open_palette_and_cancel_use_only_ui_picker_boundary() {
    let dir = Scratch::new("open");
    let file = saved(&dir, "open.ncraft", 43);
    let mut app = dir.app("journal");
    let calls = stub(&mut app, vec![None, Some(file.clone())]);
    app.session.dispatch("file.save_as", &json!({"location":dir.file("initial.ncraft").to_str().unwrap()})).unwrap();
    let expected = app.session.document().clone();
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    settle(&mut h);
    assert_eq!(h.state().session.document(), &expected);
    assert!(h.state().notice.is_none());
    // The same UI dispatch used by a chosen palette entry obtains a location.
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    settle(&mut h);
    assert_eq!(calls.get(), 2);
    assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(file)));
    assert!(!h.state().session.file_status().dirty);
    let count = calls.get();
    h.state_mut().session.dispatch("file.save", &json!({})).unwrap();
    assert_eq!(calls.get(), count, "direct Session calls never see picker");
}
#[test]
fn dirty_cancel_discard_and_save_before_open_preserve_exact_work() {
    for choice in ["Cancel", "Discard", "Save"] {
        let dir = Scratch::new(choice);
        let target = saved(&dir, "target.ncraft", 44);
        let source = dir.file("source.ncraft");
        let mut app = dir.app("journal");
        add(&mut app, 45);
        let expected = app.session.document().clone();
        let calls = stub(&mut app, vec![Some(target.clone()), Some(source.clone())]);
        let mut h = harness(app);
        settle(&mut h);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        settle(&mut h);
        h.get_by_label("Save changes before opening?");
        h.get_by_label(choice).click();
        settle(&mut h);
        if choice == "Cancel" {
            assert_eq!(h.state().session.document(), &expected);
            assert!(!source.exists());
        } else {
            assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(target)));
            assert!(!h.state().session.document().canvases().values().any(|c| c.nodes().contains_key(&NodeId::from_bytes([45; 16]))));
        }
        if choice == "Save" {
            assert_eq!(nodecraft_format::decode(&std::fs::read(source).unwrap()).unwrap().project, expected);
            assert_eq!(calls.get(), 2);
        } else {
            assert_eq!(calls.get(), 1);
        }
    }
}
#[test]
fn cancelled_or_failed_save_never_continues_the_dirty_open() {
    for bad in [None, Some(PathBuf::from("relative-refused.ncraft"))] {
        let dir = Scratch::new(if bad.is_none() { "cancel-save" } else { "failed-save" });
        let target = saved(&dir, "target.ncraft", 46);
        let mut app = dir.app("journal");
        add(&mut app, 47);
        let expected = app.session.document().clone();
        stub(&mut app, vec![Some(target), bad.clone()]);
        let mut h = harness(app);
        settle(&mut h);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        settle(&mut h);
        h.get_by_label("Save").click();
        settle(&mut h);
        assert_eq!(h.state().session.document(), &expected);
        assert!(h.state().session.file_status().dirty);
        assert_eq!(h.state().notice.is_some(), bad.is_some());
    }
}
#[test]
fn late_picker_after_edits_or_replacement_and_duplicate_requests_never_dispatch() {
    for replace in [false, true] {
        let dir = Scratch::new(if replace { "replaced" } else { "edited" });
        let target = saved(&dir, "target.ncraft", 48);
        let mut app = dir.app("journal");
        let ready = Rc::new(Cell::new(false));
        let gate = ready.clone();
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        app.files.picker = Box::new(move |_| {
            count.set(count.get() + 1);
            let gate = gate.clone();
            let path = target.clone();
            Ok(Box::pin(std::future::poll_fn(move |_| if gate.get() { Poll::Ready(Ok(Some(path.clone()))) } else { Poll::Pending })))
        });
        let mut h = harness(app);
        settle(&mut h);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        h.step();
        let ctx = h.ctx.clone();
        h.state_mut().begin_file("file.save_as", &ctx);
        assert_eq!(calls.get(), 1);
        if replace {
            let next = dir.app("other-journal");
            h.state_mut().session = next.session;
        } else {
            add(h.state_mut(), 49);
        }
        let expected = h.state().session.document().clone();
        ready.set(true);
        h.step();
        assert_eq!(h.state().session.document(), &expected);
        assert!(h.state().notice.as_deref().unwrap().contains("changed"));
        assert!(h.state().files.pending.is_none());
    }
}
#[test]
fn paths_are_bounded_and_devices_missing_files_and_dangling_aliases_refuse() {
    let dir = Scratch::new("paths");
    assert!(admitted(&dir.0, false).is_err());
    assert!(admitted(&dir.file("missing"), false).is_err());
    assert!(admitted(&dir.file("new.ncraft"), true).is_ok());
    assert!(admitted(Path::new("relative"), true).is_err());
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;
        let alias = dir.file("dangling");
        symlink(dir.file("missing"), &alias).unwrap();
        assert!(admitted(&alias, true).is_err());
        assert!(admitted(&PathBuf::from(OsString::from_vec(vec![b'/', 255])), true).is_err());
    }
}

#[test]
fn actual_palette_choice_opens_the_picker_and_public_command_calls_do_not() {
    let dir = Scratch::new("palette");
    let target = saved(&dir, "chosen.ncraft", 50);
    let mut app = dir.app("journal");
    app.session.dispatch("file.save_as", &json!({"location":dir.file("start.ncraft").to_str().unwrap()})).unwrap();
    let calls = stub(&mut app, vec![Some(target.clone())]);
    let mut h = harness(app);
    settle(&mut h);
    h.state_mut().view.zoom = 0.1;
    h.step();
    let at = egui::pos2(150.0, 600.0);
    h.hover_at(at);
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
    }
    assert!(h.state().palette.is_some(), "raw double-click opens actual palette");
    h.event(egui::Event::Text("file.open".into()));
    settle(&mut h);
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(calls.get(), 1);
    assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(target)));
    assert!(h.state_mut().session.dispatch("file.open", &json!({})).is_err());
    assert_eq!(calls.get(), 1);
}
#[test]
fn minimap_held_and_release_owned_state_refuses_file_shortcuts() {
    let dir = Scratch::new("held-map");
    let mut app = dir.app("journal");
    let calls = stub(&mut app, vec![]);
    let mut h = harness(app);
    settle(&mut h);
    let at = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map.center();
    h.event(egui::Event::PointerMoved(at));
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().navigation.pointer_owned);
    for release in [false, true] {
        if release {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::O,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        h.step();
        assert_eq!(calls.get(), 0, "held/release={release}");
    }
}
#[test]
fn dirty_confirmation_invalidates_before_discard_after_a_new_edit() {
    let dir = Scratch::new("dirty-stale");
    let target = saved(&dir, "target.ncraft", 51);
    let mut app = dir.app("journal");
    add(&mut app, 52);
    stub(&mut app, vec![Some(target)]);
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    settle(&mut h);
    h.get_by_label("Save changes before opening?");
    add(h.state_mut(), 53);
    let expected = h.state().session.document().clone();
    h.step();
    assert_eq!(h.state().session.document(), &expected);
    assert!(h.state().files.pending.is_none());
    assert!(h.state().notice.as_deref().unwrap().contains("changed"));
}
#[test]
fn missing_recent_is_retained_until_explicit_menu_removal() {
    let dir = Scratch::new("recent-menu");
    let missing = dir.file("missing-public.ncraft");
    let mut app = dir.app("journal");
    app.files.projects.remember(missing.to_str().unwrap());
    let mut h = harness(app);
    settle(&mut h);
    h.get_by_label("File").click();
    settle(&mut h);
    h.get_by_label("missing-public.ncraft").click();
    settle(&mut h);
    assert!(h.state().notice.is_some());
    assert_eq!(h.state().files.projects.recent().len(), 1);
    h.get_by_label("File").click();
    settle(&mut h);
    h.get_by_label("Remove").click();
    settle(&mut h);
    assert!(h.state().files.projects.recent().is_empty());
}

#[test]
fn actual_native_save_error_keeps_dirty_work_and_does_not_open_target() {
    let dir = Scratch::new("native-save-error");
    let target = saved(&dir, "target.ncraft", 54);
    let parent = dir.file("gone-parent");
    std::fs::create_dir(&parent).unwrap();
    let source = parent.join("source.ncraft");
    let mut app = dir.app("journal");
    app.session.dispatch("file.save_as", &json!({"location":source.to_str().unwrap()})).unwrap();
    add(&mut app, 55);
    let expected = app.session.document().clone();
    stub(&mut app, vec![Some(target)]);
    std::fs::remove_file(&source).unwrap();
    std::fs::remove_dir(parent).unwrap();
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    assert_eq!(h.state().session.document(), &expected);
    assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(source)));
    assert!(h.state().session.file_status().dirty);
    assert!(h.state().notice.is_some());
}
#[test]
fn read_only_settings_never_turn_successful_save_into_failed_or_claim_persistence() {
    use nodecraft_engine::settings::{Location, open};
    let dir = Scratch::new("readonly-settings");
    let config = dir.file("config");
    std::fs::create_dir(&config).unwrap();
    let text = "schema_version=99\n[appearance]\ntheme=\"light\"\n";
    std::fs::write(config.join("config.toml"), text).unwrap();
    let (store, _) = open(Location::Dir(config.clone()));
    assert!(store.is_read_only());
    let mut app = dir.app("journal");
    app.store = Some(store);
    let target = dir.file("public.ncraft");
    stub(&mut app, vec![Some(target.clone())]);
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h);
    assert!(target.is_file());
    assert!(!h.state().session.file_status().dirty);
    assert!(h.state().notice.as_deref().unwrap().contains("could not be saved"));
    assert_eq!(std::fs::read_to_string(config.join("config.toml")).unwrap(), text);
}
#[test]
fn recovery_modal_owns_file_shortcuts_and_cannot_launch_a_picker() {
    let dir = Scratch::new("recovery-owner");
    {
        let mut app = dir.app("journal");
        add(&mut app, 56);
    }
    let (session, canvas, kept, _) = NodeCraftApp::storage_session(storage::StorageSetup { recovery: dir.file("journal"), open: None }).unwrap();
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    app.attach_recovery(dir.file("journal"), kept);
    let calls = stub(&mut app, vec![]);
    let mut h = harness(app);
    settle(&mut h);
    h.get_by_label("Recover unsaved work?");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    settle(&mut h);
    assert_eq!(calls.get(), 0);
    assert!(h.state().files.pending.is_none());
}
#[test]
fn uncommitted_text_draft_is_not_silently_saved_or_replaced() {
    let dir = Scratch::new("draft-owner");
    let mut app = dir.app("journal");
    let calls = stub(&mut app, vec![]);
    let mut h = harness(app);
    settle(&mut h);
    let expected = h.state().session.document().clone();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Text").click();
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(" public uncommitted draft".into()));
    settle(&mut h);
    assert!(h.state().editors.values().any(EditorState::has_uncommitted));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h);
    assert_eq!(calls.get(), 0);
    assert_eq!(h.state().session.document(), &expected);
    assert!(h.state().notice.as_deref().unwrap().contains("field edits"));
}

#[test]
fn file_panel_owns_canvas_keys_and_derived_zoom() {
    let dir = Scratch::new("panel-input");
    let mut app = dir.app("journal");
    add(&mut app, 57);
    let mut h = harness(app);
    settle(&mut h);
    h.get_by_label("File").click();
    settle(&mut h);
    assert!(h.state().files.menu_open);
    let expected = h.state().session.document().clone();
    let view = h.state().view;
    h.input_mut().events.extend([
        egui::Event::Zoom(1.2),
        egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
        egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
    ]);
    h.step();
    assert_eq!(h.state().session.document(), &expected);
    assert_eq!(h.state().view, view);
}

#[test]
fn picker_errors_and_panics_leave_the_document_unchanged_and_release_the_slot() {
    for kind in 0..3 {
        let dir = Scratch::new(&format!("picker-error-{kind}"));
        let mut app = dir.app("journal");
        let original = app.session.document().clone();
        app.files.picker = Box::new(move |_| match kind {
            0 => Err("Fixture native picker refused".into()),
            1 => Ok(Box::pin(std::future::ready(Err("Fixture native picker failed".into())))),
            _ => Ok(Box::pin(std::future::poll_fn(|_| panic!("fixture picker panic")))),
        });
        let mut h = harness(app);
        settle(&mut h);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
        settle(&mut h);
        assert_eq!(h.state().session.document(), &original);
        assert!(h.state().files.pending.is_none());
        assert!(h.state().notice.is_some());
        let target = dir.file("after-error.ncraft");
        stub(h.state_mut(), vec![Some(target.clone())]);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
        settle(&mut h);
        assert!(target.is_file());
    }
}
#[test]
fn picker_path_is_admitted_before_the_host_makes_an_owned_copy() {
    let huge = format!("/{}", "é".repeat(2048));
    assert!(selected_path(Path::new(&huge)).is_err());
    assert!(selected_path(Path::new("relative")).is_err());
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        assert!(selected_path(&PathBuf::from(OsString::from_vec(vec![b'/', 255]))).is_err());
    }
}
#[test]
#[ignore = "manual public-safe #407 evidence; injected OS picker, not native OS UI proof"]
fn render_file407_workflow_evidence() {
    let output = PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let dir = Scratch::new("public-evidence");
    let target = saved(&dir, "Public-forest.ncraft", 58);
    let mut app = dir.app("journal");
    add(&mut app, 59);
    stub(&mut app, vec![Some(target)]);
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    settle(&mut h);
    h.get_by_label("Save changes before opening?");
    h.render().unwrap().save(output.join("dirty-confirmation.png")).unwrap();
    h.get_by_label("Discard").click();
    settle(&mut h);
    h.get_by_label("File").click();
    settle(&mut h);
    h.get_by_label("Public-forest.ncraft");
    h.render().unwrap().save(output.join("recent-projects.png")).unwrap();
}

#[test]
fn file_chrome_is_separate_from_comments_at_narrow_and_regular_widths() {
    for width in [360.0, 1000.0] {
        let dir = Scratch::new(&format!("chrome-{width}"));
        let mut h = Harness::builder()
            .with_step_dt(1.0 / 60.0)
            .with_max_steps(120)
            .with_size(Vec2::new(width, 700.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), dir.app("journal"));
        settle(&mut h);
        let file = h.get_by_label("File").rect();
        assert!(!file.intersects(h.get_by_label("Comments").rect()));
        h.get_by_label("File").click();
        settle(&mut h);
        assert!(!file.intersects(h.get_by_label("Open project…").rect()));
        assert!(h.ctx.content_rect().contains_rect(h.get_by_label("Open project…").rect()));
    }
}

#[test]
fn correction407_supported_replacement_keeps_picker_slot_and_recents() {
    struct Dropped(Rc<Cell<usize>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let dir = Scratch::new("replacement407");
    let target = saved(&dir, "stale.ncraft", 61);
    let mut app = dir.app("journal");
    app.files.projects.remember(target.to_str().unwrap());
    let ready = Rc::new(Cell::new(false));
    let gate = ready.clone();
    let dropped = Rc::new(Cell::new(0));
    let drops = dropped.clone();
    let calls = Rc::new(Cell::new(0));
    let count = calls.clone();
    app.files.picker = Box::new(move |_| {
        count.set(count.get() + 1);
        let gate = gate.clone();
        let path = target.clone();
        let token = Dropped(drops.clone());
        Ok(Box::pin(std::future::poll_fn(move |_| {
            let _keep = &token;
            if gate.get() { Poll::Ready(Ok(Some(path.clone()))) } else { Poll::Pending }
        })))
    });
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    h.step();
    assert_eq!(calls.get(), 1);
    let (session, canvas, _, _) = NodeCraftApp::storage_session(storage::StorageSetup { recovery: dir.file("replacement"), open: None }).unwrap();
    h.state_mut().replace_session(session, canvas).unwrap();
    assert_eq!(dropped.get(), 0, "native operation must retain its occupied future");
    assert_eq!(h.state().files.projects.recent().len(), 1);
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    assert_eq!(calls.get(), 1);
    let expected = h.state().session.document().clone();
    ready.set(true);
    settle(&mut h);
    assert_eq!(h.state().session.document(), &expected);
    assert!(h.state().notice.as_deref().unwrap().contains("changed"));
    assert_eq!(dropped.get(), 1);
    assert!(h.state().files.pending.is_none());
    h.state_mut().begin_file("file.open", &ctx);
    assert_eq!(calls.get(), 2);
}
#[test]
fn correction407_file_panel_blocks_authoritative_navigation() {
    let dir = Scratch::new("nav-query407");
    let mut h = harness(dir.app("journal"));
    settle(&mut h);
    let ctx = h.ctx.clone();
    assert!(h.state_mut().navigation_enabled(&ctx, "canvas.minimap_toggle", &json!({})).is_ok());
    h.get_by_label("File").click();
    settle(&mut h);
    assert!(h.state_mut().navigation_enabled(&ctx, "canvas.minimap_toggle", &json!({})).is_err());
    assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.minimap_toggle", &json!({})).is_err());
}
#[test]
fn correction407_file_panel_blocks_actual_navigation_pointer() {
    let dir = Scratch::new("nav-pointer407");
    let mut h = harness(dir.app("journal"));
    settle(&mut h);
    h.get_by_label("File").click();
    settle(&mut h);
    assert!(h.state().files.menu_open);
    let view = h.state().view;
    let visible = h.state().navigation.visible;
    for label in ["Fit all", "100%", "Hide map"] {
        h.get_by_label(label).click();
        settle(&mut h);
        assert_eq!(h.state().navigation.visible, visible);
        assert_eq!(h.state().view, view);
    }
    let point = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map.center();
    for pressed in [true, false] {
        h.hover_at(point);
        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
        assert_eq!(h.state().view, view);
        assert!(!h.state().navigation.pointer_owned);
    }
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!h.state().files.menu_open);
    let ctx = h.ctx.clone();
    assert!(h.state_mut().navigation_enabled(&ctx, "canvas.minimap_toggle", &json!({})).is_ok());
    h.get_by_label("Hide map").click();
    settle(&mut h);
    assert!(!h.state().navigation.visible);
}

#[test]
fn correction407_pending_completion_and_choice_close_keep_navigation_owned_through_frame() {
    for confirm in [false, true] {
        let dir = Scratch::new(if confirm { "confirm-frame407" } else { "completion-frame407" });
        let mut app = dir.app("journal");
        let gate = Rc::new(Cell::new(false));
        let ready = gate.clone();
        if confirm {
            add(&mut app, 62);
            let target = saved(&dir, "choice.ncraft", 63);
            stub(&mut app, vec![Some(target)]);
        } else {
            app.files.picker = Box::new(move |_| {
                let gate = ready.clone();
                Ok(Box::pin(std::future::poll_fn(move |_| if gate.get() { Poll::Ready(Ok(None)) } else { Poll::Pending })))
            });
        }
        let transitions = Rc::new(Cell::new(0));
        let seen = transitions.clone();
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_step_dt(1.0 / 60.0).with_max_steps(120).build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                let owned_before = app.files.pending.is_some() || app.files.menu_open;
                app.show(ui);
                if owned_before && app.files.pending.is_none() && !app.files.menu_open {
                    assert!(app.file_dialog_frame(ui.ctx()), "a later pass of the same frame keeps canvas ownership");
                    assert!(app.navigation_enabled(ui.ctx(), "canvas.minimap_toggle", &json!({})).is_err());
                    assert!(app.dispatch_navigation(ui.ctx(), "canvas.minimap_toggle", &json!({})).is_err());
                    seen.set(seen.get() + 1);
                }
            },
            app,
        );
        settle(&mut h);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        settle(&mut h);
        assert!(h.state().files.pending.is_some());
        let ctx = h.ctx.clone();
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.minimap_toggle", &json!({})).is_err());
        let visible = h.state().navigation.visible;
        let view = h.state().view;
        if confirm {
            h.get_by_label("Cancel").click();
            settle(&mut h);
        } else {
            gate.set(true);
            let pos = h.get_by_label("Hide map").rect().center();
            h.input_mut().events.extend([
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
            ]);
            h.step();
        }
        assert!(transitions.get() > 0);
        assert_eq!(h.state().navigation.visible, visible);
        assert_eq!(h.state().view, view);
        settle(&mut h);
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.minimap_toggle", &json!({})).is_ok());
    }
}

#[test]
fn file_ownership_preserves_every_cards_canvas_origin_and_actual_hit_rect() {
    let dir = Scratch::new("geometry-gate407");
    let mut app = dir.app("journal");
    for (id, x, y) in [(64_u8, 100.0, 0.0), (65, 100.0, 200.0), (66, -250.0, 200.0)] {
        app.session.dispatch("node.create", &json!({"canvas":app.canvas,"id":NodeId::from_bytes([id;16]),"kind":"source.text","pos":{"x":x,"y":y}})).unwrap();
        app.session.dispatch("node.title", &json!({"canvas":app.canvas,"id":NodeId::from_bytes([id;16]),"title":format!("Card {id}")})).unwrap();
    }
    app.files.picker = Box::new(|_| Ok(Box::pin(std::future::pending())));
    let mut h = harness(app);
    settle(&mut h);
    let view = h.state().view;
    let check = |h: &Harness<'_, NodeCraftApp>| {
        assert_eq!(h.state().view, view);
        for id in [64_u8, 65, 66] {
            let expected = h.state().view.rect_to_screen(h.state().viewport.min, h.state().painted[&NodeId::from_bytes([id; 16])]);
            let actual = h.get_by_label(&format!("Node Card {id}")).rect();
            assert!(
                (actual.min - expected.min).length() < 0.01 && (actual.max - expected.max).length() < 0.01,
                "card {id}: actual {actual:?}, expected {expected:?}"
            );
        }
    };
    check(&h);
    h.get_by_label("File").click();
    settle(&mut h);
    assert!(h.state().files.menu_open);
    check(&h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    check(&h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    settle(&mut h);
    assert!(h.state().files.pending.is_some());
    check(&h);
}

#[derive(Default)]
struct HeldSave(std::sync::Mutex<Option<nodecraft_engine::save_runner::SaveJob>>);
impl nodecraft_engine::save_runner::SaveRunner for HeldSave {
    fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
        let mut slot = self.0.lock().unwrap();
        assert!(slot.is_none());
        *slot = Some(job);
        Ok(())
    }
}
impl HeldSave {
    fn run(&self) {
        self.0.lock().unwrap().take().unwrap().run();
    }
}
#[test]
fn native_save_returns_before_disk_work_and_blocks_duplicate_file_actions() {
    let dir = Scratch::new("background-save");
    let mut app = dir.app("journal");
    let path = dir.file("source.ncraft");
    app.session.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
    add(&mut app, 81);
    let expected = app.session.document().clone();
    let runner = Arc::new(HeldSave::default());
    app.session.set_save_runner(runner.clone());
    let calls = stub(&mut app, vec![]);
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    h.step();
    assert!(h.state().session.file_status().saving, "the event frame must not execute the held save");
    assert!(runner.0.lock().unwrap().is_some());
    assert!(h.state().files.owns_input());
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    assert_eq!(calls.get(), 0);
    assert_eq!(h.state().session.document(), &expected);
    runner.run();
    h.step();
    settle(&mut h);
    assert!(!h.state().files.owns_input());
    assert!(!h.state().session.file_status().dirty);
    assert_eq!(nodecraft_format::decode(&std::fs::read(path).unwrap()).unwrap().project, expected);
}

#[test]
fn selected_save_as_waits_for_publication_before_recents_and_open_or_failure() {
    for fail in [false, true] {
        let dir = Scratch::new(if fail { "async-fail" } else { "async-open" });
        let target = saved(&dir, "target.ncraft", 82);
        let parent = dir.file("save-parent");
        std::fs::create_dir(&parent).unwrap();
        let source = parent.join("source.ncraft");
        let mut app = dir.app("journal");
        add(&mut app, 83);
        let expected = app.session.document().clone();
        let runner = Arc::new(HeldSave::default());
        app.session.set_save_runner(runner.clone());
        let calls = stub(&mut app, vec![Some(source.clone())]);
        let mut h = harness(app);
        settle(&mut h);
        let stamp = Stamp::capture(h.state());
        h.state_mut().save_then_open(target.clone(), stamp);
        h.step();
        assert!(h.state().session.file_status().saving);
        assert_eq!(h.state().session.document(), &expected);
        assert!(h.state().files.projects.recent().is_empty());
        assert!(!source.exists());
        let ctx = h.ctx.clone();
        h.state_mut().begin_file("file.save_as", &ctx);
        assert_eq!(calls.get(), 1);
        if fail {
            std::fs::remove_dir(&parent).unwrap();
        }
        runner.run();
        h.step();
        settle(&mut h);
        assert!(!h.state().files.owns_input());
        if fail {
            assert_eq!(h.state().session.document(), &expected);
            assert!(h.state().session.file_status().dirty);
            assert!(h.state().files.projects.recent().is_empty());
            assert!(h.state().notice.is_some());
        } else {
            assert_eq!(nodecraft_format::decode(&std::fs::read(&source).unwrap()).unwrap().project, expected);
            assert_eq!(h.state().session.file_status().location, Some(nodecraft_format::StorageLocation::Path(target.clone())));
            assert_eq!(h.state().files.projects.recent(), vec![target.to_str().unwrap(), source.to_str().unwrap()]);
        }
    }
}
#[test]
fn completed_save_cannot_open_after_replacement_and_quit_never_runs_continuation() {
    for quit in [false, true] {
        let dir = Scratch::new(if quit { "async-quit" } else { "async-stale" });
        let target = saved(&dir, "target.ncraft", 84);
        let source = dir.file("source.ncraft");
        let mut app = dir.app("journal");
        app.session.dispatch("file.save_as", &json!({"location":source.to_str().unwrap()})).unwrap();
        add(&mut app, 85);
        let expected = app.session.document().clone();
        let runner = Arc::new(HeldSave::default());
        app.session.set_save_runner(runner.clone());
        let stamp = Stamp::capture(&app);
        app.save_then_open(target, stamp);
        assert!(app.session.file_status().saving);
        runner.run();
        if quit {
            app.close_storage();
            assert_eq!(app.session.document(), &expected);
            assert_eq!(app.session.file_status().location, Some(nodecraft_format::StorageLocation::Path(source)));
            assert!(match std::fs::metadata(dir.file("journal")) {
                Ok(metadata) => metadata.len() == 0,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            });
        } else {
            // A host replacement between frames must not receive the old save's Open.
            let replacement = dir.app("replacement");
            app.session = replacement.session;
            let replaced = app.session.document().clone();
            let mut h = harness(app);
            settle(&mut h);
            assert_eq!(h.state().session.document(), &replaced);
            assert!(!h.state().files.owns_input());
            assert!(h.state().files.projects.recent().is_empty());
        }
    }
}
#[test]
fn inline_save_runner_fallback_completes_without_leaving_a_pending_file_owner() {
    struct Inline;
    impl nodecraft_engine::save_runner::SaveRunner for Inline {
        fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
            Err(job)
        }
    }
    let dir = Scratch::new("inline-save");
    let mut app = dir.app("journal");
    let path = dir.file("inline.ncraft");
    app.session.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
    add(&mut app, 86);
    app.session.set_save_runner(Arc::new(Inline));
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h);
    assert!(!h.state().session.file_status().dirty);
    assert!(!h.state().files.owns_input());
}

fn model_owner_app(dir: &Scratch) -> (NodeCraftApp, NodeId) {
    let mut app = dir.app("model-owner-journal");
    let id = NodeId::from_bytes([157; 16]);
    let params = [("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("Circle".into()))].into();
    let node = app.session.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &params).unwrap();
    app.session.dispatch("node.add", &json!({"canvas":app.canvas,"node":node})).unwrap();
    (app, id)
}

#[test]
fn model_and_file_pickers_cannot_acquire_a_second_owner() {
    let dir = Scratch::new("model-file-owner472");
    let (mut app, id) = model_owner_app(&dir);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    app.files.picker = Box::new(move |_| {
        observed.set(observed.get() + 1);
        Ok(Box::pin(std::future::pending()))
    });
    let mut h = harness(app);
    settle(&mut h);
    h.get_by_label("File").click();
    settle(&mut h);
    let revision = h.state().session.revision();
    let canvas = h.state().canvas;
    assert!(h.state_mut().open_model_picker(canvas, vec![id], revision).is_err(), "the actual File menu owns model opening");
    assert!(h.state().model_choice.is_none());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    h.state_mut().open_model_picker(canvas, vec![id], revision).unwrap();
    settle(&mut h);
    assert!(h.get_by_label("File").accesskit_node().is_disabled());
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    assert_eq!(calls.get(), 0, "model ownership cannot create an OS-picker future");
    assert!(h.state().files.pending.is_none());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    h.state_mut().begin_file("file.open", &ctx);
    assert_eq!(calls.get(), 1, "the finished model owner permits a later File operation");
    assert!(h.state().files.pending.is_some());
    assert!(h.state_mut().open_model_picker(canvas, vec![id], revision).is_err(), "a parked File future retains its slot");
    assert!(h.state().model_choice.is_none());
}

#[test]
fn file_completion_keeps_model_opening_refused_until_the_next_frame() {
    let dir = Scratch::new("model-file-completion472");
    let (mut app, id) = model_owner_app(&dir);
    app.files.picker = Box::new(|_| Ok(Box::pin(std::future::ready(Ok(None)))));
    let attempted = Rc::new(Cell::new(false));
    let observed = attempted.clone();
    let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(120).build_ui_state(
        move |ui, app: &mut NodeCraftApp| {
            let was_pending = app.files.pending.is_some();
            app.show(ui);
            if was_pending && app.files.pending.is_none() && !observed.replace(true) {
                assert!(app.open_model_picker(app.canvas, vec![id], app.session.revision()).is_err(), "completion still owns the current frame");
                assert!(app.model_choice.is_none());
            }
        },
        app,
    );
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    settle(&mut h);
    assert!(attempted.get());
    let canvas = h.state().canvas;
    let revision = h.state().session.revision();
    h.state_mut().open_model_picker(canvas, vec![id], revision).unwrap();
    assert!(h.state().model_choice.is_some());
}

fn bulk_file_app(dir: &Scratch) -> NodeCraftApp {
    let mut app = dir.app("bulk-file-journal");
    let mut ids = std::collections::BTreeSet::new();
    for (byte, text) in [(71_u8, "First retained text"), (72, "Second retained text")] {
        let id = NodeId::from_bytes([byte; 16]);
        let mut node = app.session.kinds().get("source.text").unwrap().descriptor().instantiate(id, &Default::default()).unwrap();
        node.params.insert("text".into(), Value::Text(text.into()));
        app.session.dispatch("node.add", &json!({"canvas":app.canvas,"node":node})).unwrap();
        ids.insert(id);
    }
    app.selection.replace(ids);
    app
}

#[test]
fn bulk_file_menu_and_parked_picker_disable_actual_bulk_reset_controls() {
    let dir = Scratch::new("bulk-file-owner473");
    let mut app = bulk_file_app(&dir);
    app.files.picker = Box::new(|_| Ok(Box::pin(std::future::pending())));
    let mut h = harness(app);
    settle(&mut h);
    let before = h.state().session.document().clone();
    h.get_by_label("File").click();
    settle(&mut h);
    let reset = h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").get_by_label("Reset all to default");
    assert!(reset.accesskit_node().is_disabled(), "the File panel owns bulk controls");
    reset.click();
    settle(&mut h);
    assert_eq!(h.state().session.document(), &before);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    settle(&mut h);
    assert!(h.state().files.pending.is_some());
    let reset = h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").get_by_label("Reset all to default");
    assert!(reset.accesskit_node().is_disabled(), "a parked File future owns bulk controls");
    reset.click();
    settle(&mut h);
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn dirty_bulk_draft_refuses_file_factory_and_remains_recoverable() {
    let dir = Scratch::new("bulk-file-draft473");
    let mut app = bulk_file_app(&dir);
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    app.files.picker = Box::new(move |_| {
        observed.set(observed.get() + 1);
        Ok(Box::pin(std::future::pending()))
    });
    let mut h = harness(app);
    settle(&mut h);
    let before = h.state().session.document().clone();
    h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").get_by_label("Edit for all…").click();
    settle(&mut h);
    h.event(egui::Event::Text("Recover this shared draft".into()));
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    assert_eq!(calls.get(), 0, "a bulk draft must not be replaced by file acquisition");
    assert!(h.state().files.pending.is_none());
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Text").value().as_deref(), Some("Recover this shared draft"));
}

#[test]
fn file_completion_frame_cannot_publish_a_cached_bulk_reset_click() {
    let dir = Scratch::new("bulk-file-completion473");
    let mut app = bulk_file_app(&dir);
    app.files.picker = Box::new(|_| Ok(Box::pin(std::future::ready(Ok(None)))));
    let mut h = harness(app);
    settle(&mut h);
    let before = h.state().session.document().clone();
    let reset = h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").get_by_label("Reset all to default").rect().center();
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    assert!(h.state().files.pending.is_some());
    h.input_mut().events.extend([
        egui::Event::PointerMoved(reset),
        egui::Event::PointerButton { pos: reset, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
        egui::Event::PointerButton { pos: reset, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
    ]);
    h.step();
    assert!(h.state().files.pending.is_none());
    assert_eq!(h.state().session.document(), &before, "completion owns the cached pointer click");
    assert!(h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").get_by_label("Reset all to default").accesskit_node().is_disabled());
    settle(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::Window, "Shared parameters").get_by_label("Reset all to default").click();
    settle(&mut h);
    assert_ne!(h.state().session.document(), &before, "a later unowned explicit click remains usable");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn storage968_actual_journal_conflict_retains_startup_reason_and_focus_action() {
    let dir = Scratch::new("storage968-lock");
    let owner = dir.app("journal");
    let setup = storage::StorageSetup { recovery: dir.file("journal"), open: None };
    let (mut second, _, owns) = NodeCraftApp::from_storage_setup(Default::default(), Some(setup), "unused").unwrap();
    assert!(!owns);
    assert!(second.storage_owner_blocked());
    let startup = second.notice.clone().unwrap();
    assert!(startup.contains("recovery location is already in use"));
    assert!(startup.contains("Another NodeCraft window"));
    let focused = Rc::new(Cell::new(0));
    let count = focused.clone();
    second
        .session
        .register_host_service(nodecraft_engine::storage_window_cmds::SERVICE, move |_| {
            count.set(count.get() + 1);
            Ok(json!({"requested":true}))
        })
        .unwrap();
    let calls = stub(&mut second, vec![]);
    let mut h = harness(second);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().notice = None;
    h.state_mut().begin_file("file.save", &ctx);
    assert_eq!(h.state().notice.as_deref(), Some(startup.as_str()));
    settle(&mut h);
    h.get_by_label("Focus the other window").click();
    settle(&mut h);
    assert_eq!(focused.get(), 1);
    assert_eq!(calls.get(), 0);
    assert!(owner.session.file_status().available);
    assert!(!h.state().session.file_status().available);
}

#[test]
fn storage968_unreadable_project_still_has_storage_and_picker() {
    let dir = Scratch::new("storage968-unreadable");
    let path = dir.file("broken.ncraft");
    std::fs::write(&path, b"not a NodeCraft project").unwrap();
    let setup = storage::StorageSetup { recovery: dir.file("journal"), open: Some(path) };
    let (mut app, _, owns) = NodeCraftApp::from_storage_setup(Default::default(), Some(setup), "unused").unwrap();
    assert!(owns);
    assert!(app.session.file_status().available);
    assert!(!app.storage_owner_blocked());
    assert!(app.notice.as_deref().unwrap().starts_with("That project couldn't be opened:"));
    assert!(app.notice.as_deref().unwrap().contains("can still be saved"));
    let calls = stub(&mut app, vec![None]);
    let mut h = harness(app);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().begin_file("file.open", &ctx);
    settle(&mut h);
    assert_eq!(calls.get(), 1);
}

#[test]
fn storage968_storage_failure_missing_configuration_and_closed_recovery_have_distinct_reasons() {
    let dir = Scratch::new("storage968-failures");
    let folder = dir.file("not-a-folder");
    std::fs::write(&folder, b"occupied").unwrap();
    let setup = storage::StorageSetup { recovery: folder.join("journal"), open: None };
    let (mut failed, _, owns) = NodeCraftApp::from_storage_setup(Default::default(), Some(setup), "unused").unwrap();
    assert!(!owns);
    let startup = failed.notice.clone().unwrap();
    assert!(startup.contains("recovery folder can't be created"));
    assert!(!failed.storage_owner_blocked());
    let ctx = egui::Context::default();
    failed.begin_file("file.open", &ctx);
    assert_eq!(failed.notice.as_deref(), Some(startup.as_str()));
    let (mut missing, _, _) =
        NodeCraftApp::from_storage_setup(Default::default(), None, "No settings folder is available. Restart after fixing access.").unwrap();
    missing.begin_file("file.save_as", &ctx);
    assert_eq!(missing.notice.as_deref(), Some("No settings folder is available. Restart after fixing access."));
    let mut closed = dir.app("closed");
    closed.session.close_recovery(true).unwrap();
    assert!(!closed.session.file_status().available);
    let reason = closed.session.file_status().unavailable_reason.unwrap();
    assert!(reason.contains("Recovery was closed"));
    assert!(reason.contains("reopen NodeCraft"));
    closed.begin_file("file.open", &ctx);
    assert_eq!(closed.notice.as_deref(), Some(reason));
    assert_eq!(closed.session.dispatch("file.save", &json!({})).unwrap_err().message, reason);
    let mut h = harness(closed);
    settle(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::Window, "Project files").get_by_label(reason);
}

#[test]
fn storage968_replacement_does_not_reuse_an_old_failure_reason() {
    let (mut app, _, _) = NodeCraftApp::from_storage_setup(Default::default(), None, "Old startup reason").unwrap();
    let replacement = nodecraft_engine::Session::new(app.session.document().clone()).unwrap();
    app.replace_session(replacement, app.canvas).unwrap();
    app.begin_file("file.open", &egui::Context::default());
    assert!(!app.notice.as_deref().unwrap().contains("Old startup reason"));
    assert!(app.notice.as_deref().unwrap().contains("not configured"));
}

#[test]
#[ignore = "manual public-safe storage968 before/after evidence"]
fn render_storage968_evidence() {
    let output = PathBuf::from(std::env::var_os("NODECRAFT_STORAGE968_EVIDENCE").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let dir = Scratch::new("storage968-evidence");
    let mut app = dir.app("journal");
    app.session.close_recovery(true).unwrap();
    let mut h = harness(app);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h);
    h.render().unwrap().save(output.join("closed-recovery.png")).unwrap();
    h.get_by_label("File").click();
    settle(&mut h);
    h.render().unwrap().save(output.join("closed-recovery-file-panel.png")).unwrap();
}
