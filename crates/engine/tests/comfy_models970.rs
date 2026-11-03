//! The ComfyUI server's own models as choices (#970), through the engine and the real ComfyUI
//! adapter against a scripted server: a checkpoint the server lists is put on an image card and
//! the queued workflow loads exactly that file; a checkpoint the server lacks is refused before
//! anything is queued, never swapped for another. No network.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session, comfy_server_cmds,
    comfy_workflow_cmds::NodeCatalog,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, ProviderError, comfyui::ComfyProvider};
use nodecraft_types::{CanvasId, NodeId, ParamValues, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([0x70; 16]);
const CARD: NodeId = NodeId::from_bytes([0x71; 16]);
const OBJECT_INFO: &str = include_str!("fixtures/comfy_object_info970.json");

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A ComfyUI server listing the fixture's models that finishes every prompt with one PNG.
struct Server {
    requests: Mutex<Vec<HttpRequest>>,
    png: Vec<u8>,
}
impl Http for Server {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let path = r.url.split("8188").nth(1).unwrap_or_default().to_owned();
        let body = match path.split('?').next().unwrap_or_default() {
            "/object_info" => OBJECT_INFO.as_bytes().to_vec(),
            "/queue" => br#"{"queue_running":[],"queue_pending":[]}"#.to_vec(),
            "/prompt" => {
                let sent: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                serde_json::to_vec(&json!({"prompt_id": sent["prompt_id"], "number": 1, "node_errors": {}})).unwrap()
            }
            "/view" => self.png.clone(),
            p if p.starts_with("/history/") => {
                let id = p.trim_start_matches("/history/");
                if self.requests.lock().unwrap().iter().any(|r| r.url.ends_with("/prompt")) {
                    let files = json!([{"filename": "NodeCraft_00001_.png", "subfolder": "", "type": "output"}]);
                    serde_json::to_vec(
                        &json!({id: {"outputs": {"10": {"images": files}}, "status": {"status_str": "success", "completed": true, "messages": []}}}),
                    )
                    .unwrap()
                } else {
                    b"{}".to_vec()
                }
            }
            _ => b"{}".to_vec(),
        };
        self.requests.lock().unwrap().push(r);
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

/// An image card on a cloud model, the scripted server as the default ComfyUI server, and its
/// node list read as `comfy.nodes.refresh` would.
fn session() -> (Session, RunHost, Arc<Server>) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0x70; 16]), "Server models").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values: ParamValues =
        [("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("a lighthouse at dusk".into()))].into();
    let node = kinds.get(nodecraft_nodes::generation::KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "server-models".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let png = nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap();
    let server = Arc::new(Server { requests: Mutex::default(), png });
    let comfy = ComfyProvider::new("http://gpu-box:8188", None, "fixture-salt", server.clone(), Arc::new(|| 0)).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(comfy)).unwrap();
    routing.select(CARD, "comfyui", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let nodes = NodeCatalog::parse(OBJECT_INFO.as_bytes()).unwrap().0;
    comfy_server_cmds::finish(&mut session, 1, "gpu-box", true, Ok(Arc::new(nodes).into()));
    (session, host, server)
}

/// Uses `name` from the server's checkpoints on the card, through the reviewed preview.
fn use_checkpoint(session: &mut Session, name: &str) {
    let mut request = json!({"canvas": C, "nodes": [CARD], "group": "checkpoints", "name": name});
    let preview = session.dispatch("comfy.models.preview", &request).unwrap();
    request["expected_revision"] = preview["revision"].clone();
    request["preview_binding"] = preview["binding"].clone();
    session.dispatch("comfy.models.use", &request).unwrap();
}

/// Starts a free run of the card and waits for it to settle.
fn run(session: &mut Session, host: &mut RunHost) -> Vec<RunNotice> {
    let request = json!({"canvas": C, "mode": "selected", "nodes": [CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    let mut start = request;
    start["accept"] = json!({"token": quote["token"], "partial": false, "unpriced_jobs": 0, "amounts": {}});
    session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    notices
}

#[test]
fn the_fixture_servers_checkpoints_and_loras_are_listed_grouped() {
    let (mut session, _host, _server) = session();
    let list = session.dispatch("comfy.models.list", &json!({"canvas": C, "nodes": [CARD]})).unwrap();
    assert_eq!(list["server"], "gpu-box");
    let group = |id: &str| list["groups"].as_array().unwrap().iter().find(|g| g["id"] == id).unwrap().clone();
    let names = |group: &serde_json::Value| group["models"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert!(names(&group("checkpoints")).contains(&"juggernaut_xl_v9.safetensors".to_owned()));
    assert_eq!(names(&group("loras")), ["detail_tweaker.safetensors", "watercolor_style.safetensors"]);
}

#[test]
fn a_card_made_from_a_server_checkpoint_runs_with_that_checkpoint() {
    let (mut session, mut host, server) = session();
    use_checkpoint(&mut session, "juggernaut_xl_v9.safetensors");
    let notices = run(&mut session, &mut host);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let requests = server.requests.lock().unwrap();
    let prompt = requests.iter().find(|r| r.url.ends_with("/prompt")).expect("the built-in workflow was queued");
    let sent: serde_json::Value = serde_json::from_slice(&prompt.body).unwrap();
    assert_eq!(sent["prompt"]["1"]["class_type"], "CheckpointLoaderSimple");
    assert_eq!(sent["prompt"]["1"]["inputs"]["ckpt_name"], "juggernaut_xl_v9.safetensors", "the chosen file, not the default");
    assert_eq!(sent["prompt"]["2"]["inputs"]["text"], "a lighthouse at dusk");
}

#[test]
fn a_checkpoint_the_server_lacks_is_refused_and_never_replaced() {
    let (mut session, mut host, server) = session();
    let before = session.document().clone();
    let refused = session.dispatch("comfy.models.preview", &json!({"canvas": C, "nodes": [CARD], "group": "checkpoints", "name": "gone.safetensors"}));
    assert_eq!(refused.unwrap_err().kind, nodecraft_types::ErrorKind::NotFound);
    assert_eq!(session.document(), &before);

    // A card whose checkpoint has since gone from the server is refused at run time too.
    use_checkpoint(&mut session, "juggernaut_xl_v9.safetensors");
    session
        .dispatch("node.param_set", &json!({"canvas": C, "id": CARD, "name": "checkpoint", "value": {"type": "text", "value": "gone.safetensors"}}))
        .unwrap();
    let list = session.dispatch("comfy.models.list", &json!({"canvas": C, "nodes": [CARD]})).unwrap();
    assert_eq!(list["current"][0]["present"], false, "shown as missing");
    let notices = run(&mut session, &mut host);
    assert!(
        notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. }) && format!("{n:?}").contains("gone.safetensors")),
        "{notices:?}"
    );
    assert!(!server.requests.lock().unwrap().iter().any(|r| r.url.ends_with("/prompt")), "nothing was queued with another checkpoint");
    let card = session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    assert_eq!(card.params.get("checkpoint"), Some(&Value::Text("gone.safetensors".into())), "the card keeps what it names");
}
