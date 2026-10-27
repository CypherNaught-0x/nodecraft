//! Hunyuan3D through shipping catalog, typed mesh outputs, cost consent and actual fal queue. Offline.
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
use nodecraft_exec::cost::Charge;
use nodecraft_exec::{
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::MESH_KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([42; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const GLB: &[u8] = include_bytes!("../../providers/src/fal/fixtures/triangle.glb");
struct Transport {
    route: &'static str,
    failure: u8,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = r.url.clone();
        let submit = r.method == nodecraft_providers::HttpMethod::Post;
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            if submit && self.failure == 1 {
                return Err(ProviderError::new(ErrorKind::Network, "lost paid reply"));
            }
            let task = "https://queue.fal.run/fal-ai/hunyuan3d-v3/requests/public-task-42";
            let body = if submit {
                assert_eq!(url, format!("https://queue.fal.run/{}", self.route));
                serde_json::to_vec(&json!({"request_id":"public-task-42","status":"IN_QUEUE","status_url":format!("{task}/status"),"response_url":task,"cancel_url":format!("{task}/cancel")})).unwrap()
            } else if url.ends_with("/status") {
                serde_json::to_vec(&if self.failure == 2 { json!({"status":"COMPLETED","error":"generation failed"}) } else { json!({"status":"COMPLETED"}) })
                    .unwrap()
            } else if url == task {
                serde_json::to_vec(&json!({"model_glb":{"url":"https://v3.fal.media/public-hunyuan.glb","file_size":GLB.len()},"thumbnail":{"url":"https://unused.example/thumbnail"},"model_urls":{"obj":{"url":"https://unused.example/mesh.obj"}}})).unwrap()
            } else {
                assert_eq!(url, "https://v3.fal.media/public-hunyuan.glb");
                GLB.to_vec()
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

fn run(id: &str, preset: &str, failure: u8) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let (route, role) = match id {
        "fal.hunyuan3d-v3" => (nodecraft_providers::fal::HUNYUAN_TEXT, None),
        "fal.hunyuan3d-v3-image" => (nodecraft_providers::fal::HUNYUAN_IMAGE, Some("image")),
        _ => (nodecraft_providers::fal::HUNYUAN_SKETCH, Some("image")),
    };
    let params =
        [("model".into(), Value::Text(id.into())), ("prompt".into(), Value::Text("A simple cube".into())), ("mesh_preset".into(), Value::Text(preset.into()))]
            .into();
    let card = kinds.get(MESH_KIND).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([42; 16]), "Public Hunyuan fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "hunyuan42".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport { route, failure, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    if let Some(role) = role {
        let side = if id.ends_with("-sketch") { 128 } else { 4 };
        let image = nodecraft_media::encode_png(side, side, &vec![120; usize::try_from(side * side * 4).unwrap()], &Default::default()).unwrap();
        let source = NodeId::from_bytes([2; 16]);
        import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &[Incoming { name: "public-reference.png", bytes: &image }], &[source]).unwrap();
        session
            .dispatch(
                "edge.connect",
                &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([2;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":role},"order":0}}),
            )
            .unwrap();
    }
    let price = if preset == "pbr" { 525_000 } else { 375_000 };
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], price, "{quote}");
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(http.requests.lock().unwrap().is_empty());
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
    if failure > 0 {
        assert!(node.history.is_empty());
        assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let [Value::Media(asset)] = node.history.last().unwrap().outputs.as_slice() else { panic!("one typed mesh expected") };
        assert_eq!(asset.mime.essence(), "model/gltf-binary");
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let bytes = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap();
        assert_eq!(bytes, GLB);
        let mesh = nodecraft_media::decode_mesh(&bytes, &Default::default()).unwrap();
        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.indices.len(), 3);
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost.micros, price);
    assert_eq!(entries[0].status, if failure > 0 { Charge::Unresolved } else { Charge::Actual });
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == nodecraft_providers::HttpMethod::Post).count(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["enable_pbr"], preset == "pbr");
    assert!(body.get("face_count").is_none());
    assert_eq!(body.get("input_image_url").is_some(), role.is_some());
    assert_eq!(body.get("prompt").is_some(), id != "fal.hunyuan3d-v3-image");
    assert_eq!(body.get("generate_type").is_some(), id != "fal.hunyuan3d-v3-sketch");
    for r in requests.iter().filter(|r| r.url.starts_with("https://v3.fal.media")) {
        assert!(r.headers.is_empty());
    }
}
#[test]
fn hunyuan42_actual_host_text_photo_and_sketch_produce_viewable_meshes_with_exact_prices() {
    for id in ["fal.hunyuan3d-v3", "fal.hunyuan3d-v3-image", "fal.hunyuan3d-v3-sketch"] {
        run(id, "textured", 0);
    }
}
#[test]
fn hunyuan42_actual_host_includes_pbr_surcharge_for_each_mode() {
    for id in ["fal.hunyuan3d-v3", "fal.hunyuan3d-v3-image", "fal.hunyuan3d-v3-sketch"] {
        run(id, "pbr", 0);
    }
}
#[test]
fn hunyuan42_actual_host_keeps_failed_or_lost_paid_requests_and_never_replays_creates() {
    run("fal.hunyuan3d-v3", "textured", 1);
    run("fal.hunyuan3d-v3-image", "pbr", 2);
}
