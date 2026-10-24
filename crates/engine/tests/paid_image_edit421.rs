//! Real image adapters behind the shipping catalog, asset resolver and RunHost. Offline only.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, Provider, ProviderError, secrets::Secret};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([42; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const JPEG: &[u8] = include_bytes!("fixtures/seedream15.jpg");

fn base64(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(alphabet[(a >> 2) as usize] as char);
        out.push(alphabet[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 { alphabet[(((b & 15) << 2) | (c >> 6)) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { alphabet[(c & 63) as usize] as char } else { '=' });
    }
    out
}

struct Canned {
    reply: Vec<u8>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("paid image adapters must use the no-redirect boundary")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: self.reply.clone() }) })
    }
}

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

fn edit(model: &str, provider: Arc<dyn Provider>, http: &Canned) {
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::builtin().unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut params = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Make the public fixture blue".into()))]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    if model.starts_with("openai-images.") {
        params.insert("output_format".into(), Value::Text("jpeg".into()));
    }
    let card = kinds.get(KIND).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    assert!(card.inputs.iter().any(|port| port.name == "image"), "the selected catalog model exposes editing");
    let mut project = Project::new(ProjectId::from_bytes([43; 16]), "Public image edit").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "offline-image-edit".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy::NEVER);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(provider).unwrap();
    routing.select(CARD, binding.target().adapter(), binding.target()).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    s.set_provider_routing(routing).unwrap();
    host.install(&mut s);
    let hash = s.import_asset(JPEG).unwrap();
    let source = s
        .kinds()
        .get("source.image")
        .unwrap()
        .instantiate_instance(
            SOURCE,
            Default::default(),
            &[("asset".into(), Value::Media(AssetRef { hash, mime: "image/jpeg".parse().unwrap(), meta: MediaMeta::default() }))].into(),
        )
        .unwrap();
    s.dispatch("node.add", &json!({"canvas":C,"node":source})).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([3; 16]),
        from: Endpoint { node: SOURCE, port: "out".into() },
        to: Endpoint { node: CARD, port: "image".into() },
        order: 0,
    };
    s.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = s.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{model}: {q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    request["accept"] = json!({"token":q["token"],"amounts":amounts,"partial":true,"unpriced_jobs":1});
    s.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut s));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{model}: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{model}: {notices:?}");
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{model}: {notices:?}");
    let node = s.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    let [generation] = node.history.as_slice() else { panic!("one recorded edit") };
    let [Value::List(images)] = generation.outputs.as_slice() else { panic!("image list output") };
    let [Value::Media(output)] = images.as_slice() else { panic!("one complete image") };
    assert_eq!(output.hash, hash, "the fixture's exact completed JPEG was imported");
    let requests = http.requests.lock().unwrap();
    let [request] = requests.as_slice() else { panic!("one dispatch, no paid retry") };
    if model.starts_with("openai-images.") {
        assert!(request.url.ends_with("/images/edits"));
        assert!(request.body.windows(JPEG.len()).any(|w| w == JPEG), "the stored reference reached the multipart edit");
    } else {
        let encoded = base64(JPEG);
        assert!(request.body.windows(encoded.len()).any(|w| w == encoded.as_bytes()), "the stored reference reached the real adapter wire format");
    }
}

#[test]
fn all_three_image_pickers_edit_through_the_shipping_host_and_asset_store() {
    let data = base64(JPEG);
    for (model, reply) in [
        (
            "gemini-images.flash-lite",
            json!({"status":"completed","steps":[{"type":"model_output","content":[{"type":"image","mime_type":"image/jpeg","data":data}]}]}),
        ),
        // Omit Seedream's advisory size entirely, including on the shipping host path.
        ("seedream.4_5", json!({"model":nodecraft_providers::seedream::MODEL,"data":[{"b64_json":data}]})),
        ("openai-images.sunburst", json!({"data":[{"b64_json":data}]})),
    ] {
        let http = Arc::new(Canned { reply: serde_json::to_vec(&reply).unwrap(), requests: Mutex::default() });
        let key = Secret::new("fixture-key-not-real").unwrap();
        let provider: Arc<dyn Provider> = match model {
            "gemini-images.flash-lite" => Arc::new(nodecraft_providers::gemini_images::GeminiImages::new(key, http.clone())),
            "seedream.4_5" => Arc::new(nodecraft_providers::seedream::Seedream::new(key, http.clone())),
            _ => Arc::new(nodecraft_providers::openai_images::OpenAiImages::new(key, http.clone())),
        };
        edit(model, provider, &http);
    }
}
