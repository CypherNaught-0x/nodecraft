//! fal's Wan 2.2 image and speech to video (#607) through the engine: a Generate video card on the
//! shipped catalog, with a picture and a speech track wired into its ports, is quoted, run through
//! the real fal adapter over a scripted transport, and leaves one clip. The submitted body carries
//! each wired input in its own field. No network.
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
use nodecraft_providers::{BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([67; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const FIRST: NodeId = NodeId::from_bytes([2; 16]);
const LAST: NodeId = NodeId::from_bytes([3; 16]);
const SPEECH: NodeId = NodeId::from_bytes([4; 16]);
const TONE: &[u8] = include_bytes!("../../providers/src/fal/fixtures/tone.wav");
const CLIP: &[u8] = include_bytes!("../../providers/src/fal/fixtures/blue.mp4");

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

/// Answers one queue cycle for `model` and keeps every request.
struct Transport {
    model: &'static str,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let mut requests = self.requests.lock().unwrap();
        let n = requests.len();
        assert_eq!(r.method, if n == 0 { HttpMethod::Post } else { HttpMethod::Get });
        requests.push(r);
        let root = "https://queue.fal.run/fal-ai/wan/requests/id1";
        let body = match n {
            0 => {
                json!({"request_id":"id1","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})
            }
            1 => json!({"status":"COMPLETED"}),
            2 => json!({"video":{"url":"https://fal.media/fixture","content_type":"application/octet-stream"}}),
            _ => return Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: CLIP.to_vec() }) }),
        };
        let body = serde_json::to_vec(&body).unwrap();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    transport: Arc<Transport>,
}

/// A Generate video card on `model`, with two pictures and a speech track imported, nothing wired.
fn fixture(model: &'static str, remote: &'static str) -> Fixture {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([67; 16]), "Wan").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("A paper boat on a pond".into()))].into();
    let card = kinds.get(VIDEO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "wan".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let transport = Arc::new(Transport { model: remote, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), transport.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let (first, last) = (png(90), png(200));
    let files = [Incoming { name: "first.png", bytes: &first }, Incoming { name: "last.png", bytes: &last }, Incoming { name: "speech.wav", bytes: TONE }];
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &files, &[FIRST, LAST, SPEECH]).unwrap();
    Fixture { session, host, transport }
}

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[shade; 4 * 4 * 4], &Default::default()).unwrap()
}

fn wire(f: &mut Fixture, n: u8, from: NodeId, port: &str) {
    let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": CARD, "port": port}, "order": 0});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
}

/// Quotes (fal bills by usage, so the quote is incomplete), accepts, runs, and returns the body
/// fal received.
fn run(f: &mut Fixture) -> serde_json::Value {
    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_ne!(q["complete"], true, "an unknown price is never shown as complete: {q}");
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":true,"unpriced_jobs":1,"amounts":{}});
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
    assert_eq!(requests.len(), 4, "submit, status, result, download");
    assert_eq!(requests[0].url, format!("https://queue.fal.run/{}", f.transport.model));
    serde_json::from_slice(&requests[0].body).unwrap()
}

/// The base64 payload of a data URI field, checked to be `len` bytes of `mime`.
fn data<'a>(body: &'a serde_json::Value, field: &str, mime: &str, len: usize) -> &'a str {
    let uri = body[field].as_str().unwrap_or_else(|| panic!("{field} missing"));
    let payload = uri.strip_prefix(&format!("data:{mime};base64,")).unwrap_or_else(|| panic!("{field} is not {mime}"));
    assert_eq!(payload.len(), len.div_ceil(3) * 4, "{field} carries the whole input");
    payload
}

#[test]
fn the_shipped_catalog_offers_both_new_wan_models_on_the_video_card() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    for id in ["fal.wan22-i2v", "fal.wan22-s2v"] {
        assert!(snapshot.model(VIDEO_KIND, id).is_some(), "{id} is a video model");
        assert!(snapshot.model(nodecraft_nodes::generation::KIND, id).is_none(), "{id} is not an image model");
    }
}

#[test]
fn first_and_last_frames_reach_their_own_fields() {
    let mut f = fixture("fal.wan22-i2v", nodecraft_providers::fal::WAN_I2V);
    wire(&mut f, 10, FIRST, "first_frame");
    wire(&mut f, 11, LAST, "last_frame");
    let body = run(&mut f);
    assert_eq!(body["prompt"], "A paper boat on a pond");
    let (first, last) = (png(90), png(200));
    let (sent_first, sent_last) = (data(&body, "image_url", "image/png", first.len()), data(&body, "end_image_url", "image/png", last.len()));
    assert_ne!(sent_first, sent_last, "two different frames");
    assert_eq!((body["resolution"].as_str(), body["aspect_ratio"].as_str()), (Some("480p"), Some("auto")));
}

#[test]
fn a_portrait_and_speech_reach_their_own_fields() {
    let mut f = fixture("fal.wan22-s2v", nodecraft_providers::fal::WAN_S2V);
    wire(&mut f, 10, FIRST, "portrait");
    wire(&mut f, 11, SPEECH, "speech");
    let body = run(&mut f);
    data(&body, "audio_url", "audio/wav", TONE.len());
    data(&body, "image_url", "image/png", png(90).len());
    assert!(body.get("image_url").is_some() && body.get("end_image_url").is_none());
    assert_eq!(body["num_frames"], 80);
}
