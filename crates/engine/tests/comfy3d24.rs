//! Offline end-to-end proof of local image-to-mesh presets, not a live Hunyuan3D execution.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    import::{Incoming, import_files},
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::MESH_KIND;
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, ProviderError, comfyui::ComfyProvider};
use nodecraft_types::{CanvasId, EdgeId, NodeId, Number, ProjectId, Unsigned, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([24; 16]);
const CARD: NodeId = NodeId::from_bytes([25; 16]);
const IMAGE: NodeId = NodeId::from_bytes([26; 16]);
const INFO: &str = include_str!("../../providers/src/comfyui/fixtures/mesh-object-info.json");
const OBJ: &[u8] = b"v -1 0 0\nv 1 0 0\nv 0 2 0\nf 1 2 3\n";
const GLTF: &[u8] = include_bytes!("../../providers/src/comfyui/fixtures/triangle.gltf");
const RETAINED_PROMPT: &str = "A retained prompt from another model must never be sent to this image-only preset";

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
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
struct Server {
    name: String,
    bytes: Vec<u8>,
    poster: Vec<u8>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Server {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = url::Url::parse(&request.url).unwrap();
        let body = match url.path() {
            "/object_info" => INFO.as_bytes().to_vec(),
            "/queue" => br#"{"queue_running":[],"queue_pending":[]}"#.to_vec(),
            "/upload/image" => br#"{"name":"original-reference.png","subfolder":"input","type":"input"}"#.to_vec(),
            "/prompt" => {
                let posted: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                serde_json::to_vec(&json!({"prompt_id":posted["prompt_id"],"number":1,"node_errors":{}})).unwrap()
            }
            "/view" => {
                let filename = url.query_pairs().find(|(key, _)| key == "filename").unwrap().1;
                if filename.as_ref() == self.name.as_str() {
                    self.bytes.clone()
                } else {
                    assert_eq!(filename, "poster.png");
                    self.poster.clone()
                }
            }
            path if path.starts_with("/history/") => {
                let id = path.trim_start_matches("/history/");
                let posted = self
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| request.url.ends_with("/prompt"))
                    .any(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["prompt_id"].as_str() == Some(id));
                if posted {
                    serde_json::to_vec(&json!({id:{"outputs":{"10":{"3d":[{"filename":self.name,"subfolder":"mesh","type":"output"}],"images":[{"filename":"poster.png","subfolder":"mesh","type":"output"}]}},"status":{"status_str":"success","completed":true,"messages":[]}}})).unwrap()
                } else {
                    b"{}".to_vec()
                }
            }
            path => panic!("unexpected authored Comfy fixture request: {path}"),
        };
        self.requests.lock().unwrap().push(request);
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

fn run(model: &str, extension: &str, mime: &str, bytes: Vec<u8>) {
    let catalog = GenerationCatalog::builtin().unwrap();
    let kinds = catalog.registry().unwrap();
    let params = [
        ("model".into(), Value::Text(model.into())),
        ("prompt".into(), Value::Text(RETAINED_PROMPT.into())),
        ("seed".into(), Value::Unsigned(Unsigned(17))),
        ("steps".into(), Value::Unsigned(Unsigned(25))),
        ("cfg".into(), Value::Number(Number::new(4.5).unwrap())),
    ]
    .into();
    let card = kinds.get(MESH_KIND).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([24; 16]), "Original local 3D fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Local 3D") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "comfy3d24-proof".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let server = Arc::new(Server {
        name: format!("original.{extension}"),
        bytes: bytes.clone(),
        poster: nodecraft_media::encode_png(2, 2, &[160; 16], &Default::default()).unwrap(),
        requests: Mutex::default(),
    });
    let provider = ComfyProvider::new("http://original-fixture:8188", None, "comfy3d24-original", server.clone(), Arc::new(|| 0)).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(provider)).unwrap();
    routing.select(CARD, "comfyui", binding.target()).unwrap();
    let mut resources = Resources::default();
    resources.insert(catalog.clone());
    resources.insert(catalog.run_catalog());
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let image = nodecraft_media::encode_png(4, 4, &[120; 64], &Default::default()).unwrap();
    import_files(&mut session, C, Position { x: -300., y: 0. }, 200., &[Incoming { name: "original-reference.png", bytes: &image }], &[IMAGE]).unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([27;16]),"from":{"node":IMAGE,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}}),
        )
        .unwrap();
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    let total = quote["totals"].as_object().unwrap().values().map(|amount| amount["micros"].as_u64().unwrap()).sum::<u64>();
    assert_eq!(total, 0, "local inference has no NodeCraft provider charge: {quote}");
    assert!(server.requests.lock().unwrap().is_empty(), "quoting must not submit or upload");
    request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":false,"unpriced_jobs":0});
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = vec![];
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "run did not finish: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{model}/{extension}: {notices:?}");
    assert!(!notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let node = session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    let generation = node.history.iter().find(|generation| Some(generation.id) == node.active).unwrap();
    let [Value::Media(asset)] = generation.outputs.as_slice() else { panic!("one exact typed mesh, no poster: {:?}", generation.outputs) };
    assert_eq!(asset.mime.essence(), mime);
    let metrics = nodecraft_format::asset_source::StreamMetrics::default();
    let actual = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap();
    assert_eq!(actual, bytes);
    let mesh = nodecraft_media::decode_mesh(&actual, &Default::default()).unwrap();
    assert_eq!((mesh.positions.len(), mesh.indices.len()), (3, 3));
    let asset = asset.clone();
    let expected = session.document().clone();
    let archive = nodecraft_format::encode(&nodecraft_format::Snapshot { project: expected.clone(), assets: session.assets().clone(), epoch: 0 }).unwrap();
    let reopened = Session::with_storage(nodecraft_format::decode(&archive).unwrap(), Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
    assert_eq!(reopened.document(), &expected);
    reopened.assets().validate_project(reopened.document()).unwrap();
    assert_eq!(reopened.assets().open_verified(&asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap(), bytes);
    assert!(budget.log.lock().unwrap().entries().all(|entry| entry.cost.micros == 0));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|request| request.url.ends_with("/prompt")).count(), 1);
    assert_eq!(requests.iter().filter(|request| request.url.ends_with("/upload/image")).count(), 1);
    let posted = &requests.iter().find(|request| request.url.ends_with("/prompt")).unwrap().body;
    let sent: serde_json::Value = serde_json::from_slice(posted).unwrap();
    assert!(!String::from_utf8_lossy(posted).contains(RETAINED_PROMPT));
    assert_eq!(sent["prompt"]["2"]["inputs"]["image"], "input/original-reference.png");
    assert_eq!(sent["prompt"]["7"]["inputs"]["seed"], 17);
    assert_eq!(sent["prompt"]["7"]["inputs"]["steps"], 25);
    assert_eq!(sent["prompt"]["7"]["inputs"]["cfg"], 4.5);
    assert_eq!(sent["prompt"]["10"]["class_type"], "SaveGLB");
    assert_eq!(sent["prompt"].get("6").is_some(), model.ends_with("2.1"));
    assert!(requests.iter().flat_map(|request| &request.headers).all(|(name, _)| !name.eq_ignore_ascii_case("authorization")));
}
#[test]
fn comfy3d24_actual_host_selected_quote_consent_exact_mesh_history_and_save_reopen() {
    let mesh = nodecraft_media::decode_mesh(OBJ, &Default::default()).unwrap();
    let glb = nodecraft_media::encode_glb(&mesh).unwrap();
    for model in ["comfyui.hunyuan3d-2", "comfyui.hunyuan3d-2.1"] {
        for (extension, mime, bytes) in
            [("glb", "model/gltf-binary", glb.clone()), ("obj", "model/obj", OBJ.to_vec()), ("gltf", "model/gltf+json", GLTF.to_vec())]
        {
            run(model, extension, mime, bytes);
        }
    }
}
