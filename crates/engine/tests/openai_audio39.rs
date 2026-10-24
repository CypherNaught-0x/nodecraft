//! Shipping catalog, asset resolution and RunHost with the actual audio adapter. Offline.
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
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::{AUDIO_KIND, TEXT_KIND};
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, ProviderError, openai_audio::OpenAiAudio, secrets::Secret};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const C: CanvasId = CanvasId::from_bytes([39; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const WAV: &[u8] = include_bytes!("../../providers/src/fal/fixtures/tone.wav");
const MP3: &[u8] = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64\0\0\0\0";
#[derive(Clone, Copy, Debug)]
enum Reply {
    Success,
    Lost,
    RateLimitedThenSuccess,
    Refused(u16, &'static str),
}
struct Transport {
    reply: Reply,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let speech = r.url.ends_with("/speech");
        let attempt = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(r);
            requests.len()
        };
        Box::pin(async move {
            if matches!(self.reply, Reply::Lost) {
                Err(ProviderError::new(ErrorKind::Network, "lost paid reply"))
            } else if let Reply::Refused(status, code) = self.reply {
                Ok(HttpResponse { status, headers: vec![], body: serde_json::to_vec(&json!({"error":{"code":code}})).unwrap() })
            } else if matches!(self.reply, Reply::RateLimitedThenSuccess) && attempt == 1 {
                Ok(HttpResponse { status: 429, headers: vec![], body: br#"{"error":{"code":"rate_limit_exceeded"}}"#.to_vec() })
            } else {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: if speech { MP3.to_vec() } else { br#"{"text":"A paper boat.","usage":{"type":"duration","seconds":2}}"#.to_vec() },
                })
            }
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
fn run(speech: bool, reply: Reply) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let model = if speech { "openai-audio.mini-tts" } else { "openai-audio.transcribe" };
    let kind = if speech { AUDIO_KIND } else { TEXT_KIND };
    let mut params: nodecraft_types::ParamValues = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("A paper boat.".into()))].into();
    if speech {
        params.insert("instructions".into(), Value::Text("🌊".repeat(4096)));
    }
    let card = kinds.get(kind).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([39; 16]), "Public audio fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "audio39".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport { reply, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(OpenAiAudio::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "openai-audio", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    if !speech {
        let hash = session.import_asset(WAV).unwrap();
        let source = session
            .kinds()
            .get("source.audio")
            .unwrap()
            .instantiate_instance(
                SOURCE,
                Default::default(),
                &[("asset".into(), Value::Media(AssetRef { hash, mime: "audio/wav".parse().unwrap(), meta: MediaMeta::default() }))].into(),
            )
            .unwrap();
        session.dispatch("node.add", &json!({"canvas":C,"node":source})).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([3; 16]),
            from: Endpoint { node: SOURCE, port: "out".into() },
            to: Endpoint { node: CARD, port: "audio".into() },
            order: 0,
        };
        session.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    }
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
    let failed = matches!(reply, Reply::Lost | Reply::Refused(..));
    if failed {
        assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
        assert!(node.history.is_empty());
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let output = &node.history.last().unwrap().outputs;
        if speech {
            assert!(matches!(output.as_slice(),[Value::Media(a)] if a.mime.essence()=="audio/mpeg"));
        } else {
            assert!(matches!(output.as_slice(),[Value::Text(t)] if t=="A paper boat."));
        }
    }
    let requests = http.requests.lock().unwrap();
    let attempts = if matches!(reply, Reply::RateLimitedThenSuccess) { 2 } else { 1 };
    assert_eq!(requests.len(), attempts, "only a proven transient refusal retries under max_attempts=3: {reply:?}");
    if speech {
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["instructions"].as_str().unwrap(), "🌊".repeat(4096), "the catalog and adapter both accept 4096 Unicode characters");
    } else {
        assert!(requests[0].body.windows(WAV.len()).any(|w| w == WAV));
    }
    if failed {
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "unknown money is never invented");
    }
}
#[test]
fn openai_audio39_catalog_speech_records_audio() {
    run(true, Reply::Success);
}
#[test]
fn openai_audio39_catalog_transcription_resolves_audio_and_records_text() {
    run(false, Reply::Success);
}
#[test]
fn openai_audio39_lost_paid_reply_is_terminal_in_the_actual_host() {
    run(true, Reply::Lost);
    run(false, Reply::Lost);
}

#[test]
fn openai_audio39_proven_rate_refusal_retries_in_the_actual_host() {
    run(true, Reply::RateLimitedThenSuccess);
    run(false, Reply::RateLimitedThenSuccess);
}

#[test]
fn openai_audio39_auth_and_quota_refusals_do_not_retry_in_the_actual_host() {
    for (status, code) in [(401, "invalid_api_key"), (429, "insufficient_quota"), (429, "credit_balance_exhausted")] {
        run(true, Reply::Refused(status, code));
    }
}
