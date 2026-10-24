//! Higgsfield Genjutsu source video edits (#179) through the engine: a Generate video card on the
//! shipped catalog, with an imported MP4 and a reference picture wired in, is quoted per second of
//! its source before anything is paid, refuses a source the model won't take, and runs through
//! the real Higgsfield adapter over a scripted transport. No network.
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
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::VIDEO_KIND;
use nodecraft_providers::{BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, higgsfield::Higgsfield, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([79; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const PICTURE: NodeId = NodeId::from_bytes([3; 16]);
const SHORT: NodeId = NodeId::from_bytes([4; 16]);
/// Five seconds of 640×640, made for this test.
const FIVE_SECONDS: &[u8] = include_bytes!("../../providers/src/higgsfield/fixtures/source5s.mp4");
/// One second of 16×16.
const ONE_SECOND: &[u8] = include_bytes!("../../providers/src/fal/fixtures/blue.mp4");
const REQUEST: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms.min(5)));
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

/// Answers Higgsfield's upload grants, uploads, submit, status and result download, and keeps
/// every request.
#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let mut requests = self.requests.lock().unwrap();
        let n = requests.len();
        let reply = if r.url.ends_with("/files/generate-upload-url") {
            let mime: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            let mime = mime["content_type"].as_str().unwrap().to_owned();
            json!({"public_url": format!("https://upload.cdn.example.com/inputs/{n}"), "upload_url": "https://upload.storage.example.com/put", "content_type": mime})
        } else if r.method == HttpMethod::Put {
            json!({})
        } else if r.url.ends_with("/motion-transfer/v1.0") {
            json!({"status": "queued", "request_id": REQUEST})
        } else if r.url.ends_with("/status") {
            json!({"request_id": REQUEST, "status": "completed", "video": {"url": "https://cdn.example.com/out.mp4"}})
        } else if r.url == "https://cdn.example.com/out.mp4" {
            requests.push(r);
            return Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: FIVE_SECONDS.to_vec() }) });
        } else {
            panic!("unexpected request {}", r.url)
        };
        requests.push(r);
        let body = serde_json::to_vec(&reply).unwrap();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    transport: Arc<Transport>,
}

/// A Generate video card on Genjutsu motion transfer, with a five second clip, a one second clip
/// and a picture imported, nothing wired.
fn fixture(resolution: &str) -> Fixture {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([79; 16]), "Genjutsu").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("higgsfield.genjutsu-motion-transfer".into())), ("resolution".into(), Value::Text(resolution.into()))].into();
    let card = kinds.get(VIDEO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "genjutsu".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let transport = Arc::new(Transport::default());
    let mut routing = ProviderRouting::new();
    let key = Secret::new("fixture-key-id-0001:fixture-secret-not-real-0001").unwrap();
    let binding = routing.bind_default(Arc::new(Higgsfield::new(key, transport.clone()))).unwrap();
    routing.select(CARD, "higgsfield", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let picture = nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap();
    let files = [
        Incoming { name: "dance.mp4", bytes: FIVE_SECONDS },
        Incoming { name: "subject.png", bytes: &picture },
        Incoming { name: "short.mp4", bytes: ONE_SECOND },
    ];
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &files, &[SOURCE, PICTURE, SHORT]).unwrap();
    Fixture { session, host, transport }
}

fn wire(f: &mut Fixture, n: u8, from: NodeId, port: &str) {
    let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": CARD, "port": port}, "order": 0});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
}
fn request() -> serde_json::Value {
    json!({"canvas": C, "mode": "selected", "nodes": [CARD]})
}

#[test]
fn an_imported_clip_carries_its_length_and_frame_size() {
    let f = fixture("720p");
    let canvas = f.session.document().canvases().get(&C).unwrap();
    let meta = |node: NodeId| match canvas.nodes().get(&node).unwrap().params.get("asset") {
        Some(Value::Media(asset)) => asset.meta.clone(),
        other => panic!("{other:?}"),
    };
    let five = meta(SOURCE);
    assert_eq!((five.duration_ms, five.width, five.height), (Some(5_000), Some(640), Some(640)));
    assert_eq!(meta(SHORT).duration_ms, Some(1_000));
}

#[test]
fn the_quote_counts_each_second_of_the_source_at_the_chosen_resolution_then_the_edit_runs() {
    let mut f = fixture("1080p");
    wire(&mut f, 10, SOURCE, "source");
    wire(&mut f, 11, PICTURE, "image");
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!((q["allowed"].as_bool(), q["complete"].as_bool()), (Some(true), Some(true)), "{q}");
    // Five seconds at 1.632 USD.
    assert_eq!(q["totals"]["USD"]["micros"], 8_160_000, "{q}");
    let mut start = request();
    start["accept"] = json!({"token": q["token"], "partial": false, "unpriced_jobs": 0, "amounts": {"USD": 8_160_000}});
    f.session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let card = f.session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    let result = card.history.iter().find(|g| Some(g.id) == card.active).expect("an active result");
    assert!(matches!(result.outputs.as_slice(), [Value::Media(a)] if a.mime.essence() == "video/mp4"), "{:?}", result.outputs);
    let requests = f.transport.requests.lock().unwrap();
    let submit = requests.iter().find(|r| r.url.ends_with("/motion-transfer/v1.0")).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&submit.body).unwrap();
    // The source goes up first, as an MP4, then the picture; the card's empty prompt is left out.
    assert_eq!(body["video_url"], "https://upload.cdn.example.com/inputs/0");
    assert_eq!(body["image_urls"], json!(["https://upload.cdn.example.com/inputs/2"]));
    assert_eq!(body["resolution"], "1080p");
    assert!(body.get("prompt").is_none(), "{body}");
    let put = requests.iter().find(|r| r.method == HttpMethod::Put).unwrap();
    assert_eq!(put.body, FIVE_SECONDS);
}

#[test]
fn a_source_too_short_for_the_model_is_refused_before_anything_is_sent() {
    let mut f = fixture("720p");
    wire(&mut f, 10, SHORT, "source");
    wire(&mut f, 11, PICTURE, "image");
    // The planner's reason is a fixed code here; the card names the length (see the nodes tests).
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    assert!(q["blocked"][0].as_str().unwrap().contains("[node_validation]"), "{q}");
    assert!(f.transport.requests.lock().unwrap().is_empty());
}

#[test]
fn a_source_of_unknown_length_leaves_the_step_unpriced_with_its_reason() {
    let mut f = fixture("720p");
    // An MP4 whose headers state no length: its quote can't be known before the run.
    let mut headless = vec![0, 0, 0, 0x18];
    headless.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
    headless.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    const UNKNOWN: NodeId = NodeId::from_bytes([5; 16]);
    import_files(&mut f.session, C, Position { x: -300.0, y: 400.0 }, 200.0, &[Incoming { name: "unknown.mp4", bytes: &headless }], &[UNKNOWN]).unwrap();
    wire(&mut f, 10, UNKNOWN, "source");
    wire(&mut f, 11, PICTURE, "image");
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!((q["allowed"].as_bool(), q["complete"].as_bool()), (Some(true), Some(false)), "{q}");
    let why = q["steps"][0]["unpriced"].as_str().unwrap();
    assert!(why.contains("priced per second of its source video, whose length isn't known yet"), "{why}");
}
