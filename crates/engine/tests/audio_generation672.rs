//! The Generate audio card (#672) through the engine: on the shipped catalog it offers fal's
//! Kokoro, and a card with a prompt is quoted, run through the real fal adapter over a scripted
//! transport, and leaves one sound that the card's audio body plays. No network.
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
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::{AUDIO_KIND, KIND, VIDEO_KIND};
use nodecraft_providers::{BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, Modality, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([72; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const TONE: &[u8] = include_bytes!("../../providers/src/fal/fixtures/tone.wav");

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

/// Answers one Kokoro queue cycle and keeps every request.
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
        assert_eq!(r.method, if n == 0 { HttpMethod::Post } else { HttpMethod::Get });
        requests.push(r);
        let root = "https://queue.fal.run/fal-ai/kokoro/requests/id1";
        let body = match n {
            0 => {
                json!({"request_id":"id1","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})
            }
            1 => json!({"status":"COMPLETED"}),
            2 => json!({"audio":{"url":"https://fal.media/fixture","content_type":"audio/wav"}}),
            _ => return Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: TONE.to_vec() }) }),
        };
        let body = serde_json::to_vec(&body).unwrap();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

#[test]
fn the_shipped_catalog_offers_kokoro_on_the_audio_card_only() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    assert!(snapshot.model(AUDIO_KIND, "fal.kokoro").is_some(), "Kokoro is an audio model");
    for kind in [KIND, VIDEO_KIND] {
        assert!(snapshot.model(kind, "fal.kokoro").is_none(), "Kokoro makes no {kind}");
    }
    let kinds = snapshot.registry().unwrap();
    let card = kinds.get(AUDIO_KIND).expect("a Generate audio kind");
    assert_eq!(card.descriptor().label, "Generate audio");
    assert!(card.descriptor().outputs.iter().all(|p| p.port_type.modality == Modality::Audio));
}

#[test]
fn a_prompt_runs_through_kokoro_and_leaves_one_sound() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([72; 16]), "Speech").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("fal.kokoro".into())), ("prompt".into(), Value::Text("Hello from a paper boat".into()))].into();
    let card = kinds.get(AUDIO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "speech".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let transport = Arc::new(Transport { requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), transport.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);

    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_ne!(q["complete"], true, "Kokoro bills by usage, so the quote is never complete: {q}");
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":true,"unpriced_jobs":1,"amounts":{}});
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
    assert!(matches!(result.outputs.as_slice(), [Value::Media(a)] if a.mime.essence() == "audio/wav"), "{:?}", result.outputs);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 4, "submit, status, result, download");
    assert_eq!(requests[0].url, format!("https://queue.fal.run/{}", nodecraft_providers::fal::KOKORO));
    let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["prompt"], "Hello from a paper boat");
}
