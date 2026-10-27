//! Shipping cards, resolver, consent, ledger and RunHost with the actual Stability adapter. Offline.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
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
use nodecraft_nodes::generation::{AUDIO_KIND, KIND, MESH_KIND};
use nodecraft_providers::{
    BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError,
    secrets::Secret,
    stability::{AUDIO, CORE, FAST_3D, Stability},
};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Unsigned, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([36; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const ID: &str = "a6dc6c6e20acda010fe14d71f180658f2896ed9b4ec25aa99a6ff06c796987c4";
const GLB: &[u8] = include_bytes!("../../providers/src/fal/fixtures/triangle.glb");
fn png(side: u32) -> Vec<u8> {
    nodecraft_media::encode_png(side, side, &vec![180; usize::try_from(side * side * 4).unwrap()], &Default::default()).unwrap()
}
fn wav() -> Vec<u8> {
    let data = vec![0u8; 441 * 4];
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(data.len() + 36).unwrap().to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(44_100u32.to_le_bytes());
    out.extend(176_400u32.to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend(data);
    out
}
struct Transport {
    model: &'static str,
    failure: u8,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let (posts, gets) = {
            let mut requests = self.requests.lock().unwrap();
            let posts = requests.iter().filter(|r| r.method == HttpMethod::Post).count();
            let gets = requests.iter().filter(|r| r.method == HttpMethod::Get).count();
            requests.push(request.clone());
            (posts, gets)
        };
        Box::pin(async move {
            if request.method == HttpMethod::Post {
                let path = match self.model {
                    CORE => "stable-image/generate/core",
                    AUDIO => "audio/stable-audio/text-to-audio",
                    _ => "3d/stable-fast-3d",
                };
                assert_eq!(request.url, format!("https://api.stability.ai/v2beta/{path}"));
                if self.failure == 1 {
                    return Err(ProviderError::new(ErrorKind::Network, "lost paid reply"));
                }
                if self.failure == 2 || self.failure == 3 && posts == 0 {
                    let (status, name) = if self.failure == 2 { (401, "unauthorized") } else { (429, "rate_limit_exceeded") };
                    return Ok(HttpResponse {
                        status,
                        headers: vec![],
                        body: serde_json::to_vec(&json!({"id":"public-refusal","name":name,"errors":["fixture refusal"]})).unwrap(),
                    });
                }
                if self.model == AUDIO {
                    return Ok(HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"id":ID})).unwrap() });
                }
            } else {
                assert_eq!(request.method, HttpMethod::Get);
                assert_eq!(request.url, format!("https://api.stability.ai/v2beta/audio/results/{ID}"));
                if self.failure == 4 && gets == 0 {
                    return Ok(HttpResponse { status: 503, headers: vec![], body: b"fixture busy".to_vec() });
                }
                if gets == usize::from(self.failure == 4) {
                    return Ok(HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"id":ID,"status":"in-progress"})).unwrap() });
                }
            }
            let (mime, mut body) = match self.model {
                CORE => ("image/png", png(4)),
                AUDIO => ("audio/wav", wav()),
                _ => ("model/gltf-binary", GLB.to_vec()),
            };
            if self.failure == 5 && self.model == AUDIO {
                body[20..22].copy_from_slice(&65535u16.to_le_bytes());
            }
            Ok(HttpResponse { status: 200, headers: vec![("Content-Type".into(), mime.into())], body })
        })
    }
}
struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, _ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1));
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
fn run(id: &str, duration: u64, failure: u8) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let (kind, model, price) = match id {
        "stability.image-core" => (KIND, CORE, 30_000),
        "stability.audio3" => (AUDIO_KIND, AUDIO, 80_000),
        _ => (MESH_KIND, FAST_3D, 100_000),
    };
    let mut params = [("model".into(), Value::Text(id.into())), ("prompt".into(), Value::Text("A blue paper boat".into()))]
        .into_iter()
        .collect::<nodecraft_types::ParamValues>();
    if model == AUDIO {
        params.insert("duration_seconds".into(), Value::Unsigned(Unsigned(duration)));
    }
    let node = kinds.get(kind).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([36; 16]), "Public Stability fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "stability36".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport { model, failure, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Stability::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "stability", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    if model == FAST_3D {
        let source = NodeId::from_bytes([2; 16]);
        let picture = png(64);
        import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &[Incoming { name: "public-reference.png", bytes: &picture }], &[source]).unwrap();
        session
            .dispatch(
                "edge.connect",
                &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([2;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":"image"},"order":0}}),
            )
            .unwrap();
    }
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], price, "{quote}");
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(http.requests.lock().unwrap().is_empty(), "no paid call before exact cost consent");
    request["accept"] = json!({"token":quote["token"],"amounts":{"USD":price},"partial":false,"unpriced_jobs":0});
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
    let node = session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    if matches!(failure, 1 | 2 | 5) {
        assert!(node.history.is_empty());
        assert!(notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. })), "{notices:?}");
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let outputs = &node.history.last().unwrap().outputs;
        let asset = if model == CORE {
            let [Value::List(images)] = outputs.as_slice() else { panic!("image list") };
            let [Value::Media(asset)] = images.as_slice() else { panic!("one image") };
            asset
        } else {
            let [Value::Media(asset)] = outputs.as_slice() else { panic!("typed media") };
            asset
        };
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let bytes = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap();
        match model {
            CORE => {
                assert_eq!(asset.mime.essence(), "image/png");
                assert_eq!(bytes, png(4));
                let (meta, image) = nodecraft_media::decode_image(&bytes, &Default::default()).unwrap();
                assert_eq!((meta.width, meta.height), (4, 4));
                assert_eq!((image.width(), image.height()), (4, 4));
            }
            AUDIO => {
                assert_eq!(asset.mime.essence(), "audio/wav");
                assert_eq!(bytes, wav());
                let sound = nodecraft_media::probe_sound(&bytes);
                assert_eq!((sound.sample_rate, sound.channels), (Some(44_100), Some(2)));
                assert!(sound.duration_ms.is_some_and(|n| n > 0));
            }
            _ => {
                assert_eq!(asset.mime.essence(), "model/gltf-binary");
                assert_eq!(bytes, GLB);
                let mesh = nodecraft_media::decode_mesh(&bytes, &Default::default()).unwrap();
                assert_eq!((mesh.positions.len(), mesh.indices.len()), (3, 3));
            }
        }
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    if failure == 2 {
        assert!(entries.is_empty(), "a definite refusal releases the reservation");
    } else {
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cost.micros, price);
        assert_eq!(entries[0].status, if matches!(failure, 1 | 5) { Charge::Unresolved } else { Charge::Actual });
    }
    let requests = http.requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|request| request.method == HttpMethod::Post).count(),
        if failure == 3 { 2 } else { 1 },
        "only a proven unbilled rate-limit refusal can retry a paid create"
    );
    if model == FAST_3D {
        let picture = png(64);
        assert!(requests[0].body.windows(picture.len()).any(|part| part == picture), "the actual asset reaches the first-party upload");
    }
    if model == AUDIO && !matches!(failure, 1 | 2) {
        assert!(requests.iter().filter(|request| request.method == HttpMethod::Get).all(|request| request.url.ends_with(ID)));
    }
}
#[test]
fn stability36_actual_host_runs_all_three_catalog_families_with_exact_cost_consent_and_usable_media() {
    run("stability.image-core", 30, 0);
    run("stability.audio3", 1, 0);
    run("stability.audio3", 380, 0);
    run("stability.fast-3d", 30, 0);
}
#[test]
fn stability36_actual_host_lost_paid_replies_stay_unresolved_without_post_replay() {
    for id in ["stability.image-core", "stability.audio3", "stability.fast-3d"] {
        run(id, 30, 1);
    }
}
#[test]
fn stability36_actual_host_releases_definite_refusals_and_retries_only_unbilled_rate_limits() {
    for id in ["stability.image-core", "stability.audio3", "stability.fast-3d"] {
        run(id, 30, 2);
        run(id, 30, 3);
    }
}
#[test]
fn stability36_actual_host_audio_poll_failure_resumes_the_accepted_id_without_a_second_post() {
    run("stability.audio3", 30, 4);
}

#[test]
fn stability36_actual_host_unplayable_wav_format_is_unresolved_without_recording_success_or_replaying() {
    run("stability.audio3", 30, 5);
}
