//! HeyGen uses actual unknown-cost consent, both media modes and native original MP4 storage. Offline only.
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
use nodecraft_nodes::generation::VIDEO_KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([29; 16]);
const CARD: NodeId = NodeId::from_bytes([38; 16]);
const IMAGE: NodeId = NodeId::from_bytes([39; 16]);
struct Wire {
    remote: String,
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
            let body = if url == format!("https://queue.fal.run/{}", self.remote) {
                serde_json::to_vec(&json!({"request_id":"original-heygen43", "status":"IN_QUEUE",
                    "status_url":"https://queue.fal.run/fal-ai/heygen/requests/original-heygen43/status",
                    "response_url":"https://queue.fal.run/fal-ai/heygen/requests/original-heygen43",
                    "cancel_url":"https://queue.fal.run/fal-ai/heygen/requests/original-heygen43/cancel"}))
                .unwrap()
            } else if url.ends_with("/status") {
                if self.gated.load(Ordering::SeqCst) { br#"{"status":"IN_PROGRESS"}"#.to_vec() } else { br#"{"status":"COMPLETED"}"#.to_vec() }
            } else if url.ends_with("original-heygen43") {
                serde_json::to_vec(&json!({"video":{"url":"https://v3.fal.media/files/original/pika29.mp4","content_type":"video/mp4"}})).unwrap()
            } else {
                assert_eq!(url, "https://v3.fal.media/files/original/pika29.mp4");
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
fn original_png(width: u32, height: u32) -> Vec<u8> {
    let rgba: Vec<u8> = (0..width * height).flat_map(|_| [36, 73, 96, 255]).collect();
    nodecraft_media::encode_png(width, height, &rgba, &Default::default()).unwrap()
}
fn speech_wav() -> Vec<u8> {
    let mut b = b"RIFF".to_vec();
    b.extend_from_slice(&16036u32.to_le_bytes());
    b.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    b.extend_from_slice(&8000u32.to_le_bytes());
    b.extend_from_slice(&16000u32.to_le_bytes());
    b.extend_from_slice(b"\x02\0\x10\0data");
    b.extend_from_slice(&16000u32.to_le_bytes());
    b.resize(16044, 0);
    b
}
fn empty_project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([29; 16]), "Original offline HeyGen client fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Animate") }.apply(&mut project).unwrap();
    project
}
fn run(model: &str, lost: bool, streaming: bool, large: bool, hold_then_retry: bool, resume: bool) {
    let dir = tempfile::tempdir().unwrap();
    let generation = GenerationCatalog::builtin().unwrap();
    let card = generation
        .registry()
        .unwrap()
        .get(VIDEO_KIND)
        .unwrap()
        .instantiate_instance(
            CARD,
            Default::default(),
            &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Retained card text is never sent to TTS".into()))].into(),
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
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "heygen43".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let journal = nodecraft_engine::run_journal::MemoryJournal::default();
    if resume {
        host.open_journal(Box::new(journal.clone())).unwrap();
    }
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    // Native/classic constructors already install the catalog paired with their own kinds.
    session = session.with_services(services);
    let mut video = include_bytes!("../../providers/src/fal/fixtures/pika29.mp4").to_vec();
    if large {
        let padding = 17 * 1024 * 1024usize;
        video.extend_from_slice(&u32::try_from(padding + 8).unwrap().to_be_bytes());
        video.extend_from_slice(b"free");
        video.resize(video.len() + padding, 0);
    }
    let png: Arc<[u8]> = video.into();
    if large {
        assert!(png.len() > 16 * 1024 * 1024, "actually exercise the former download/storage boundary");
    }
    let remote = nodecraft_catalog::Catalog::builtin().unwrap().get(model).unwrap().provider_model.clone();
    let wire = Arc::new(Wire { remote, lost, gated: AtomicBool::new(hold_then_retry || resume), png: png.clone(), requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), wire.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let photo = model == "fal.heygen-avatar4";
    let input = if photo { original_png(64, 48) } else { include_bytes!("../../providers/src/fal/fixtures/pika29.mp4").to_vec() };
    let speech = speech_wav();
    let speech_id = NodeId::from_bytes([42; 16]);
    assert!(input.len() <= 1024 * 1024);
    import_files(
        &mut session,
        C,
        Position { x: -300., y: 0. },
        200.,
        &[Incoming { name: if photo { "original.png" } else { "original.mp4" }, bytes: &input }, Incoming { name: "speech.wav", bytes: &speech }],
        &[IMAGE, speech_id],
    )
    .unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([40;16]),"from":{"node":IMAGE,"port":"out"},"to":{"node":CARD,"port":if photo {"portrait"} else {"video"}},"order":0}}),
        )
        .unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([43;16]),"from":{"node":speech_id,"port":"out"},"to":{"node":CARD,"port":"speech"},"order":0}}),
        )
        .unwrap();
    let resume_path = dir.path().join("resume.ncraft");
    if resume {
        session.dispatch("file.save_as", &json!({"location":resume_path})).unwrap();
    }
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    if !streaming {
        let error = session.dispatch("run.quote", &request).unwrap_err();
        assert!(error.message.contains("HeyGen needs a desktop project"), "{error}");
        assert!(wire.requests.lock().unwrap().is_empty());
        assert!(budget.log.lock().unwrap().entries().next().is_none());
        return;
    }
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], false);
    assert!(quote["totals"].as_object().unwrap().is_empty());
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(wire.requests.lock().unwrap().is_empty());
    request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":true,"unpriced_jobs":0});
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(wire.requests.lock().unwrap().is_empty());
    request["accept"]["unpriced_jobs"] = json!(1);
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut notices = vec![];
    if resume {
        while !journal.saved().is_some_and(|saved| !saved.submitted.is_empty()) {
            notices.extend(host.pump(&mut session));
            assert!(Instant::now() < deadline, "handle never journaled");
            std::thread::sleep(Duration::from_millis(1));
        }
        host.shut_down();
        drop(host);
        drop(session);
        session = Session::with_storage(
            nodecraft_format::Snapshot { project: empty_project(), assets: Default::default(), epoch: 0 },
            Box::new(nodecraft_format::NativeStorage::new(dir.path().join("resume-recovery")).unwrap()),
        )
        .unwrap();
        session.dispatch("file.open", &json!({"location":resume_path,"discard_dirty":true})).unwrap();
        host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone());
        host.open_journal(Box::new(journal)).unwrap();
        let mut services = Services::default();
        host.register(&mut services).unwrap();
        session = session.with_services(services);
        host.install(&mut session);
        assert!(host.has_waiting());
        wire.gated.store(false, Ordering::SeqCst);
        host.resume(&session, &binding);
    }
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
        let [Value::Media(asset)] = node.history.last().unwrap().outputs.as_slice() else { panic!("one complete video") };
        let asset = asset.clone();
        assert_eq!(asset.mime.essence(), "video/mp4");
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let read = |s: &Session| {
            let lease = s.assets().open_verified(&asset, &Default::default(), &metrics).unwrap();
            let mut bytes = Vec::new();
            lease.reader(metrics.clone()).take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            assert!(bytes.len() <= 64 * 1024 * 1024);
            bytes
        };
        assert_eq!(read(&session).as_slice(), png.as_ref());
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
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "unknown usage must not invent a currency invoice");
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Post).count(), 1, "uncertain create never replays");
    if !hold_then_retry && !resume {
        assert_eq!(requests.len(), if lost { 1 } else { 4 });
    }
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body["audio_url"].as_str().unwrap().starts_with("data:audio/wav;base64,"));
    assert!(body.get("prompt").is_none());
    if photo {
        assert_eq!(body["resolution"], "720p");
        assert_eq!(body["enable_safety_checker"], true);
        assert!(body["image_url"].as_str().unwrap().starts_with("data:image/png;base64,"));
        assert_eq!(body.as_object().unwrap().len(), 7);
    } else {
        assert_eq!(body["enable_dynamic_duration"], false);
        assert_eq!(body["enable_caption"], false);
        assert!(body["video_url"].as_str().unwrap().starts_with("data:video/mp4;base64,"));
        assert_eq!(body.as_object().unwrap().len(), 6);
    }
    if !lost {
        assert!(requests.iter().find(|request| request.url.starts_with("https://v3.fal.media/")).unwrap().headers.is_empty(), "no credential sent to storage");
    }
}
#[test]
fn heygen43_actual_host_both_media_modes_keep_unknown_consent_and_original_history() {
    for model in ["fal.heygen-avatar4", "fal.heygen-lipsync-speed", "fal.heygen-lipsync-precision"] {
        run(model, false, true, false, false, false);
    }
}
#[test]
fn heygen43_actual_large_result_worker_history_indexed_save_reopen_and_held_retry() {
    run("fal.heygen-lipsync-speed", false, true, true, false, false);
    run("fal.heygen-avatar4", false, true, true, true, false);
}
#[test]
fn heygen43_actual_lost_paid_reply_never_replays_and_classic_refuses_all_modes() {
    run("fal.heygen-avatar4", true, true, false, false, false);
    for model in ["fal.heygen-avatar4", "fal.heygen-lipsync-speed", "fal.heygen-lipsync-precision"] {
        run(model, false, false, false, false, false);
    }
}
#[test]
fn heygen43_actual_journal_resume_polls_the_original_task_without_another_create() {
    run("fal.heygen-lipsync-precision", false, true, true, false, true);
}
