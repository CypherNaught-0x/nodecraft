//! FLUX.1 Kontext editing through the shipping catalog and RunHost (#1060): a wired image reaches
//! BFL base64-encoded under `input_image`, the quote is the published 0.04 USD per image, and the
//! edited PNG (its size set by BFL) is recorded. Offline.
#![cfg(not(target_arch = "wasm32"))]
use base64::Engine as _;
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, bfl::Bfl, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([60; 16]);
const CARD: NodeId = NodeId::from_bytes([61; 16]);
const SOURCE: NodeId = NodeId::from_bytes([62; 16]);

fn png(width: u32, height: u32) -> Vec<u8> {
    nodecraft_media::encode_png(width, height, &vec![90; (width * height * 4) as usize], &nodecraft_media::Limits::default()).unwrap()
}

struct Transport(Mutex<Vec<HttpRequest>>);
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = r.url.clone();
        self.0.lock().unwrap().push(r);
        Box::pin(async move {
            let body = if url.ends_with("/v1/flux-kontext-pro") {
                serde_json::to_vec(&json!({"id":"public-task-1060","polling_url":"https://api.eu1.bfl.ai/v1/get_result?id=public-task-1060"})).unwrap()
            } else if url.contains("get_result") {
                serde_json::to_vec(&json!({"id":"public-task-1060","status":"Ready","result":{"sample":"https://delivery.eu1.bfl.ai/public.png"}})).unwrap()
            } else {
                assert_eq!(url, "https://delivery.eu1.bfl.ai/public.png");
                // BFL sets an edit's size: not the request's, which fixes none.
                png(96, 64)
            };
            Ok(HttpResponse { status: 200, headers: vec![], body })
        })
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
fn a_kontext_edit_sends_the_wired_image_and_records_the_result() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let params = [("model".into(), Value::Text("bfl.flux-kontext-pro".into())), ("prompt".into(), Value::Text("Make the sky orange".into()))].into();
    let card = kinds.get(KIND).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([60; 16]), "Public Kontext fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "bfl1060".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport(Mutex::default()));
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Bfl::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "bfl", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let input = png(32, 32);
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &[Incoming { name: "photo.png", bytes: &input }], &[SOURCE]).unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([63;16]),"from":{"node":SOURCE,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}}),
        )
        .unwrap();

    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!((quote["allowed"].clone(), quote["complete"].clone()), (json!(true), json!(true)), "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], 40_000, "the published 0.04 USD per image: {quote}");
    request["accept"] = json!({"token":quote["token"],"amounts":{"USD":quote["totals"]["USD"]["micros"]},"partial":false,"unpriced_jobs":0});
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = vec![];
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
    let requests = http.0.lock().unwrap();
    let create = requests.iter().find(|r| r.method == HttpMethod::Post).unwrap();
    assert_eq!(create.url, "https://api.bfl.ai/v1/flux-kontext-pro");
    let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(body["input_image"].as_str().unwrap()).unwrap(), input, "the wired image itself");
    assert_eq!(body["prompt"], "Make the sky orange");
    assert!(body.get("width").is_none() && body.get("height").is_none(), "an edit fixes no size: {body}");
}
