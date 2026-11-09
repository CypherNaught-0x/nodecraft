//! The startup open starts before the shell pumps any request (#367): a control request queued
//! before the first frame is refused as busy, never applied to the sample project and then
//! discarded by the open.
use super::*;
use crate::shell::Shell;
use egui_kittest::Harness;
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_ui_egui::app::storage::StorageSetup;

#[derive(Default)]
struct Gate(std::sync::Mutex<Vec<SaveJob>>);
impl SaveRunner for Gate {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}

#[test]
fn a_control_request_queued_before_the_first_frame_meets_the_startup_open_busy() {
    let base = tempfile::tempdir().unwrap();
    let file = base.path().join("startup.ncraft");
    let marker = nodecraft_types::NodeId::from_bytes([61; 16]);
    {
        let ctx = egui::Context::default();
        let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("seed-settings")));
        let setup = StorageSetup { recovery: base.path().join("seed-settings/recovery/session.journal"), open: None };
        let mut seed = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(setup)).unwrap();
        let canvas = seed.canvas;
        seed.session.dispatch("node.create", &json!({"canvas":canvas,"id":marker,"kind":"source.text","pos":{"x":300,"y":0}})).unwrap();
        seed.session.dispatch("file.save_as", &json!({"location":file.to_str().unwrap()})).unwrap();
        drop(seed);
    }
    let gate = Arc::new(Gate::default());
    let (queued, waiting) = std::sync::mpsc::channel();
    let (mut client, calls) = control::local(Arc::new(move || {
        let _ = queued.send(());
    }));
    let added = nodecraft_types::NodeId::from_bytes([62; 16]);
    let (canvas_tx, canvas_rx) = std::sync::mpsc::channel::<serde_json::Value>();
    let reply = std::thread::spawn(move || {
        let canvas = canvas_rx.recv().unwrap();
        let params = json!({"id":"node.create","params":{"canvas":canvas,"id":added,"kind":"source.text","pos":{"x":0,"y":0}}});
        client.call("command.run", params.as_object().unwrap().clone())
    });
    let mut pending = Some((calls, canvas_tx));
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_eframe(|cc| {
        let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
        let setup = StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: Some(file.clone()) };
        let mut app = NodeCraftApp::with_settings_and_storage(&cc.egui_ctx, store, Some(setup)).unwrap();
        app.session.set_save_runner(gate.clone());
        let (calls, canvas_tx) = pending.take().unwrap();
        // The request is in the queue before the shell's first frame runs.
        canvas_tx.send(serde_json::to_value(app.canvas).unwrap()).unwrap();
        waiting.recv().unwrap();
        let listener = control::Listener { addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)), calls };
        let channel = Channel { listener, pump: CallPump::new(FileRoots::default()), token_file: None };
        let agent = crate::agent_panel::AgentPanel::new(Err("no agent in this test".into()), Default::default());
        Shell { app, agent, channel: Some(channel), storage_window: None, storage_window_pending: None, mcp: None }
    });
    // Building the harness ran the first frame: logic (open, then the pump), then the window.
    let error = reply.join().unwrap().unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Busy, "{error}");
    assert!(error.message.contains("still opening"), "{}", error.message);
    let jobs = std::mem::take(&mut *gate.0.lock().unwrap());
    assert_eq!(jobs.len(), 1, "the startup open was waiting on the runner");
    jobs.into_iter().for_each(SaveJob::run);
    for _ in 0..20 {
        h.step();
    }
    let document = h.state().app.session.document();
    assert!(document.canvases().values().any(|c| c.nodes().contains_key(&marker)), "the file opened: {:?}", h.state().app.notice());
    assert!(!document.canvases().values().any(|c| c.nodes().contains_key(&added)), "the refused request left nothing");
}
