//! The ComfyUI workflow card (#9) through the engine: a card placed from an imported workflow is
//! planned, quoted free and run through the real ComfyUI adapter against a scripted server, which
//! receives the stored workflow with the card's values and the file wired into it, and runs only
//! the output the card keeps; its saved image lands on the card. No network.
#![cfg(not(target_arch = "wasm32"))]
#[path = "comfy_workflow9/cloud.rs"]
mod cloud;
use nodecraft_engine::{
    Session,
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
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::comfy_workflow::{self, Card, MappedInput, MappedOutput, MappedParam, Mapping};
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, ProviderError, comfyui::ComfyProvider};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, MediaMeta, Modality, NodeId, ParamControl, ParamSpec, ProjectId, Unsigned, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([9; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const OBJECT_INFO: &str = r#"{
  "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
  "TextToImage": {"input": {"required": {"text": ["STRING", {"multiline": true}], "seed": ["INT", {"min": 0, "max": 4294967295}], "image": ["IMAGE", {}]}}, "output": ["IMAGE"]},
  "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true}
}"#;
const WORKFLOW: &str = r#"{
  "1": {"class_type": "TextToImage", "inputs": {"text": "a red fox", "seed": 7, "image": ["3", 0]}},
  "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "NodeCraft"}},
  "3": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
  "4": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "unmapped"}}
}"#;

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

/// A ComfyUI server that finishes every prompt at once with one saved PNG; keeps every request.
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
            "/upload/image" => br#"{"name":"stored.png","subfolder":"in","type":"input"}"#.to_vec(),
            "/view" => self.png.clone(),
            p if p.starts_with("/history/") => {
                let id = p.trim_start_matches("/history/");
                let posted = self.requests.lock().unwrap().iter().any(|r| r.url.ends_with("/prompt"));
                if posted {
                    let files = json!([{"filename": "NodeCraft_00001_.png", "subfolder": "", "type": "output"}]);
                    serde_json::to_vec(
                        &json!({id: {"outputs": {"2": {"images": files}}, "status": {"status_str": "success", "completed": true, "messages": []}}}),
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

/// What importing the workflow above stores: its Load Image node as a port, its prompt and seed
/// as parameters, its first Save Image node as the result (the second is left out).
fn card() -> Card {
    let spec = |name: &str, control: ParamControl, default: Value| ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        control,
        required: false,
        default: Some(default),
        visible_if: None,
    };
    Card {
        workflow: WORKFLOW.into(),
        mapping: Mapping {
            inputs: vec![MappedInput { node: "3".into(), input: "image".into(), port: "photo".into(), modality: Modality::Image }],
            params: vec![
                MappedParam {
                    node: "1".into(),
                    input: "text".into(),
                    spec: spec("prompt", ParamControl::Text { multiline: true, max_bytes: 4096 }, Value::Text("a red fox".into())),
                },
                MappedParam { node: "1".into(), input: "seed".into(), spec: spec("seed", ParamControl::Seed { max: None }, Value::Unsigned(Unsigned(7))) },
            ],
            outputs: vec![MappedOutput { node: "2".into(), label: "Save Image".into(), modality: Modality::Image }],
            partners: Vec::new(),
            output_names: Vec::new(),
        },
    }
}

#[test]
fn the_workflow_model_is_free_and_offered_on_no_ordinary_card() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let catalog = snapshot.run_catalog();
    let model = catalog.catalog().get(comfy_workflow::MODEL_ID).expect("the workflow card's model");
    assert_eq!((model.provider.as_str(), model.provider_model.as_str()), (comfy_workflow::PROVIDER, comfy_workflow::REMOTE_MODEL));
    assert!(!model.cards);
    for kind in [nodecraft_nodes::generation::KIND, nodecraft_nodes::generation::VIDEO_KIND, nodecraft_nodes::generation::AUDIO_KIND] {
        assert!(snapshot.model(kind, comfy_workflow::MODEL_ID).is_none(), "{kind} never offers it");
    }
    assert!(snapshot.registry().unwrap().get(comfy_workflow::ID).is_ok());
}

/// A session with `card` placed, its photo input wired, and a ComfyUI server answering it.
fn placed(card: Card) -> (Session, RunHost, Arc<Server>) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Workflow").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("prompt".into(), Value::Text("a blue whale".into()))].into();
    let node = kinds.get(comfy_workflow::ID).unwrap().instantiate_instance(CARD, card.config().unwrap(), &values).unwrap();
    assert_eq!(node.params.get("model"), Some(&Value::Text(comfy_workflow::MODEL_ID.into())));
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "workflow".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let png = nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap();
    let server = Arc::new(Server { requests: Mutex::default(), png: png.clone() });
    let comfy = ComfyProvider::new("http://gpu-box:8188", None, "fixture-salt", server.clone(), Arc::new(|| 0)).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(comfy)).unwrap();
    routing.select(CARD, comfy_workflow::PROVIDER, binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let hash = session.import_asset(&png).unwrap();
    let asset = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let source = session.kinds().get("source.image").unwrap().instantiate_instance(SOURCE, Default::default(), &[("asset".into(), asset)].into()).unwrap();
    session.dispatch("node.add", &json!({"canvas":C,"node":source})).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([3; 16]),
        from: Endpoint { node: SOURCE, port: "out".into() },
        to: Endpoint { node: CARD, port: "photo".into() },
        order: 0,
    };
    session.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    (session, host, server)
}

#[test]
fn a_placed_workflow_is_quoted_free_and_runs_on_the_comfyui_server() {
    let (mut session, mut host, server) = placed(card());

    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["complete"], true, "a free model's quote is complete: {q}");
    let charged = q["totals"].as_object().map_or(0, |totals| totals.values().map(|t| t["micros"].as_u64().unwrap()).sum::<u64>());
    assert_eq!(charged, 0, "it runs on your own server: {q}");
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{}});
    session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let card = session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    let result = card.history.iter().find(|g| Some(g.id) == card.active).expect("an active result");
    let saved = matches!(result.outputs.as_slice(), [Value::List(items)] if matches!(items.as_slice(), [Value::Media(a)] if a.mime.essence() == "image/png"));
    assert!(saved, "one saved image on the card's image list port: {:?}", result.outputs);

    let requests = server.requests.lock().unwrap();
    let prompt = requests.iter().find(|r| r.url.ends_with("/prompt")).expect("the workflow was queued");
    let sent: serde_json::Value = serde_json::from_slice(&prompt.body).unwrap();
    assert_eq!(sent["prompt"]["1"]["inputs"]["text"], "a blue whale", "the card's value replaces the workflow's");
    assert_eq!(sent["prompt"]["1"]["inputs"]["seed"], 7);
    assert_eq!(sent["prompt"]["2"]["class_type"], "SaveImage");
    assert!(requests.iter().any(|r| r.url.ends_with("/upload/image")), "the wired file was uploaded");
    assert_eq!(sent["prompt"]["3"]["inputs"]["image"], "in/stored.png", "the workflow loads the wired file");
    assert!(sent["prompt"].get("4").is_none(), "the output the card left out never runs");
}

#[test]
fn a_workflow_with_partner_nodes_is_never_quoted_free() {
    let mut card = card();
    card.mapping.partners = vec!["PartnerImage".into()];
    let (mut session, _host, _server) = placed(card);
    let q = session.dispatch("run.quote", &json!({"canvas":C,"mode":"selected","nodes":[CARD]})).unwrap();
    assert_eq!(q["complete"], false, "{q}");
    assert!(q["steps"][0]["unpriced"].as_str().is_some_and(|why| why.contains("PartnerImage") && why.contains("comfy.org")), "{q}");
}

#[path = "comfy_workflow9/mapping947.rs"]
mod mapping947;
