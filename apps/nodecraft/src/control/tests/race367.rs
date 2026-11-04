//! A control client racing the person's background open (#367): while it runs, a change is
//! refused as busy and names `file.cancel`, a read still answers, and once it settles the
//! client works on the opened project.
use super::*;
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};

#[derive(Default)]
struct Gate(std::sync::Mutex<Vec<SaveJob>>);
impl SaveRunner for Gate {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}

#[test]
fn a_control_client_meets_a_background_open_busy_and_then_the_opened_project() {
    let base = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
    let storage = nodecraft_ui_egui::app::storage::StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: None };
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(storage)).unwrap();
    draw(&mut app, &ctx);
    let file = base.path().join("raced.ncraft");
    let canvas = app.canvas;
    let marker = nodecraft_types::NodeId::from_bytes([71; 16]);
    app.session.dispatch("node.create", &json!({"canvas":canvas,"id":marker,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    app.session.dispatch("file.save_as", &json!({"location":file.to_str().unwrap()})).unwrap();
    app.session.dispatch("node.remove", &json!({"canvas":canvas,"id":marker})).unwrap();
    let gate = Arc::new(Gate::default());
    app.session.set_save_runner(gate.clone());
    app.session.dispatch("file.open", &json!({"location":file.to_str().unwrap(),"discard_dirty":true,"background":true})).unwrap();
    let mut connection = connection();
    let other = nodecraft_types::NodeId::from_bytes([72; 16]);
    let create = json!({"canvas":canvas,"id":other,"kind":"source.text","pos":{"x":0,"y":0}});
    let error = run(&mut app, &ctx, &mut connection, "node.create", create.clone()).unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Busy, "{error}");
    assert!(error.message.contains("file.cancel"), "{}", error.message);
    assert!(list(&mut app, &ctx, &mut connection)["commands"].is_array(), "reads still answer");
    std::mem::take(&mut *gate.0.lock().unwrap()).into_iter().for_each(SaveJob::run);
    // The window's frame picks up the open, then the client's change lands on that project.
    draw(&mut app, &ctx);
    run(&mut app, &ctx, &mut connection, "node.create", create).unwrap();
    let nodes = app.session.document().canvases()[&canvas].nodes().clone();
    assert!(nodes.contains_key(&marker) && nodes.contains_key(&other));
}

/// A control client changing the project while the person's ⌘S is still writing (#367): the
/// change used to wait on the UI thread for the disk. It is refused at once instead, as busy and
/// naming `file.cancel`, with the save's job held, so waiting would never have returned; reads
/// still answer, and once the save is done the change lands after it.
#[test]
fn a_control_client_meets_a_background_save_busy_instead_of_blocking_the_window() {
    let base = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
    let storage = nodecraft_ui_egui::app::storage::StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: None };
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(storage)).unwrap();
    draw(&mut app, &ctx);
    let file = base.path().join("saving.ncraft");
    let canvas = app.canvas;
    let marker = nodecraft_types::NodeId::from_bytes([73; 16]);
    app.session.dispatch("node.create", &json!({"canvas":canvas,"id":marker,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    let gate = Arc::new(Gate::default());
    app.session.set_save_runner(gate.clone());
    assert_eq!(app.session.dispatch("file.save_as", &json!({"location":file.to_str().unwrap(),"background":true})).unwrap()["saving"], true);
    let saved = app.session.document().clone();
    let mut connection = connection();
    let other = nodecraft_types::NodeId::from_bytes([74; 16]);
    let create = json!({"canvas":canvas,"id":other,"kind":"source.text","pos":{"x":0,"y":0}});
    let started = std::time::Instant::now();
    let error = run(&mut app, &ctx, &mut connection, "node.create", create.clone()).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "answered without waiting for the disk");
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Busy, "{error}");
    assert!(error.message.contains("file.cancel") && error.message.contains("saved"), "{}", error.message);
    // Format selection edits the document too, through the window's host: refused the same way.
    let layout = run(&mut app, &ctx, &mut connection, "canvas.auto_format", json!({"mode":"grid"})).unwrap_err();
    assert_eq!(layout.kind, nodecraft_types::ErrorKind::Busy, "{layout}");
    assert!(list(&mut app, &ctx, &mut connection)["commands"].is_array(), "reads still answer");
    assert_eq!(app.session.document(), &saved);
    std::mem::take(&mut *gate.0.lock().unwrap()).into_iter().for_each(SaveJob::run);
    draw(&mut app, &ctx);
    run(&mut app, &ctx, &mut connection, "node.create", create).unwrap();
    assert!(app.session.document().canvases()[&canvas].nodes().contains_key(&other));
    // The save published before the change (the engine's own tests read the file back).
    assert!(file.exists());
    assert!(app.session.file_status().dirty, "saved, then changed");
}
