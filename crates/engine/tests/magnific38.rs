//! Real Magnific adapter, RunHost consent and exact original persistence. Entirely offline.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::Services,
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
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, magnific::Magnific, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([38; 16]);
const CARD: NodeId = NodeId::from_bytes([39; 16]);
const IMAGE: NodeId = NodeId::from_bytes([40; 16]);
const TASK: &str = "8fcd0038-ea18-4f33-a234-38c3bbbeeeee";
const ENDPOINT: &str = "https://api.magnific.com/v1/ai/image-upscaler-precision-v2";
const OUTPUT: &str = "https://ai-statics.freepik.com/public-magnific38.png";

struct Wire {
    lost: bool,
    output: Arc<[u8]>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let method = request.method;
        let url = request.url.clone();
        let cap = request.max_response_bytes;
        self.requests.lock().unwrap().push(request);
        Box::pin(async move {
            let body = if method == HttpMethod::Post {
                assert_eq!(url, ENDPOINT);
                if self.lost {
                    return Err(ProviderError::new(ErrorKind::Network, "lost paid submit reply"));
                }
                serde_json::to_vec(&json!({"data":{"task_id":TASK,"status":"CREATED","generated":[]}})).unwrap()
            } else if url == format!("{ENDPOINT}/{TASK}") {
                serde_json::to_vec(&json!({"data":{"task_id":TASK,"status":"COMPLETED","generated":[OUTPUT]}})).unwrap()
            } else {
                assert_eq!(url, OUTPUT);
                self.output.to_vec()
            };
            assert!(body.len() <= cap, "actual HTTP cap admits the complete original");
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
        let (sender, receiver) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = sender.send(());
        });
        Box::pin(async move {
            let _ = receiver.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
fn image(width: u32, height: u32, noise: bool) -> Vec<u8> {
    let mut rgba = vec![0; width as usize * height as usize * 4];
    let mut random = 0x38cafe12u32;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        for channel in pixel.iter_mut().take(3) {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *channel = if noise { random as u8 } else { 120 };
        }
        pixel[3] = 255;
    }
    nodecraft_media::encode_png(width, height, &rgba, &Default::default()).unwrap()
}
fn empty_project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([38; 16]), "Public Magnific fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Enhance") }.apply(&mut project).unwrap();
    project
}
fn run(lost: bool, streaming: bool, large: bool) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = GenerationCatalog::builtin().unwrap();
    let card = catalog
        .registry()
        .unwrap()
        .get(KIND)
        .unwrap()
        .instantiate_instance(
            CARD,
            Default::default(),
            &[
                ("model".into(), Value::Text("magnific.precision-v2".into())),
                ("prompt".into(), Value::Text("Retained text must not guide an image-only upscale".into())),
                ("scale_factor".into(), Value::Text(if large { "4" } else { "2" }.into())),
            ]
            .into(),
        )
        .unwrap();
    let mut project = empty_project();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let snapshot = nodecraft_format::Snapshot { project, assets: Default::default(), epoch: 0 };
    let mut session = if streaming {
        Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(directory.path().join("recovery")).unwrap())).unwrap()
    } else {
        Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap()
    };
    assert_eq!(session.assets().context().supports_large_assets(), streaming);
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "magnific38".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    session = session.with_services(services);
    let (width, height) = if large { (4096, 2048) } else { (64, 48) };
    let original: Arc<[u8]> = image(width, height, large).into();
    if large {
        assert!(original.len() > 16 * 1024 * 1024, "exercise actual large-asset storage, not a small stand-in");
    }
    let wire = Arc::new(Wire { lost, output: original.clone(), requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Magnific::new(Secret::new("public-magnific38-fixture").unwrap(), wire.clone()))).unwrap();
    routing.select(CARD, "magnific", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let factor = if large { 4 } else { 2 };
    let input = image(width / factor, height / factor, false);
    assert!(input.len() <= 1024 * 1024);
    import_files(&mut session, C, Position { x: -300., y: 0. }, 200., &[Incoming { name: "original.png", bytes: &input }], &[IMAGE]).unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([41;16]),"from":{"node":IMAGE,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}}),
        )
        .unwrap();
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    if !streaming {
        let error = session.dispatch("run.quote", &request).unwrap_err();
        assert!(error.message.contains("Magnific") && error.message.contains("desktop"), "{error}");
        assert!(wire.requests.lock().unwrap().is_empty());
        return;
    }
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], false, "credit usage must never become a fabricated currency amount");
    assert!(quote["totals"].as_object().unwrap().is_empty());
    assert!(session.dispatch("run.start", &request).is_err());
    request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":true,"unpriced_jobs":0});
    assert!(session.dispatch("run.start", &request).is_err(), "one unknown-price job needs explicit acceptance");
    assert!(wire.requests.lock().unwrap().is_empty());
    request["accept"]["unpriced_jobs"] = json!(1);
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    let node = &session.document().canvases()[&C].nodes()[&CARD];
    if lost {
        assert!(notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. })), "{notices:?}");
        assert!(node.history.is_empty());
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let [Value::List(outputs)] = node.history.last().unwrap().outputs.as_slice() else { panic!("one image list") };
        let [Value::Media(asset)] = outputs.as_slice() else { panic!("one original image") };
        let asset = asset.clone();
        assert_eq!(asset.mime.essence(), "image/png");
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let read = |session: &Session| {
            let lease = session.assets().open_verified(&asset, &Default::default(), &metrics).unwrap();
            let mut bytes = Vec::new();
            lease.reader(metrics.clone()).take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            assert!(bytes.len() <= 256 * 1024 * 1024);
            bytes
        };
        assert_eq!(read(&session).as_slice(), original.as_ref());
        let path = directory.path().join("result.ncraft");
        session.dispatch("file.save_as", &json!({"location":path})).unwrap();
        let expected = session.document().clone();
        drop(session);
        let mut reopened = Session::with_storage(
            nodecraft_format::Snapshot { project: empty_project(), assets: Default::default(), epoch: 0 },
            Box::new(nodecraft_format::NativeStorage::new(directory.path().join("reopened-recovery")).unwrap()),
        )
        .unwrap();
        reopened.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
        assert_eq!(reopened.document(), &expected);
        assert_eq!(read(&reopened).as_slice(), original.as_ref());
    }
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "unpriced usage has no invented monetary invoice");
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|request| request.method == HttpMethod::Post).count(), 1, "uncertain paid creates never replay");
    assert_eq!(requests.len(), if lost { 1 } else { 3 });
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["scale_factor"], factor);
    assert_eq!(body["flavor"], "photo");
    assert_eq!(body["sharpen"], 7);
    assert_eq!(body["smart_grain"], 7);
    assert_eq!(body["ultra_detail"], 30);
    assert_eq!(body["filter_nsfw"], true);
    assert!(!body["image"].as_str().unwrap().starts_with("data:"), "the native API takes raw base64");
    for absent in ["prompt", "seed", "n", "output_format", "webhook_url"] {
        assert!(body.get(absent).is_none());
    }
    if !lost {
        assert!(requests.last().unwrap().headers.is_empty(), "no key reaches the output host");
    }
}

#[test]
fn magnific38_real_runhost_unknown_consent_preserves_the_original_across_save_reopen() {
    run(false, true, false);
}
#[test]
fn magnific38_lost_paid_reply_never_replays_in_the_actual_host() {
    run(true, true, false);
}

// Large-result and Classic refusal fixtures are enabled with the shared Topaz worker/guard
// integration, not a second implementation of those storage boundaries.
