//! LTX Pro through the shipping catalog, unknown-price consent, frame resolver and fal queue. Offline.
#![cfg(not(target_arch = "wasm32"))]
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
use nodecraft_nodes::generation::VIDEO_KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const C: CanvasId = CanvasId::from_bytes([30; 16]);
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
            let body = if url.ends_with("/text-to-video") || url.ends_with("/image-to-video") {
                serde_json::to_vec(&json!({"request_id":"public-task-30", "status":"IN_QUEUE",
                    "status_url":"https://queue.fal.run/fal-ai/ltx-2.3/requests/public-task-30/status",
                    "response_url":"https://queue.fal.run/fal-ai/ltx-2.3/requests/public-task-30",
                    "cancel_url":"https://queue.fal.run/fal-ai/ltx-2.3/requests/public-task-30/cancel"}))
                .unwrap()
            } else if url.ends_with("/status") {
                serde_json::to_vec(&json!({"status":"COMPLETED"})).unwrap()
            } else if url.ends_with("public-task-30") {
                serde_json::to_vec(&json!({"video":{"url":"https://v3.fal.media/files/public/ltx.mp4"}})).unwrap()
            } else {
                assert_eq!(url, "https://v3.fal.media/files/public/ltx.mp4");
                include_bytes!("../../providers/src/fal/fixtures/blue.mp4").to_vec()
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
fn run(lost: bool, frames: bool) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let model = if frames { "fal.ltx23-i2v" } else { "fal.ltx23" };
    let kind = VIDEO_KIND;
    let params = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("A public harbour".into()))].into();
    let card = kinds.get(kind).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([30; 16]), "Public LTX fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "ltx30".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport { lost, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    if frames {
        let image = nodecraft_media::encode_png(4, 4, &[120; 4 * 4 * 4], &Default::default()).unwrap();
        let first = NodeId::from_bytes([2; 16]);
        let last = NodeId::from_bytes([3; 16]);
        import_files(
            &mut session,
            C,
            Position { x: -300.0, y: 0.0 },
            200.0,
            &[Incoming { name: "first.png", bytes: &image }, Incoming { name: "last.png", bytes: &image }],
            &[first, last],
        )
        .unwrap();
        for (n, source, role) in [(2, first, "first_frame"), (3, last, "last_frame")] {
            session
                .dispatch(
                    "edge.connect",
                    &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([n;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":role},"order":0}}),
                )
                .unwrap();
        }
    }
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_ne!(quote["complete"], true, "an uncertain rate is never a complete quote: {quote}");
    assert!(session.dispatch("run.start", &request).is_err(), "unknown-priced work requires explicit acceptance");
    assert!(http.requests.lock().unwrap().is_empty());
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
        let [Value::Media(asset)] = output.as_slice() else { panic!("one complete video: {output:?}") };
        assert_eq!(asset.mime.as_str(), "video/mp4");
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let bytes = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap();
        assert_eq!(bytes, include_bytes!("../../providers/src/fal/fixtures/blue.mp4").to_vec());
    }
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), if lost { 1 } else { 4 }, "uncertain paid submissions must not retry even with max_attempts=3");
    assert_eq!(requests.iter().filter(|request| request.method == nodecraft_providers::HttpMethod::Post).count(), 1);
    assert_eq!(requests[0].url, format!("https://queue.fal.run/fal-ai/ltx-2.3/{}", if frames { "image-to-video" } else { "text-to-video" }));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["duration"], 6);
    assert_eq!(body["fps"], 25);
    assert_eq!(body["generate_audio"], true);
    assert_eq!(body["image_url"].is_string(), frames);
    assert_eq!(body["end_image_url"].is_string(), frames);
    if !lost {
        assert!(requests[3].headers.is_empty(), "download never carries credentials");
    }
}
#[test]
fn ltx30_catalog_queue_download_records_video() {
    run(false, false);
}
#[test]
fn ltx30_lost_paid_reply_is_terminal_in_actual_host() {
    run(true, false);
}

#[test]
fn ltx30_actual_host_resolves_first_and_last_frames_into_their_own_fields() {
    run(false, true);
}
