//! While an earlier session's journal is kept aside at startup (#1053), the window has no storage
//! yet: a control request that would change the sample project is refused as busy rather than
//! applied unjournalled (and then blocking the startup open and Recover), and once the keep-aside
//! settles the window takes the journal over and starts its focus endpoint (#968).
use super::*;
use crate::shell::Shell;
use egui_kittest::Harness;
use nodecraft_ui_egui::app::storage::StorageSetup;

#[test]
fn a_control_request_during_the_keep_aside_is_refused_and_the_focus_endpoint_starts_after() {
    let base = tempfile::tempdir().unwrap();
    let journal = base.path().join("settings/recovery/session.journal");
    // A session edits and "crashes": its journal holds work to keep aside next launch.
    {
        let ctx = egui::Context::default();
        let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
        let setup = StorageSetup { recovery: journal.clone(), open: None };
        let mut crashed = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(setup)).unwrap();
        let canvas = crashed.canvas;
        let marker = nodecraft_types::NodeId::from_bytes([63; 16]);
        crashed.session.dispatch("node.create", &json!({"canvas":canvas,"id":marker,"kind":"source.text","pos":{"x":300,"y":0}})).unwrap();
    }
    let before = std::fs::read(&journal).unwrap();
    let (queued, waiting) = std::sync::mpsc::channel();
    let (mut client, calls) = control::local(Arc::new(move || {
        let _ = queued.send(());
    }));
    let added = nodecraft_types::NodeId::from_bytes([64; 16]);
    let (canvas_tx, canvas_rx) = std::sync::mpsc::channel::<serde_json::Value>();
    let reply = std::thread::spawn(move || {
        let canvas = canvas_rx.recv().unwrap();
        let params = json!({"id":"node.create","params":{"canvas":canvas,"id":added,"kind":"source.text","pos":{"x":0,"y":0}}});
        client.call("command.run", params.as_object().unwrap().clone())
    });
    let mut pending = Some((calls, canvas_tx));
    let recovery = journal.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_eframe(|cc| {
        let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
        let setup = StorageSetup { recovery: recovery.clone(), open: None };
        let app = NodeCraftApp::with_settings_and_storage(&cc.egui_ctx, store, Some(setup)).unwrap();
        assert!(app.keeping_aside(), "the earlier journal is being kept aside");
        let (calls, canvas_tx) = pending.take().unwrap();
        canvas_tx.send(serde_json::to_value(app.canvas).unwrap()).unwrap();
        waiting.recv().unwrap();
        let listener = control::Listener { addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)), calls };
        let channel = Channel { listener, pump: CallPump::new(FileRoots::default()), token_file: None };
        let agent = crate::agent_panel::AgentPanel::new(Err("no agent in this test".into()), Default::default());
        Shell { app, agent, channel: Some(channel), storage_window: None, storage_window_pending: Some(recovery.clone()), mcp: None }
    });
    // The first frame pumped the request while the keep-aside ran.
    let error = reply.join().unwrap().unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Busy, "{error}");
    assert!(error.message.contains("keeping the last session's unsaved work safe"), "{}", error.message);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while h.state().app.keeping_aside() || h.state().storage_window.is_none() {
        h.step();
        assert!(std::time::Instant::now() < deadline, "the keep-aside never settled: {:?}", h.state().app.notice);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let shell = h.state();
    assert!(shell.app.session.file_status().available, "the window took the journal over");
    assert!(shell.storage_window.is_some() && shell.storage_window_pending.is_none(), "and listens for a second window");
    let document = shell.app.session.document();
    assert!(!document.canvases().values().any(|c| c.nodes().contains_key(&added)), "the refused request left nothing");
    assert_eq!(std::fs::read(journal.with_file_name("session.journal.unrecovered-1")).unwrap(), before, "the work was kept");
}
