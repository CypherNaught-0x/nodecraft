//! The actual paid host, source image, streamed history and fal queue; no live calls.
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
    cost::Charge,
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Unsigned, Value};
use serde_json::json;
use std::{
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([37; 16]);
const CARD: NodeId = NodeId::from_bytes([38; 16]);
const IMAGE: NodeId = NodeId::from_bytes([39; 16]);
struct Wire {
    lost: bool,
    gated: AtomicBool,
    png: Arc<[u8]>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = request.url.clone();
        let cap = request.max_response_bytes;
        self.requests.lock().unwrap().push(request);
        Box::pin(async move {
            if self.lost {
                return Err(ProviderError::new(ErrorKind::Network, "lost paid create reply"));
            }
            let body = if url == "https://queue.fal.run/topaz/upscale/image/precision" {
                serde_json::to_vec(&json!({"request_id":"original-topaz37", "status":"IN_QUEUE",
                    "status_url":"https://queue.fal.run/topaz/upscale/requests/original-topaz37/status",
                    "response_url":"https://queue.fal.run/topaz/upscale/requests/original-topaz37",
                    "cancel_url":"https://queue.fal.run/topaz/upscale/requests/original-topaz37/cancel"}))
                .unwrap()
            } else if url.ends_with("/status") {
                if self.gated.load(Ordering::SeqCst) { br#"{"status":"IN_PROGRESS"}"#.to_vec() } else { br#"{"status":"COMPLETED"}"#.to_vec() }
            } else if url.ends_with("original-topaz37") {
                serde_json::to_vec(&json!({"image":{"url":"https://v3.fal.media/files/original/topaz37.png","content_type":"image/png"}})).unwrap()
            } else {
                assert_eq!(url, "https://v3.fal.media/files/original/topaz37.png");
                self.png.to_vec()
            };
            assert!(body.len() <= cap, "the real HTTP cap must admit the complete original result");
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
fn original_png(width: u32, height: u32, noise: bool) -> Vec<u8> {
    let mut rgba = vec![0; width as usize * height as usize * 4];
    let mut state = 0x37cafe12u32;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        for channel in pixel.iter_mut().take(3) {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *channel = if noise { state as u8 } else { 100 };
        }
        pixel[3] = 255;
    }
    nodecraft_media::encode_png(width, height, &rgba, &Default::default()).unwrap()
}
fn empty_project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([37; 16]), "Original Topaz fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Upscale") }.apply(&mut project).unwrap();
    project
}
fn run(lost: bool, streaming: bool, large: bool, hold_then_retry: bool) {
    let dir = tempfile::tempdir().unwrap();
    let generation = GenerationCatalog::builtin().unwrap();
    let card = generation
        .registry()
        .unwrap()
        .get(KIND)
        .unwrap()
        .instantiate_instance(
            CARD,
            Default::default(),
            &[
                ("model".into(), Value::Text("fal.topaz-precision".into())),
                ("prompt".into(), Value::Text("Retained text must not condition a pixel upscale".into())),
                ("upscale_factor".into(), Value::Unsigned(Unsigned(if large { 4 } else { 2 }))),
            ]
            .into(),
        )
        .unwrap();
    let mut project = empty_project();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let snapshot = nodecraft_format::Snapshot { project, assets: Default::default(), epoch: 0 };
    let mut session = if streaming {
        Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap())).unwrap()
    } else {
        Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap()
    };
    assert_eq!(session.assets().context().supports_large_assets(), streaming);
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "topaz37".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    // Native/classic constructors already install the catalog paired with their own kinds.
    session = session.with_services(services);
    let (width, height) = if large { (4096, 2048) } else { (64, 48) };
    let png: Arc<[u8]> = original_png(width, height, large).into();
    if large {
        assert!(png.len() > 16 * 1024 * 1024, "actually exercise the former download/storage boundary");
    }
    let wire = Arc::new(Wire { lost, gated: AtomicBool::new(hold_then_retry), png: png.clone(), requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), wire.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let factor = if large { 4 } else { 2 };
    let input = original_png(width / factor, height / factor, false);
    assert!(input.len() <= 1024 * 1024);
    import_files(&mut session, C, Position { x: -300., y: 0. }, 200., &[Incoming { name: "original.png", bytes: &input }], &[IMAGE]).unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([40;16]),"from":{"node":IMAGE,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}}),
        )
        .unwrap();
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    if !streaming {
        let error = session.dispatch("run.quote", &request).unwrap_err();
        assert!(error.message.contains("Topaz Precision needs a desktop project"), "{error}");
        assert!(wire.requests.lock().unwrap().is_empty());
        assert!(budget.log.lock().unwrap().entries().next().is_none());
        return;
    }
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true);
    assert_eq!(quote["totals"]["USD"]["micros"], 80_000);
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(wire.requests.lock().unwrap().is_empty());
    request["accept"] = json!({"token":quote["token"],"amounts":{"USD":80_000},"partial":false,"unpriced_jobs":0});
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut notices = vec![];
    if hold_then_retry {
        while !wire.requests.lock().unwrap().iter().any(|request| request.method == HttpMethod::Post) {
            notices.extend(host.pump(&mut session));
            assert!(Instant::now() < deadline, "create never reached mock");
            std::thread::sleep(Duration::from_millis(1));
        }
        session.dispatch("node.remove", &json!({"canvas":C,"id":CARD})).unwrap();
        wire.gated.store(false, Ordering::SeqCst);
    }
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    if hold_then_retry {
        let held = host.held_results();
        let [view] = held.as_slice() else { panic!("whole result must stay held: {notices:?}") };
        assert!(view.retry);
        assert!(notices.iter().any(|notice| matches!(notice,RunNotice::Failed{reason,..} if reason.contains("paid result is held"))));
        session.dispatch("edit.undo", &json!({})).unwrap();
        session.dispatch("run.held_retry", &json!({"id":view.id})).unwrap();
        while host.held_results().iter().any(|item| item.pending.is_some()) {
            notices.extend(host.pump(&mut session));
            assert!(Instant::now() < deadline, "held retry never finished");
        }
        assert!(host.held_results().is_empty(), "retry records without another request");
    }
    let node = &session.document().canvases()[&C].nodes()[&CARD];
    if lost {
        assert!(notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. })), "{notices:?}");
        assert!(node.history.is_empty());
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let [Value::List(images)] = node.history.last().unwrap().outputs.as_slice() else { panic!("image list") };
        let [Value::Media(asset)] = images.as_slice() else { panic!("one image") };
        let asset = asset.clone();
        assert_eq!(asset.mime.essence(), "image/png");
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let read = |s: &Session| {
            let lease = s.assets().open_verified(&asset, &Default::default(), &metrics).unwrap();
            let mut bytes = Vec::new();
            lease.reader(metrics.clone()).take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            assert!(bytes.len() <= 64 * 1024 * 1024);
            bytes
        };
        assert_eq!(read(&session).as_slice(), png.as_ref());
        let info = nodecraft_media::probe_image(&png, &Default::default()).unwrap();
        assert_eq!((info.width, info.height), (width, height));
        let path = dir.path().join("result.ncraft");
        session.dispatch("file.save_as", &json!({"location":path})).unwrap();
        let expected = session.document().clone();
        drop(session); // Reopen follows an actual close; retain the native single-writer lock.
        let mut reopened = Session::with_storage(
            nodecraft_format::Snapshot { project: empty_project(), assets: Default::default(), epoch: 0 },
            Box::new(nodecraft_format::NativeStorage::new(dir.path().join("reopen-recovery")).unwrap()),
        )
        .unwrap();
        reopened.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
        assert_eq!(reopened.document(), &expected);
        assert_eq!(read(&reopened).as_slice(), png.as_ref());
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost.micros, 80_000);
    assert_eq!(entries[0].status, if lost { Charge::Unresolved } else { Charge::Actual });
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Post).count(), 1, "uncertain create never replays");
    if !hold_then_retry {
        assert_eq!(requests.len(), if lost { 1 } else { 4 });
    }
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "Standard V2");
    assert_eq!(body["upscale_factor"], factor);
    assert_eq!(body["crop_to_fill"], false);
    assert_eq!(body["output_format"], "png");
    assert_eq!(body["enable_safety_checker"], true);
    assert!(body["image_url"].as_str().unwrap().starts_with("data:image/png;base64,"));
    assert!(body.get("prompt").is_none());
    assert!(body.get("seed").is_none());
    assert!(body.get("num_images").is_none());
    if !lost {
        assert!(requests.iter().find(|request| request.url.starts_with("https://v3.fal.media/")).unwrap().headers.is_empty(), "no credential sent to storage");
    }
}
#[test]
fn topaz37_native_large_png_survives_actual_run_history_and_indexed_save_reopen() {
    run(false, true, true, false);
}
#[test]
fn topaz37_lost_paid_reply_retains_reservation_and_never_replays() {
    run(true, true, false, false);
}
#[test]
fn topaz37_classic_browser_and_default_storage_refuse_before_any_paid_http() {
    run(false, false, false, false);
}

#[test]
fn topaz37_held_large_png_retry_preserves_original_without_paying_again() {
    run(false, true, true, true);
}
