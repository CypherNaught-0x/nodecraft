use super::*;
use std::collections::VecDeque;

/// What the fake browser answers, and what it was asked to write.
#[derive(Default)]
struct Script {
    opens: VecDeque<Result<Option<Picked>, String>>,
    names: VecDeque<Result<Option<Named>, String>>,
    writes: VecDeque<Result<(), String>>,
    written: Vec<(Option<u64>, String, Vec<u8>)>,
    asked: Vec<String>,
    /// The next open never answers, as a file field that doesn't report a cancel.
    hang: bool,
}

struct Fake(Rc<RefCell<Script>>);

impl Port for Fake {
    fn open(&mut self, _max: u64) -> PickFuture<Option<Picked>> {
        if std::mem::take(&mut self.0.borrow_mut().hang) {
            return Box::pin(std::future::pending());
        }
        let answer = self.0.borrow_mut().opens.pop_front().unwrap_or(Ok(None));
        Box::pin(std::future::ready(answer))
    }
    fn save_as(&mut self, suggested: &str) -> PickFuture<Option<Named>> {
        let mut script = self.0.borrow_mut();
        script.asked.push(suggested.to_owned());
        let answer = script.names.pop_front().unwrap_or(Ok(None));
        Box::pin(std::future::ready(answer))
    }
    fn write(&mut self, target: Option<u64>, name: &str, bytes: Vec<u8>) -> PickFuture<()> {
        let mut script = self.0.borrow_mut();
        script.written.push((target, name.to_owned(), bytes));
        let answer = script.writes.pop_front().unwrap_or(Ok(()));
        Box::pin(std::future::ready(answer))
    }
}

fn app() -> (NodeCraftApp, Rc<RefCell<Script>>) {
    let script = Rc::new(RefCell::new(Script::default()));
    let app = NodeCraftApp::with_web_files(nodecraft_engine::commands::Services::default(), WebFiles::new(Box::new(Fake(Rc::clone(&script)))), None).unwrap();
    (app, script)
}

fn add_text(app: &mut NodeCraftApp, n: u8) {
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([n; 16]), "source.text");
    app.session.dispatch("node.add", &json!({"canvas": app.canvas, "node": node})).unwrap();
}

/// Runs `f` inside one egui pass, as the app's frame does.
fn frame(app: &mut NodeCraftApp, ctx: &egui::Context, f: impl FnOnce(&mut NodeCraftApp, &egui::Context)) {
    let mut f = Some(f);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        if let Some(f) = f.take() {
            f(app, ui.ctx());
        }
    });
    // No renderer takes the font atlas here.
    output.textures_delta.clear();
}

/// Chooses `id`, then lets the next frame finish what the browser was asked to do.
fn choose(app: &mut NodeCraftApp, ctx: &egui::Context, id: &str) {
    frame(app, ctx, |app, ctx| app.web_file(id, ctx));
    frame(app, ctx, |app, ctx| {
        app.web_files_frame(ctx);
    });
}

/// Saves `app` as `name` through a save picker, returning what the browser was asked to write.
fn save_as(app: &mut NodeCraftApp, script: &Rc<RefCell<Script>>, ctx: &egui::Context, name: &str, target: Option<u64>) -> (Option<u64>, String, Vec<u8>) {
    script.borrow_mut().names.push_back(Ok(Some(Named { name: name.into(), target })));
    choose(app, ctx, "file.save_as");
    script.borrow_mut().written.pop().unwrap()
}

#[test]
fn save_as_hands_the_project_to_the_picked_file_and_save_reuses_it() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    assert!(app.session.file_status().available, "the browser has project files");
    add_text(&mut app, 50);
    let (target, name, bytes) = save_as(&mut app, &script, &ctx, "forest.ncraft", Some(7));
    assert_eq!((target, name.as_str()), (Some(7), "forest.ncraft"));
    assert!(!bytes.is_empty());
    assert!(!app.session.file_status().dirty, "{:?}", app.notice);
    assert!(!app.web_files.unsaved.get());

    add_text(&mut app, 51);
    frame(&mut app, &ctx, |app, ctx| assert!(!app.web_files_frame(ctx)));
    assert!(app.web_files.unsaved.get(), "an edit is unsaved work");
    choose(&mut app, &ctx, "file.save");
    let script = script.borrow();
    assert_eq!(script.asked, ["Untitled.ncraft"], "save goes straight to the picked file");
    let (target, name, again) = script.written.last().unwrap();
    assert_eq!((*target, name.as_str()), (Some(7), "forest.ncraft"));
    assert_ne!(again, &bytes, "the edit was written");
    assert!(!app.session.file_status().dirty);
}

#[test]
fn an_untitled_save_asks_where_and_a_cancelled_picker_changes_nothing() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    add_text(&mut app, 50);
    choose(&mut app, &ctx, "file.save");
    assert_eq!(script.borrow().asked, ["Untitled.ncraft"]);
    assert!(script.borrow().written.is_empty());
    assert!(app.session.file_status().dirty, "still unsaved");
    assert!(app.notice.is_none(), "cancelling isn't an error: {:?}", app.notice);
    assert!(!app.web_files.busy());
}

#[test]
fn a_download_says_where_the_copy_went() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    let (target, name, _) = save_as(&mut app, &script, &ctx, "forest.ncraft", None);
    assert_eq!((target, name.as_str()), (None, "forest.ncraft"));
    assert!(app.notice.as_deref().is_some_and(|n| n.starts_with("Downloaded forest.ncraft.")), "{:?}", app.notice);
}

#[test]
fn a_failed_write_keeps_the_page_guarded_until_a_save_lands() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    script.borrow_mut().writes.push_back(Err("There isn't enough space to write the file.".into()));
    save_as(&mut app, &script, &ctx, "forest.ncraft", Some(3));
    assert!(!app.session.file_status().dirty, "the session saved");
    assert!(app.web_files.unsaved.get(), "but the file didn't get it");
    let notice = app.notice.clone().unwrap();
    assert!(notice.contains("forest.ncraft wasn't saved") && notice.contains("enough space"), "{notice}");
    choose(&mut app, &ctx, "file.save");
    assert_eq!(script.borrow().written.last().unwrap().0, Some(3), "saved to the same file again");
    assert!(!app.web_files.unsaved.get());
}

#[test]
fn opening_a_picked_project_replaces_the_document_and_saves_go_back_to_it() {
    let ctx = egui::Context::default();
    let (mut source, script) = app();
    add_text(&mut source, 50);
    let (_, _, bytes) = save_as(&mut source, &script, &ctx, "forest.ncraft", None);
    let saved = source.session.document().clone();

    let (mut app, script) = app();
    script.borrow_mut().opens.push_back(Ok(Some(Picked { name: "forest.ncraft".into(), bytes, target: Some(9) })));
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    assert!(!app.web_files.busy(), "an untouched starter project isn't asked about");
    assert!(app.notice.is_none(), "{:?}", app.notice);
    assert_eq!(app.session.document(), &saved, "the picked project is open");
    assert!(app.web_files.staged.borrow().incoming.is_none(), "the bytes aren't kept");
    add_text(&mut app, 52);
    choose(&mut app, &ctx, "file.save");
    assert!(script.borrow().asked.is_empty(), "no picker for a project that has a file");
    assert_eq!(script.borrow().written.last().unwrap().0, Some(9));
}

#[test]
fn opening_over_unsaved_changes_asks_first() {
    let ctx = egui::Context::default();
    let (mut source, script) = app();
    let (_, _, bytes) = save_as(&mut source, &script, &ctx, "forest.ncraft", None);
    let saved = source.session.document().clone();

    let (mut app, script) = app();
    add_text(&mut app, 50);
    let edited = app.session.document().clone();
    let picked = || Picked { name: "forest.ncraft".into(), bytes: bytes.clone(), target: None };
    script.borrow_mut().opens.push_back(Ok(Some(picked())));
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    assert!(matches!(app.web_files.phase, Some(Phase::Confirm(_))));
    frame(&mut app, &ctx, |app, ctx| assert!(app.web_files_frame(ctx), "the question owns input"));
    app.web_answer(false);
    assert!(!app.web_files.busy());
    assert_eq!(app.session.document(), &edited, "cancel keeps the edits");

    script.borrow_mut().opens.push_back(Ok(Some(picked())));
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    app.web_answer(true);
    assert_eq!(app.session.document(), &saved, "discard opens the picked project");
}

#[test]
fn a_damaged_or_refused_pick_says_why_and_keeps_the_project() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    let before = app.session.document().clone();
    script.borrow_mut().opens.push_back(Ok(Some(Picked { name: "junk.ncraft".into(), bytes: b"not a project".to_vec(), target: None })));
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    assert!(app.notice.is_some());
    assert_eq!(app.session.document(), &before);
    assert!(app.web_files.staged.borrow().incoming.is_none());

    script.borrow_mut().opens.push_back(Err("The project is larger than the browser build opens (64 MiB).".into()));
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    assert_eq!(app.notice.as_deref(), Some("The project is larger than the browser build opens (64 MiB)."));
}

#[test]
fn host_storage_only_hands_over_what_was_picked() {
    let (_, mut storage) = WebFiles::new(Box::new(Fake(Rc::default())));
    let key = StorageLocation::Key("1/forest.ncraft".into());
    assert_eq!(storage.load(&key).unwrap_err().kind, ErrorKind::NotFound);
    storage.0.borrow_mut().incoming = Some(("2/other.ncraft".into(), vec![1]));
    assert_eq!(storage.load(&key).unwrap_err().kind, ErrorKind::NotFound, "another pick's bytes stay put");
    assert!(storage.0.borrow().incoming.is_some());
    storage.0.borrow_mut().incoming = Some(("1/forest.ncraft".into(), vec![1, 2]));
    assert_eq!(storage.load(&key).unwrap(), [1, 2]);
    assert!(storage.load(&key).is_err(), "taken once");

    let big = vec![0; MAX_ARCHIVE_BYTES + 1];
    assert!(storage.save(&key, &big).is_err());
    storage.save(&key, b"saved").unwrap();
    assert_eq!(storage.0.borrow().written, Some(("1/forest.ncraft".into(), b"saved".to_vec())));
}

#[test]
fn locations_are_unique_and_keep_a_safe_file_name() {
    let (mut files, _) = WebFiles::new(Box::new(Fake(Rc::default())));
    let a = files.location("forest.ncraft");
    let b = files.location("forest.ncraft");
    assert_ne!(a, b);
    assert_eq!(file_name(&a), "forest.ncraft");
    assert_eq!(file_name(&files.location("a/b\u{7}.ncraft")), "ab.ncraft");
    assert_eq!(file_name(&files.location(" \n")), "Untitled.ncraft");
}

#[test]
fn a_new_tab_can_be_left_without_a_warning() {
    let ctx = egui::Context::default();
    let (mut app, _) = app();
    frame(&mut app, &ctx, |app, ctx| assert!(!app.web_files_frame(ctx)));
    assert!(!app.web_files.unsaved.get(), "the starter project has nothing to lose");
    add_text(&mut app, 50);
    frame(&mut app, &ctx, |app, ctx| assert!(!app.web_files_frame(ctx)));
    assert!(app.web_files.unsaved.get());
}

#[test]
fn opening_after_a_failed_write_asks_first() {
    let ctx = egui::Context::default();
    let (mut source, script) = app();
    let (_, _, bytes) = save_as(&mut source, &script, &ctx, "other.ncraft", None);

    let (mut app, script) = app();
    add_text(&mut app, 50);
    script.borrow_mut().writes.push_back(Err("There isn't enough space to write the file.".into()));
    save_as(&mut app, &script, &ctx, "forest.ncraft", Some(3));
    assert!(!app.session.file_status().dirty);
    script.borrow_mut().opens.push_back(Ok(Some(Picked { name: "other.ncraft".into(), bytes, target: None })));
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    assert!(matches!(app.web_files.phase, Some(Phase::Confirm(_))), "the unwritten save would be lost");
}

#[test]
fn a_file_field_that_never_answers_doesnt_block_saving() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    add_text(&mut app, 50);
    script.borrow_mut().hang = true;
    frame(&mut app, &ctx, |app, ctx| app.web_file("file.open", ctx));
    assert!(app.web_files.busy());
    frame(&mut app, &ctx, |app, ctx| assert!(!app.web_files_frame(ctx)));
    let (target, _, _) = save_as(&mut app, &script, &ctx, "forest.ncraft", Some(4));
    assert_eq!(target, Some(4), "save replaced the waiting open");
    assert!(!app.web_files.busy());
}

#[test]
fn a_save_made_by_another_caller_still_reaches_the_file() {
    let ctx = egui::Context::default();
    let (mut app, script) = app();
    save_as(&mut app, &script, &ctx, "forest.ncraft", Some(5));
    add_text(&mut app, 50);
    // As the agent or a script would, straight through the command.
    app.session.dispatch("file.save", &json!({})).unwrap();
    frame(&mut app, &ctx, |app, ctx| {
        app.web_files_frame(ctx);
    });
    frame(&mut app, &ctx, |app, ctx| {
        app.web_files_frame(ctx);
    });
    let script = script.borrow();
    assert_eq!(script.written.len(), 1, "the earlier write was taken by save_as");
    assert_eq!(script.written[0].0, Some(5));
    assert!(!app.web_files.unsaved.get());
}
