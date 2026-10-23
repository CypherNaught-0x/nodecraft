//! Reading the ComfyUI server's nodes in the desktop (#9), with `/object_info` served by a fixture,
//! then importing a workflow against them.
use super::*;
use crate::app::NodeCraftApp;
use crate::app::tests::harness;
use crate::palette;
use egui_kittest::Harness;
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, ProviderError};

const OBJECT_INFO: &str = r#"{
  "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["IMAGE"]},
  "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true}
}"#;
const WORKFLOW: &str = r#"{
  "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
  "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "nc"}}
}"#;

struct Fixture;

impl Http for Fixture {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: Vec::new(), body: OBJECT_INFO.as_bytes().to_vec() }) })
    }
}

/// Reads from the fixture server, or with the app's own settings (none, headless) when `server`
/// is false.
fn fetcher(server: bool) -> Fetch {
    let http: Arc<dyn Http> = Arc::new(Fixture);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    Box::new(move |settings| {
        let fixture = ProviderSettings { comfyui_url: Some("http://127.0.0.1:8188".into()), install_salt: "salt".into() };
        let settings = if server { &fixture } else { settings };
        let keys = nodecraft_engine::secrets::MemoryStore::default();
        let env = HostEnv { keys: &keys, http: http.clone(), settings, now_ms: Arc::new(|| 0), progress: None };
        runtime.block_on(comfy_workflow_cmds::fetch(&env))
    })
}

fn with_fetcher(fetch: Fetch) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let (fetcher, mut service) = ComfyNodesFetcher::spawn(fetch).unwrap();
    let app = h.state_mut();
    app.session.register_host_service(NODES_SERVICE, move |request| service(request)).unwrap();
    app.comfy_nodes = Some(fetcher);
    h
}

/// Frames until the fetch is told.
fn answered(h: &mut Harness<'static, NodeCraftApp>) -> String {
    for _ in 0..500 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_comfy_nodes(&ctx);
        if !h.state().comfy_nodes.as_ref().unwrap().is_waiting() {
            return h.state().notice.clone().unwrap_or_default();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the fetch never answered");
}

#[test]
fn the_palette_reads_the_servers_nodes_and_a_workflow_then_imports() {
    let mut h = with_fetcher(fetcher(true));
    assert!(h.state().palette_entries().iter().any(|entry| entry.target == palette::Target::Command("comfy.nodes.refresh")));
    h.state_mut().dispatch_palette_command("comfy.nodes.refresh");
    let busy = h.state_mut().session.dispatch("comfy.nodes.refresh", &json!({})).unwrap_err();
    assert_eq!(busy.kind, ErrorKind::Busy);
    assert_eq!(answered(&mut h), "Read 2 ComfyUI node types; workflows can be imported.");
    let canvas = *h.state().session.document().canvases().keys().next().unwrap();
    let id = nodecraft_types::NodeId::from_bytes([7; 16]);
    let result = h
        .state_mut()
        .session
        .dispatch("comfy.workflow.import", &json!({"canvas": canvas, "id": id, "pos": {"x": 0.0, "y": 0.0}, "workflow": WORKFLOW}))
        .unwrap();
    assert_eq!(result["outputs"], json!(["image"]));
}

#[test]
fn without_a_server_in_settings_the_notice_says_what_to_add() {
    let mut h = with_fetcher(fetcher(false));
    h.state_mut().session.dispatch("comfy.nodes.refresh", &json!({})).unwrap();
    let notice = answered(&mut h);
    assert!(notice.starts_with("ComfyUI's nodes weren't read:"), "{notice}");
    assert!(notice.contains("comfyui_url"), "{notice}");
}
