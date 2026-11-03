//! All three MiniMax families through the actual native paid RunHost; original offline media only.
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
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Unsigned, Value};
use serde_json::json;
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const C: CanvasId = CanvasId::from_bytes([27; 16]);
const N: NodeId = NodeId::from_bytes([28; 16]);
const FRAME: NodeId = NodeId::from_bytes([29; 16]);
const MP3: &[u8] = include_bytes!("../../providers/src/fal/fixtures/minimax27-tone.mp3");
const MP4: &[u8] = include_bytes!("../../providers/src/fal/fixtures/minimax27-motion.mp4");
const PROMPT: &str = "Hello, 雪 and music.";
fn wave(seconds: u32) -> Vec<u8> {
    let n = seconds * 176400;
    let mut b = Vec::with_capacity(n as usize + 44);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(n + 36).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&44100u32.to_le_bytes());
    b.extend_from_slice(&176400u32.to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&n.to_le_bytes());
    b.resize(n as usize + 44, 0);
    b
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
struct Wire {
    alias: &'static str,
    root: &'static str,
    bytes: Arc<[u8]>,
    speech: bool,
    music: bool,
    seconds: u32,
    lost: bool,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = r.url.clone();
        let method = r.method;
        let cap = r.max_response_bytes;
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            let root = format!("https://queue.fal.run/{}/requests/minimax-public27", self.root);
            let body = if method == HttpMethod::Post {
                assert_eq!(url, format!("https://queue.fal.run/{}", self.alias));
                if self.lost {
                    return Err(ProviderError::new(ErrorKind::Network, "lost paid reply"));
                }
                serde_json::to_vec(&json!({"request_id":"minimax-public27","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})).unwrap()
            } else if url.ends_with("/status") {
                br#"{"status":"COMPLETED"}"#.to_vec()
            } else if url == root {
                let file = json!({"url":"https://v3.fal.media/files/original/minimax27","file_size":self.bytes.len()});
                let value = if self.speech {
                    json!({"audio":file,"duration_ms":1044})
                } else if self.music {
                    json!({"audio":file,"seed":27,"duration":self.seconds})
                } else {
                    json!({"video":file})
                };
                serde_json::to_vec(&value).unwrap()
            } else {
                assert_eq!(url, "https://v3.fal.media/files/original/minimax27");
                self.bytes.to_vec()
            };
            assert!(body.len() <= cap);
            Ok(HttpResponse { status: 200, headers: vec![], body })
        })
    }
}
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([27; 16]), "Public MiniMax studies").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Studies") }.apply(&mut p).unwrap();
    p
}
fn run(id: &'static str, streaming: bool, large: bool, lost: bool) {
    let (alias, root, speech, music) = match id {
        "fal.minimax-h3" => ("minimax/h3/text-to-video", "minimax/h3", false, false),
        "fal.minimax-h3-i2v" => ("minimax/h3/image-to-video", "minimax/h3", false, false),
        "fal.minimax-speech28-hd" => ("fal-ai/minimax/speech-2.8-hd", "fal-ai/minimax", true, false),
        "fal.minimax-music3" => ("minimax/music-3", "minimax/music-3", false, true),
        _ => panic!("case"),
    };
    let seconds = if music { if large { 120 } else { 1 } } else { 5 };
    let bytes: Arc<[u8]> = if speech {
        MP3.into()
    } else if music {
        wave(seconds).into()
    } else {
        MP4.into()
    };
    if large {
        assert!(bytes.len() > 16 * 1024 * 1024);
    }
    let dir = tempfile::tempdir().unwrap();
    let catalog = GenerationCatalog::builtin().unwrap();
    let kind = if speech || music { nodecraft_nodes::generation::AUDIO_KIND } else { nodecraft_nodes::generation::VIDEO_KIND };
    let mut params =
        [("model".into(), Value::Text(id.into())), ("prompt".into(), Value::Text(PROMPT.into()))].into_iter().collect::<nodecraft_types::ParamValues>();
    if !speech {
        params.insert("duration".into(), Value::Unsigned(Unsigned(u64::from(seconds))));
    }
    if music {
        params.insert("lyrics".into(), Value::Text("[verse]\nThe river carries light\n[chorus]\nWe follow it home".into()));
    }
    let node = catalog.registry().unwrap().get(kind).unwrap().instantiate_instance(N, Default::default(), &params).unwrap();
    let mut p = project();
    Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
    let snapshot = nodecraft_format::Snapshot { project: p, assets: Default::default(), epoch: 0 };
    let mut session = if streaming {
        Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap())).unwrap()
    } else {
        Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap()
    };
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "minimax27".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    session = session.with_services(services);
    let wire = Arc::new(Wire { alias, root, bytes: bytes.clone(), speech, music, seconds, lost, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("minimax27-offline-fixture").unwrap(), wire.clone()))).unwrap();
    routing.select(N, "fal", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    if id.ends_with("-i2v") {
        let png = nodecraft_media::encode_png(64, 64, &vec![120; 64 * 64 * 4], &Default::default()).unwrap();
        import_files(&mut session, C, Position { x: -300., y: 0. }, 200., &[Incoming { name: "original.png", bytes: &png }], &[FRAME]).unwrap();
        session.dispatch("edge.connect",&json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([30;16]),"from":{"node":FRAME,"port":"out"},"to":{"node":N,"port":"first_frame"},"order":0}})).unwrap();
    }
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[N]});
    if !streaming {
        let error = session.dispatch("run.quote", &request).unwrap_err();
        assert!(error.message.contains("MiniMax"), "{error}");
        assert!(wire.requests.lock().unwrap().is_empty());
        assert!(budget.log.lock().unwrap().entries().next().is_none());
        return;
    }
    let quote = session.dispatch("run.quote", &request).unwrap();
    let amount = if speech {
        PROMPT.len() as u64 * 100
    } else if music {
        u64::from(seconds) * 2000
    } else {
        u64::from(seconds) * 50000
    };
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], amount);
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(wire.requests.lock().unwrap().is_empty());
    request["accept"] = json!({"token":quote["token"],"amounts":{"USD":amount},"partial":false,"unpriced_jobs":0});
    session.dispatch("run.start", &request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut notices = vec![];
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    if lost {
        assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
        assert!(session.document().canvases()[&C].nodes()[&N].history.is_empty());
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: N }), "{notices:?}");
        let node = &session.document().canvases()[&C].nodes()[&N];
        let [Value::Media(asset)] = node.history.last().unwrap().outputs.as_slice() else { panic!("one typed audio/video output") };
        let asset = asset.clone();
        assert_eq!(
            asset.mime.essence(),
            if speech {
                "audio/mpeg"
            } else if music {
                "audio/wav"
            } else {
                "video/mp4"
            }
        );
        assert!(asset.meta.duration_ms.is_some());
        let read = |s: &Session| {
            let metrics = nodecraft_format::asset_source::StreamMetrics::default();
            let lease = s.assets().open_verified(&asset, &Default::default(), &metrics).unwrap();
            let mut b = vec![];
            lease.reader(metrics).take(64 * 1024 * 1024 + 1).read_to_end(&mut b).unwrap();
            b
        };
        assert_eq!(read(&session).as_slice(), bytes.as_ref());
        let path = dir.path().join("result.ncraft");
        session.dispatch("file.save_as", &json!({"location":path})).unwrap();
        let expected = session.document().clone();
        drop(session);
        let mut reopened = Session::with_storage(
            nodecraft_format::Snapshot { project: project(), assets: Default::default(), epoch: 0 },
            Box::new(nodecraft_format::NativeStorage::new(dir.path().join("reopen")).unwrap()),
        )
        .unwrap();
        reopened.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
        assert_eq!(reopened.document(), &expected);
        assert_eq!(read(&reopened).as_slice(), bytes.as_ref());
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost.micros, amount);
    assert_eq!(entries[0].status, if lost { Charge::Unresolved } else { Charge::Actual });
    assert_eq!(wire.requests.lock().unwrap().iter().filter(|r| r.method == HttpMethod::Post).count(), 1, "no uncertain create replay");
}
#[test]
fn minimax27_all_modalities_quote_consent_record_original_history_and_save_reopen() {
    for id in ["fal.minimax-h3", "fal.minimax-h3-i2v", "fal.minimax-speech28-hd", "fal.minimax-music3"] {
        run(id, true, false, false);
    }
}
#[test]
fn minimax27_large_wav_is_byte_exact_after_native_worker_history_and_streamed_save_reopen() {
    run("fal.minimax-music3", true, true, false);
}
#[test]
fn minimax27_classic_refuses_all_four_aliases_before_paid_http_or_reservation() {
    for id in ["fal.minimax-h3", "fal.minimax-h3-i2v", "fal.minimax-speech28-hd", "fal.minimax-music3"] {
        run(id, false, false, false);
    }
}
#[test]
fn minimax27_uncertain_paid_delivery_retains_reservation_for_all_three_families_without_replay() {
    for id in ["fal.minimax-h3", "fal.minimax-speech28-hd", "fal.minimax-music3"] {
        run(id, true, false, true);
    }
}
