//! The ComfyUI server panel (#9): saving the address in settings and checking the server, with a
//! fixture serving `/object_info`, `/system_stats` and `/queue`.
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

/// Answers by path, and records the URLs asked for.
struct Server(Arc<Mutex<Vec<String>>>);

impl Http for Server {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
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
    let mut h = harness();
    serve(&mut h, dir, asked);
    h
}

/// Gives `h`'s app settings in `dir` and a fetch thread over the fixture server.
fn serve(h: &mut Harness<'static, NodeCraftApp>, dir: &std::path::Path, asked: Arc<Mutex<Vec<String>>>) {
    let http: Arc<dyn Http> = Arc::new(Server(asked));
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

#[test]
fn the_panel_saves_the_address_and_shows_the_servers_health() {
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
    h.get_by_label("No server yet. Add its address, such as a local ComfyUI's, then check it.");
    assert!(h.get_by_label("Check").accesskit_node().is_disabled(), "nothing to check without an address");
    h.get_by_role_and_label(Role::TextInput, "Address").click();
    h.run();
    h.event(egui::Event::Text("http://gpu-box:8188".into()));
    h.run();
    h.get_by_label("Save and check").click();
    h.run();
    assert!(h.state().comfy_nodes.as_ref().unwrap().is_waiting());
    answered(&mut h);
    assert_eq!(h.state().provider_settings().comfyui_url.as_deref(), Some("http://gpu-box:8188"));
    h.state_mut().store.as_mut().unwrap().flush().unwrap();
    let (saved, _) = open(Location::Dir(dir.path().to_path_buf()));
    assert_eq!(saved.get().providers.comfyui_url.as_deref(), Some("http://gpu-box:8188"), "the address is kept in settings");
    assert!(asked.lock().unwrap().iter().all(|url| url.starts_with("http://gpu-box:8188/")));
    h.get_by_label("ComfyUI 0.3.40 · cuda:0 GPU · 18.2 / 24.0 GB free · 0 running, 1 waiting");
    h.get_by_label("1 node type read; workflows can be imported.");
    h.get_by_label("A server that asks for a token: add it under API keys as comfyui:gpu-box:8188.");
    assert!(!h.get_by_label("Check").accesskit_node().is_disabled());
}

#[test]
fn a_new_address_drops_the_old_servers_nodes_and_a_bad_one_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut h = app(dir.path(), asked.clone());
    h.state_mut().check_comfy_server(" http://gpu-box:8188 ").unwrap();
    answered(&mut h);
    assert_eq!(comfy_workflow_cmds::last(&h.state().session).1, Some(1));

    let error = h.state_mut().check_comfy_server("http://me:hunter2@other:8188").unwrap_err();
    assert!(!error.message.contains("hunter2"), "{}", error.message);
    assert_eq!(h.state().provider_settings().comfyui_url.as_deref(), Some("http://gpu-box:8188"));
    assert_eq!(comfy_workflow_cmds::last(&h.state().session).1, Some(1), "a refused address keeps the server");

    // The same address, checked again, keeps the list while it is read.
    h.state_mut().check_comfy_server("http://gpu-box:8188").unwrap();
    assert_eq!(comfy_workflow_cmds::last(&h.state().session).1, Some(1));
    assert_eq!(h.state_mut().check_comfy_server("http://gpu-box:8188").unwrap_err().kind, ErrorKind::Busy, "one read at a time");
    // A new address while that read runs abandons it: only the new server's answer lands.
    let ctx = h.ctx.clone();
    h.state_mut().tick_comfy_nodes(&ctx);
    h.state_mut().check_comfy_server("http://other:8188").unwrap();
    assert_eq!(comfy_workflow_cmds::last(&h.state().session), (None, None), "the old server's nodes are dropped at once");
    answered(&mut h);
    assert_eq!(h.state().provider_settings().comfyui_url.as_deref(), Some("http://other:8188"));
    assert!(asked.lock().unwrap().last().unwrap().starts_with("http://other:8188/"));
    assert_eq!(comfy_workflow_cmds::last(&h.state().session).1, Some(1));
    assert!(h.state().notice.as_deref().unwrap_or_default().starts_with("Read 1 ComfyUI node type"));

    h.state_mut().check_comfy_server("").unwrap();
    assert_eq!(h.state().provider_settings().comfyui_url, None);
    assert_eq!(comfy_workflow_cmds::last(&h.state().session), (None, None), "the old server's nodes are dropped");
    assert_eq!(h.state().notice.as_deref(), Some("ComfyUI isn't set up: no server address."));
}

#[test]
#[ignore = "manual public-safe capture; COMFY9_RENDER_DIR"]
fn comfy9_server_visual() {
    let out = std::path::PathBuf::from(std::env::var_os("COMFY9_RENDER_DIR").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    serve(&mut h, dir.path(), Arc::new(Mutex::new(Vec::new())));
    h.state_mut().check_comfy_server("http://127.0.0.1:8188").unwrap();
    answered(&mut h);
    h.state_mut().comfy_server.open = true;
    h.state_mut().notice = None;
    for _ in 0..4 {
        h.step();
    }
    h.render().unwrap().save(out.join("comfy9-server-panel.png")).unwrap();
}
