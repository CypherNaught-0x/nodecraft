//! Shipping catalog and RunHost with the direct BFL queue adapter. Offline.
#![cfg(not(target_arch = "wasm32"))]
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
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, ProviderError, bfl::Bfl, secrets::Secret};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const C: CanvasId = CanvasId::from_bytes([33; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
struct Transport {
    lost: bool,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = r.url.clone();
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            if self.lost {
                return Err(ProviderError::new(ErrorKind::Network, "lost paid reply"));
            }
            let body = if url.ends_with("flux-2-klein-4b") {
                serde_json::to_vec(&json!({"id":"public-task-33","polling_url":"https://api.eu1.bfl.ai/v1/get_result?id=public-task-33"})).unwrap()
            } else if url.contains("get_result") {
                serde_json::to_vec(&json!({"id":"public-task-33","status":"Ready","result":{"sample":"https://delivery.eu1.bfl.ai/public.png"}})).unwrap()
            } else {
                assert_eq!(url, "https://delivery.eu1.bfl.ai/public.png");
                nodecraft_media::encode_png(64, 64, &vec![128; 64 * 64 * 4], &nodecraft_media::Limits::default()).unwrap()
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
fn run(lost: bool) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let model = "bfl.flux-2-klein-4b";
    let kind = KIND;
    let params = [
        ("model".into(), Value::Text(model.into())),
        ("prompt".into(), Value::Text("A public harbour".into())),
        ("width".into(), Value::Unsigned(nodecraft_types::Unsigned(64))),
        ("height".into(), Value::Unsigned(nodecraft_types::Unsigned(64))),
    ]
    .into();
    let card = kinds.get(kind).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([33; 16]), "Public BFL fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "bfl33".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport { lost, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Bfl::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "bfl", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_ne!(quote["complete"], true, "{quote}");
    assert!(session.dispatch("run.start", &request).is_err(), "unknown cost requires explicit acceptance");
    request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":true,"unpriced_jobs":1});
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
    let node = session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    if lost {
        assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
        assert!(node.history.is_empty());
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let output = &node.history.last().unwrap().outputs;
        let [Value::List(images)] = output.as_slice() else { panic!("image generation must retain its list port: {output:?}") };
        assert!(matches!(images.as_slice(), [Value::Media(asset)] if asset.mime.as_str() == "image/png"));
    }
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), if lost { 1 } else { 3 }, "uncertain paid submissions must not retry even with max_attempts=3");
    assert_eq!(requests.iter().filter(|request| request.method == nodecraft_providers::HttpMethod::Post).count(), 1);
    if lost {
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "unknown money is never invented");
    }
}
#[test]
fn bfl33_catalog_queue_download_records_image() {
    run(false);
}
#[test]
fn bfl33_lost_paid_reply_is_terminal_in_actual_host() {
    run(true);
}
