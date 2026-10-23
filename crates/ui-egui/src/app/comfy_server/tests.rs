//! The ComfyUI servers panel (#9, #864): adding, editing, defaulting and removing servers in
//! settings and checking each, with a fixture serving `/object_info`, `/system_stats` and `/queue`
//! (and failing for the host `down`).
use super::*;
use crate::app::comfy_nodes::{ComfyNodesFetcher, Fetch};
use crate::app::tests::harness;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use nodecraft_engine::comfy_workflow_cmds::NODES_SERVICE;
use nodecraft_engine::provider_setup::HostEnv;
use nodecraft_engine::settings::{Location, open};
use nodecraft_providers::http::Http;
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, ProviderError};
use std::sync::{Arc, Mutex};

const OBJECT_INFO: &str = r#"{"SaveImage": {"input": {"required": {"images": ["IMAGE", {}]}}, "output": [], "output_node": true}}"#;
const SYSTEM_STATS: &str = r#"{"system": {"os": "posix", "comfyui_version": "0.3.40"},
  "devices": [{"name": "cuda:0 GPU", "type": "cuda", "vram_total": 24000000000, "vram_free": 18200000000}]}"#;
const QUEUE: &str = r#"{"queue_running": [], "queue_pending": [[1, "b", {}, {}, []]]}"#;

/// Answers by path, and records the URLs asked for. The host `down` answers 503. Each answer waits
/// for the gate, so a test can hold a check open while it acts.
struct Server(Arc<Mutex<Vec<String>>>, Arc<Mutex<()>>);

impl Http for Server {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        drop(self.1.lock().unwrap());
        if request.url.starts_with("http://down:") {
            self.0.lock().unwrap().push(request.url);
            return Box::pin(async { Ok(HttpResponse { status: 503, headers: Vec::new(), body: b"busy".to_vec() }) });
        }
        let body = match request.url.rsplit('/').next().unwrap_or_default() {
            "object_info" => OBJECT_INFO,
            "system_stats" => SYSTEM_STATS,
            _ => QUEUE,
        };
        self.0.lock().unwrap().push(request.url);
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: Vec::new(), body: body.as_bytes().to_vec() }) })
    }
}

/// The app with settings in `dir` and a fetch thread over the fixture server.
fn app(dir: &std::path::Path, asked: Arc<Mutex<Vec<String>>>) -> Harness<'static, NodeCraftApp> {
    gated_app(dir, asked, Arc::default())
}

/// [`app`] whose fixture server answers only while `gate` isn't held.
fn gated_app(dir: &std::path::Path, asked: Arc<Mutex<Vec<String>>>, gate: Arc<Mutex<()>>) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    serve(&mut h, dir, asked, gate);
    h
}

/// Gives `h`'s app settings in `dir` and a fetch thread over the fixture server.
fn serve(h: &mut Harness<'static, NodeCraftApp>, dir: &std::path::Path, asked: Arc<Mutex<Vec<String>>>, gate: Arc<Mutex<()>>) {
    let http: Arc<dyn Http> = Arc::new(Server(asked, gate));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let fetch: Fetch = Box::new(move |settings| {
        let keys = nodecraft_engine::secrets::MemoryStore::default();
        let env = HostEnv { keys: &keys, http: http.clone(), settings, now_ms: Arc::new(|| 0), progress: None };
        runtime.block_on(comfy_workflow_cmds::fetch(&env))
    });
    let (fetcher, mut service) = ComfyNodesFetcher::spawn(fetch).unwrap();
    let app = h.state_mut();
    let (mut store, _) = open(Location::Dir(dir.to_path_buf()));
    store.update(|settings| settings.providers.install_salt = "salt".into());
    app.store = Some(store);
    app.session.register_host_service(NODES_SERVICE, move |request| service(request)).unwrap();
    app.comfy_nodes = Some(fetcher);
}

/// Frames until the check is answered.
fn answered(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..500 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_comfy_nodes(&ctx);
        if !h.state().comfy_nodes.as_ref().unwrap().is_waiting() {
            h.run();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the check never answered");
}

fn servers(h: &Harness<'static, NodeCraftApp>) -> Vec<(String, String, Option<u32>)> {
    h.state().provider_settings().comfyui.iter().map(|server| (server.name.clone(), server.url.clone(), server.max_jobs)).collect()
}

fn default_name(h: &Harness<'static, NodeCraftApp>) -> Option<String> {
    h.state().provider_settings().default_comfy().map(|server| server.name.clone())
}

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Option<usize> {
    comfy_workflow_cmds::last(&h.state().session).1
}

#[test]
fn the_panel_adds_a_server_and_shows_its_health() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut h = app(dir.path(), asked.clone());
    // The toolbar re-anchors the frame after it grew by the new button.
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label("ComfyUI").click();
    h.run();
    assert!(h.state().comfy_server.open);
    h.get_by_label("No server yet. Add one, such as a local ComfyUI, then check it.");
    assert!(h.get_by_label("Add and check").accesskit_node().is_disabled(), "nothing to add without an address");
    h.get_by_role_and_label(Role::TextInput, "New server").click();
    h.run();
    h.event(egui::Event::Text("http://gpu-box:8188".into()));
    h.run();
    h.get_by_label("Add and check").click();
    h.run();
    assert!(h.state().comfy_nodes.as_ref().unwrap().is_waiting());
    answered(&mut h);
    assert_eq!(servers(&h), [("gpu-box:8188".to_owned(), "http://gpu-box:8188".to_owned(), None)], "named after its address");
    assert_eq!(default_name(&h).as_deref(), Some("gpu-box:8188"), "the first server is the default");
    h.state_mut().store.as_mut().unwrap().flush().unwrap();
    let (saved, _) = open(Location::Dir(dir.path().to_path_buf()));
    assert_eq!(saved.get().providers.comfyui.len(), 1, "the server is kept in settings");
    assert!(asked.lock().unwrap().iter().all(|url| url.starts_with("http://gpu-box:8188/")));
    h.get_by_label("ComfyUI 0.3.40 · cuda:0 GPU · 18.2 / 24.0 GB free · 0 running, 1 waiting");
    h.get_by_label("1 node type read; workflows can be imported.");
    h.get_by_label("A token, if it asks for one: API keys, comfyui:gpu-box:8188.");
    h.get_by_label("default");
    assert!(!h.get_by_label("Check").accesskit_node().is_disabled());

    // Editing the jobs at once turns Check into Save and check.
    h.get_by_role_and_label(Role::TextInput, "Jobs at once").click();
    h.run();
    h.event(egui::Event::Text("1".into()));
    h.run();
    h.get_by_label("Save and check").click();
    h.run();
    answered(&mut h);
    assert_eq!(servers(&h)[0].2, Some(1));
}

#[test]
fn each_server_is_checked_on_its_own_and_only_the_default_gives_the_node_list() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut h = app(dir.path(), asked.clone());
    h.state_mut().add_comfy_server("studio", " http://gpu-box:8188 ", "").unwrap();
    answered(&mut h);
    assert_eq!(nodes(&h), Some(1));
    h.state_mut().add_comfy_server("spare", "http://down:8188", "2").unwrap();
    answered(&mut h);
    assert_eq!(default_name(&h).as_deref(), Some("studio"), "adding another keeps the default");
    assert_eq!(servers(&h)[1], ("spare".to_owned(), "http://down:8188".to_owned(), Some(2)));
    let spare = comfy_server_cmds::report(&h.state().session, "spare").unwrap();
    assert!(spare.error.is_some(), "{spare:?}");
    assert_eq!(nodes(&h), Some(1), "another server failing keeps the default's nodes");
    assert!(h.state().notice.as_deref().unwrap_or_default().starts_with("spare: "), "{:?}", h.state().notice);
    assert!(comfy_server_cmds::report(&h.state().session, "studio").unwrap().error.is_none());

    assert_eq!(h.state_mut().add_comfy_server("spare", "http://other:8188", "").unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(h.state_mut().add_comfy_server("third", "http://other:8188", "0").unwrap_err().kind, ErrorKind::InvalidInput);
    let error = h.state_mut().save_comfy_server("studio", "http://me:hunter2@other:8188", "").unwrap_err();
    assert!(!error.message.contains("hunter2"), "{}", error.message);
    assert_eq!(servers(&h)[0].1, "http://gpu-box:8188", "a refused address changes nothing");
    assert_eq!(nodes(&h), Some(1));

    // Checking a server that isn't there keeps no report, so made-up names can't crowd out real ones.
    h.state_mut().session.dispatch("comfy.nodes.refresh", &serde_json::json!({"server": "ghost"})).unwrap();
    answered(&mut h);
    assert!(comfy_server_cmds::report(&h.state().session, "ghost").is_none());
    assert_eq!(h.state().notice.as_deref(), Some("There's no ComfyUI server called ghost."));
    assert_eq!(nodes(&h), Some(1));

    // Making the failing server the default drops the node list it can't give.
    h.state_mut().default_comfy_server("spare").unwrap();
    assert_eq!(nodes(&h), None);
    answered(&mut h);
    assert_eq!(default_name(&h).as_deref(), Some("spare"));
    assert_eq!(nodes(&h), None);
    assert!(comfy_server_cmds::report(&h.state().session, "studio").is_some(), "the old default's health stays in its row");

    // Removing the default hands it to the next server, whose nodes are read again.
    h.state_mut().remove_comfy_server("spare").unwrap();
    assert!(comfy_server_cmds::report(&h.state().session, "spare").is_none());
    answered(&mut h);
    assert_eq!(default_name(&h).as_deref(), Some("studio"));
    assert_eq!(nodes(&h), Some(1));
    h.state_mut().remove_comfy_server("studio").unwrap();
    assert!(servers(&h).is_empty());
    assert_eq!(nodes(&h), None);
    assert_eq!(h.state().provider_settings().comfyui_default, None);
}

#[test]
fn changing_one_server_leaves_another_servers_check_running() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let mut h = gated_app(dir.path(), Arc::new(Mutex::new(Vec::new())), gate.clone());
    h.state_mut().add_comfy_server("studio", "http://gpu-box:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("spare", "http://other:8188", "").unwrap();
    answered(&mut h);
    // Studio's read is held open until the edit below is made.
    let held = gate.lock().unwrap();
    h.state_mut().check_comfy_server("studio").unwrap();
    assert_eq!(h.state_mut().check_comfy_server("spare").unwrap_err().kind, ErrorKind::Busy, "one read at a time");
    let ctx = h.ctx.clone();
    h.state_mut().tick_comfy_nodes(&ctx);
    // Editing the other server keeps studio's read; its answer still lands. The edit is saved, and
    // its own check waits its turn.
    h.state_mut().save_comfy_server("spare", "http://other:8189", "").unwrap();
    assert_eq!(servers(&h)[1].1, "http://other:8189");
    assert!(h.state().comfy_nodes.as_ref().unwrap().is_waiting());
    drop(held);
    answered(&mut h);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "studio").unwrap().id, 3);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "spare").unwrap().id, 4);
    assert!(comfy_server_cmds::report(&h.state().session, "spare").unwrap().error.is_none());
    // Editing studio itself drops its read at once, and the new address is read instead.
    h.state_mut().check_comfy_server("studio").unwrap();
    h.state_mut().tick_comfy_nodes(&ctx);
    h.state_mut().save_comfy_server("studio", "http://gpu-box:8190", "").unwrap();
    assert_eq!(nodes(&h), None, "the old address's nodes are dropped");
    answered(&mut h);
    assert_eq!(nodes(&h), Some(1));
    assert_eq!(servers(&h)[0].1, "http://gpu-box:8190");
}

fn edit_address(h: &mut Harness<'static, NodeCraftApp>, row: usize, url: &str) {
    let pos = h.query_all_by_role_and_label(Role::TextInput, "Address").nth(row).unwrap().rect().center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(url.into()));
    h.run();
}

#[test]
fn editing_a_in_the_panel_queues_its_check_behind_b() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut h = gated_app(dir.path(), asked.clone(), gate.clone());
    h.state_mut().add_comfy_server("a", "http://a:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("b", "http://b:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().comfy_server.open = true;
    h.run();
    let held = gate.lock().unwrap();
    h.state_mut().check_comfy_server("b").unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().tick_comfy_nodes(&ctx);
    edit_address(&mut h, 0, "http://a:8189");
    h.get_by_label("Save and check").click();
    h.run();
    assert_eq!(servers(&h)[0].1, "http://a:8189");
    assert!(comfy_server_cmds::report(&h.state().session, "a").is_none());
    drop(held);
    answered(&mut h);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "b").unwrap().id, 3);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "a").unwrap().id, 4);
    assert!(asked.lock().unwrap().iter().any(|url| url == "http://a:8189/object_info"));
    assert_eq!(h.query_all_by_role_and_label(Role::Label, "ComfyUI 0.3.40 · cuda:0 GPU · 18.2 / 24.0 GB free · 0 running, 1 waiting").count(), 2);
}

#[test]
fn abandoning_a_running_server_preserves_another_servers_accepted_pending_id() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let mut h = gated_app(dir.path(), Arc::new(Mutex::new(Vec::new())), gate.clone());
    h.state_mut().add_comfy_server("a", "http://a:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("b", "http://b:8188", "").unwrap();
    answered(&mut h);
    let held = gate.lock().unwrap();
    h.state_mut().check_comfy_server("b").unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().tick_comfy_nodes(&ctx);
    h.state_mut().comfy_nodes.as_mut().unwrap().allow_followup(true);
    let pending = h.state_mut().session.dispatch("comfy.nodes.refresh", &json!({"server": "a"})).unwrap()["pending"].as_u64().unwrap();
    h.state_mut().comfy_nodes.as_mut().unwrap().allow_followup(false);
    h.state_mut().comfy_nodes.as_mut().unwrap().abandon();
    drop(held);
    // Observe the abandoned id's terminal report BEFORE the queued id can replace the
    // latest per-server status. Checking only the queued id missed this regression.
    let mut abandoned = false;
    for _ in 0..500 {
        h.state_mut().tick_comfy_nodes(&ctx);
        if let Some(report) = comfy_server_cmds::report(&h.state().session, "b")
            && report.id == pending - 1
        {
            assert!(report.error.as_deref().unwrap_or_default().contains("abandoned"), "{report:?}");
            abandoned = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(abandoned, "the abandoned accepted id never received its terminal report");
    answered(&mut h);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "a").unwrap().id, pending);
    assert!(comfy_server_cmds::report(&h.state().session, "a").unwrap().error.is_none());
}

#[test]
fn rapid_saves_of_one_server_share_one_pending_id_and_one_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut h = gated_app(dir.path(), asked.clone(), gate.clone());
    h.state_mut().add_comfy_server("a", "http://a:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("b", "http://b:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().comfy_server.open = true;
    h.run();
    let held = gate.lock().unwrap();
    h.state_mut().check_comfy_server("b").unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().tick_comfy_nodes(&ctx);
    edit_address(&mut h, 0, "http://a:8189");
    h.get_by_label("Save and check").click();
    h.run();
    edit_address(&mut h, 0, "http://a:8190");
    h.get_by_label("Save and check").click();
    h.run();
    assert_eq!(servers(&h)[0].1, "http://a:8190");
    // Additional saves/checks of the same queued server reuse the accepted id rather
    // than exhausting the bounded queue or promising reports nobody can observe.
    h.state_mut().comfy_nodes.as_mut().unwrap().allow_followup(true);
    for _ in 0..100 {
        let pending = h.state_mut().session.dispatch("comfy.nodes.refresh", &json!({"server": "a"})).unwrap()["pending"].as_u64();
        assert_eq!(pending, Some(4));
    }
    h.state_mut().comfy_nodes.as_mut().unwrap().allow_followup(false);
    drop(held);
    answered(&mut h);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "b").unwrap().id, 3, "cross-server check is independent");
    assert_eq!(comfy_server_cmds::report(&h.state().session, "a").unwrap().id, 4, "all same-server callers share the accepted id's terminal report");
    assert_eq!(h.state().comfy_nodes.as_ref().unwrap().completed_ids(), [1, 2, 3, 4], "every distinct accepted id is completed exactly once");
    assert_eq!(asked.lock().unwrap().iter().filter(|url| url.ends_with("/object_info") && url.starts_with("http://a:8190/")).count(), 1);
    assert!(!asked.lock().unwrap().iter().any(|url| url.starts_with("http://a:8189/")), "the queued check uses the latest saved settings");
}

#[test]
fn adding_and_removing_the_default_queue_followup_checks() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let mut h = gated_app(dir.path(), Arc::new(Mutex::new(Vec::new())), gate.clone());
    h.state_mut().add_comfy_server("a", "http://a:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("b", "http://b:8188", "").unwrap();
    answered(&mut h);
    let held = gate.lock().unwrap();
    h.state_mut().check_comfy_server("b").unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().tick_comfy_nodes(&ctx);
    h.state_mut().add_comfy_server("c", "http://c:8188", "").unwrap();
    h.state_mut().remove_comfy_server("a").unwrap();
    assert_eq!(default_name(&h).as_deref(), Some("b"));
    drop(held);
    answered(&mut h);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "c").unwrap().id, 4);
    assert_eq!(comfy_server_cmds::report(&h.state().session, "b").unwrap().id, 5);
    assert_eq!(nodes(&h), Some(1));
}

#[test]
fn panel_action_preserves_unsaved_draft_in_another_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path(), Arc::new(Mutex::new(Vec::new())));
    h.state_mut().add_comfy_server("a", "http://a:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("b", "http://b:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().comfy_server.open = true;
    h.run();
    edit_address(&mut h, 0, "http://a:8189");
    assert_eq!(h.state().comfy_server.rows[0].url, "http://a:8189");
    let pos = h.query_all_by_role_and_label(Role::Button, "Remove").nth(1).unwrap().rect().center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert_eq!(servers(&h).len(), 1, "the unrelated row was removed through the panel");
    assert_eq!(h.state().comfy_server.rows[0].url, "http://a:8189");
    assert_eq!(servers(&h)[0].1, "http://a:8188", "the draft was not saved by the other action");
    h.get_by_label("Save and check").click();
    h.run();
    answered(&mut h);
    assert_eq!(servers(&h)[0].1, "http://a:8189");
}

#[test]
fn a_comfy_cards_menu_picks_its_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path(), Arc::new(Mutex::new(Vec::new())));
    h.state_mut().store.as_mut().unwrap().update(|settings| {
        settings.providers.comfyui = vec![ComfyServer::new("studio", "http://gpu-box:8188"), ComfyServer::new("spare", "http://other:8188")];
        settings.providers.comfyui_default = Some("studio".into());
    });
    const CARD: nodecraft_types::NodeId = nodecraft_types::NodeId::from_bytes([41; 16]);
    let canvas = h.state().canvas;
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": CARD, "kind": nodecraft_nodes::generation::KIND, "pos": {"x": 200, "y": -110}})).unwrap();
    let mut request = json!({"canvas": canvas, "nodes": [CARD], "model": "comfyui.sdxl"});
    let preview = session.dispatch("node.model_preview", &request).unwrap();
    request["expected_revision"] = preview["revision"].clone();
    request["preview_binding"] = preview["binding"].clone();
    session.dispatch("node.set_model", &request).unwrap();
    h.run();
    let picked = |h: &Harness<'static, NodeCraftApp>| {
        let app = h.state();
        app.session
            .document()
            .canvases()
            .get(&app.canvas)
            .unwrap()
            .nodes()
            .get(&CARD)
            .unwrap()
            .config
            .get(nodecraft_engine::provider_setup::COMFY_SERVER_CONFIG)
            .cloned()
    };
    crate::app::split_tests::right_click(&mut h, CARD);
    h.get_by_label_contains("ComfyUI server").hover();
    h.run();
    h.get_by_label("Default (studio)");
    h.get_by_label("spare").click();
    h.run();
    assert_eq!(picked(&h), Some(nodecraft_types::Value::Text("spare".into())), "{:?}", h.state().notice);
    crate::app::split_tests::right_click(&mut h, CARD);
    h.get_by_label_contains("ComfyUI server").hover();
    h.run();
    h.get_by_label("Default (studio)").click();
    h.run();
    assert_eq!(picked(&h), None, "back to the default");
}

#[test]
#[ignore = "manual public-safe capture; COMFY9_RENDER_DIR"]
fn comfy9_server_visual() {
    let out = std::path::PathBuf::from(std::env::var_os("COMFY9_RENDER_DIR").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::Vec2::new(1000.0, 800.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    serve(&mut h, dir.path(), Arc::new(Mutex::new(Vec::new())), Arc::default());
    h.state_mut().add_comfy_server("studio", "http://127.0.0.1:8188", "1").unwrap();
    answered(&mut h);
    h.state_mut().add_comfy_server("spare", "http://down:8188", "").unwrap();
    answered(&mut h);
    h.state_mut().comfy_server.open = true;
    h.state_mut().notice = None;
    for _ in 0..4 {
        h.step();
    }
    h.render().unwrap().save(out.join("comfy864-servers-panel.png")).unwrap();
}
