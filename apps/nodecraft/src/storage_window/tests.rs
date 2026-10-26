use super::*;
use std::{
    thread,
    time::{Duration, Instant},
};

#[test]
fn storage968_owner_channel_focuses_and_refuses_document_commands() {
    let dir = tempfile::tempdir().unwrap();
    let setup = nodecraft_ui_egui::app::storage::StorageSetup { recovery: dir.path().join("journal"), open: None };
    // Exercise the same native constructor as the desktop, including real journal ownership.
    let ctx = egui::Context::default();
    let store = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Memory).0;
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(setup.clone())).unwrap();
    assert!(app.session.file_status().available);
    let mut owner = StorageWindow::start(&mut app, &ctx, &setup.recovery).unwrap().unwrap();
    let path = owner.owner.as_ref().unwrap().1.clone();
    let before = app.session.document().clone();
    let worker_path = path.clone();
    let worker = thread::spawn(move || focus_remote(&worker_path));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.is_finished() {
        owner.pump(&mut app, &ctx);
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    worker.join().unwrap().unwrap();
    let mut output = ctx.run_ui(Default::default(), |_| {});
    let commands = &output.viewport_output.get(&egui::ViewportId::ROOT).unwrap().commands;
    assert!(commands.iter().any(|command| matches!(command, egui::ViewportCommand::Focus)));
    output.textures_delta.clear();
    for request in [
        Request::State,
        Request::Screenshot,
        Request::RunCommand(json!({"id":"node.add","params":{}}).as_object().unwrap().clone()),
        Request::RunCommand(json!({"id":"file.focus_owner","params":{"location":"anything"}}).as_object().unwrap().clone()),
    ] {
        assert_eq!(answer(&mut app, &request).unwrap_err().kind, ErrorKind::Forbidden);
    }
    assert_eq!(app.session.document(), &before);
    assert!(path.exists());
    let replacement = nodecraft_engine::Session::new(before.clone()).unwrap();
    app.replace_session(replacement, app.canvas).unwrap();
    owner.pump(&mut app, &ctx);
    assert!(!path.exists(), "session replacement retires the old owner endpoint");
    drop(owner);
    assert!(!path.exists());
}

#[test]
fn storage968_missing_owner_request_fails_without_freezing_or_changing_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let setup = nodecraft_ui_egui::app::storage::StorageSetup { recovery: dir.path().join("journal"), open: None };
    let make_store = || nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Memory).0;
    let owner = NodeCraftApp::with_settings_and_storage(&ctx, make_store(), Some(setup.clone())).unwrap();
    let mut guest = NodeCraftApp::with_settings_and_storage(&ctx, make_store(), Some(setup.clone())).unwrap();
    assert!(guest.storage_owner_blocked());
    let mut channel = StorageWindow::start(&mut guest, &ctx, &setup.recovery).unwrap().unwrap();
    let before = guest.session.document().clone();
    assert_eq!(guest.session.dispatch("file.focus_owner", &json!({})).unwrap(), json!({"requested":true}));
    assert_eq!(guest.session.dispatch("file.focus_owner", &json!({})).unwrap_err().kind, ErrorKind::Busy);
    let deadline = Instant::now() + Duration::from_secs(10);
    while channel.busy.load(Ordering::SeqCst) {
        channel.pump(&mut guest, &ctx);
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(guest.notice.as_deref().unwrap().contains("could not be reached"));
    assert_eq!(guest.session.document(), &before);
    assert!(owner.session.file_status().available);
}
