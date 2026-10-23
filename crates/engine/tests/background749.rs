//! The catalog background-removal mode through the shipping RunHost. Offline only.
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
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([42; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
fn png(alpha: u8) -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[32, 64, 96, alpha].repeat(4), &nodecraft_media::Limits::default()).unwrap()
}

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
    replies: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Canned {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("paid image adapters must use the no-redirect boundary")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let response = self.replies.lock().unwrap().pop_front().expect("one canned response per request");
        Box::pin(async move { Ok(response) })
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

#[test]
fn catalog_card_quotes_and_runs_image_only_removal_through_the_real_adapter() {
    let model = "fal.bria-rmbg";
    let input = png(255);
    let output = png(0);
    let reply = |value| HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() };
    let http = Arc::new(Canned {
        replies: Mutex::new(VecDeque::from([
            reply(json!({"request_id":"r_749","status":"IN_QUEUE",
            "status_url":"https://queue.fal.run/fal-ai/bria/requests/r_749/status",
            "response_url":"https://queue.fal.run/fal-ai/bria/requests/r_749",
            "cancel_url":"https://queue.fal.run/fal-ai/bria/requests/r_749/cancel"})),
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"image":{"url":"https://v3.fal.media/files/public/cutout749.png",
            "content_type":"image/png","file_size":output.len(),"width":2,"height":2}})),
            HttpResponse { status: 200, headers: vec![], body: output.clone() },
        ])),
        requests: Mutex::default(),
    });
    let provider: Arc<dyn Provider> = Arc::new(nodecraft_providers::fal::Fal::new(Secret::new("fixture-key-not-real").unwrap(), http.clone()));
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::builtin().unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    // Generic generation cards retain their local prompt across model switches. A mode
    // without a prompt role must never send it to this differently priced endpoint.
    let params = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("retained local prompt".into()))].into_iter().collect();
    let card = kinds.get(KIND).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let media_ports: Vec<_> = card.inputs.iter().filter(|port| port.name != "prompt").collect();
    let [port] = media_ports.as_slice() else { panic!("one image input") };
    assert_eq!(port.name, "image");
    let mut project = Project::new(ProjectId::from_bytes([43; 16]), "Public background removal").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "offline-background-removal".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy::NEVER);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(provider).unwrap();
    routing.select(CARD, binding.target().adapter(), binding.target()).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    s.set_provider_routing(routing).unwrap();
    host.install(&mut s);
    let hash = s.import_asset(&input).unwrap();
    let source = s
        .kinds()
        .get("source.image")
        .unwrap()
        .instantiate_instance(
            SOURCE,
            Default::default(),
            &[("asset".into(), Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() }))].into(),
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
    assert_eq!(q["totals"]["USD"]["micros"], 18_000, "the image-only catalog estimate is $0.018");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    request["accept"] = json!({"token":q["token"],"amounts":amounts,"partial":false,"unpriced_jobs":0});
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
    assert_ne!(output.hash, hash, "the completed cutout differs from the opaque source");
    assert_eq!(output.mime.as_str(), "image/png");
    let metrics = nodecraft_format::asset_source::StreamMetrics::default();
    let stored = s.assets().open_verified(output, &Default::default(), &metrics).unwrap().read_small(4096, &metrics).unwrap();
    assert_eq!(stored, png(0), "the exact transparent PNG was imported into generation history");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 4, "one create, status, result and credential-free download");
    assert_eq!(requests[0].url, "https://queue.fal.run/fal-ai/bria/background/remove");
    assert_eq!(requests[0].method, nodecraft_providers::HttpMethod::Post);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap(),
        json!({"image_url":format!("data:image/png;base64,{}", base64(&input)),"sync_mode":false})
    );
    assert_eq!(requests.iter().filter(|request| request.method == nodecraft_providers::HttpMethod::Post).count(), 1);
    for request in requests.iter().take(3) {
        assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value == "Key fixture-key-not-real"));
    }
    assert!(requests[3].headers.is_empty());
    assert!(http.replies.lock().unwrap().is_empty());
}
