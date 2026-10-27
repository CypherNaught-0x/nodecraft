//! Rodin's four fixed-price routes through the actual catalog, RunHost, and decoded mesh assets. Offline.
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
    cost::Charge,
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::MESH_KIND;
use nodecraft_providers::{
    BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError,
    fal::{Fal, RODIN_FAST_IMAGES, RODIN_FAST_TEXT, RODIN_IMAGES, RODIN_TEXT},
    secrets::Secret,
};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ParamValues, ProjectId, Unsigned, Value};
use serde_json::{Value as Json, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([41; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const GLB: &[u8] = include_bytes!("../../providers/src/fal/fixtures/triangle.glb");
const CDN: &str = "https://v3.fal.media/public-rodin41.glb";
const MODELS: [&str; 4] = ["fal.rodin25", "fal.rodin25-images", "fal.rodin25-fast", "fal.rodin25-fast-images"];

#[derive(Clone, Copy, Debug)]
enum Failure {
    None,
    LostReply,
    AmbiguousReply,
}
struct Transport {
    route: &'static str,
    failure: Failure,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let url = r.url.clone();
        let submit = r.method == HttpMethod::Post;
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            let task = "https://queue.fal.run/fal-ai/hyper3d/requests/public-task-41";
            let body = if submit {
                assert_eq!(url, format!("https://queue.fal.run/{}", self.route), "only the selected route may receive a paid create");
                match self.failure {
                    Failure::LostReply => return Err(ProviderError::new(ErrorKind::Network, "lost paid reply")),
                    Failure::AmbiguousReply => br#"{"request_id":"public-task-41","request_id":"another-task","status":"IN_QUEUE"}"#.to_vec(),
                    Failure::None => serde_json::to_vec(&json!({"request_id":"public-task-41","status":"IN_QUEUE","status_url":format!("{task}/status"),"response_url":task,"cancel_url":format!("{task}/cancel")})).unwrap(),
                }
            } else if url == format!("{task}/status") {
                serde_json::to_vec(&json!({"status":"COMPLETED"})).unwrap()
            } else if url == task {
                serde_json::to_vec(&json!({"model_mesh":{"url":CDN,"file_size":GLB.len(),"content_type":"application/octet-stream"},"model_meshes":[{"url":"https://unused.example/sidecar.obj"}],"textures":[{"url":"https://unused.example/texture.png"}]})).unwrap()
            } else {
                assert_eq!(url, CDN, "unused sidecars and textures must not be fetched");
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
fn reference(index: u8) -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[40 + index * 30; 4 * 4 * 4], &Default::default()).unwrap()
}
/// Decode a standard data URI to compare the actual graph ordering with original reference bytes.
fn reference_bytes(uri: &Json) -> Vec<u8> {
    let encoded = uri.as_str().unwrap().strip_prefix("data:image/png;base64,").unwrap();
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    assert_eq!(encoded.len() % 4, 0);
    let mut out = Vec::new();
    for chunk in encoded.as_bytes().as_chunks::<4>().0 {
        let mut bits = 0u32;
        for byte in chunk {
            bits = (bits << 6) | if *byte == b'=' { 0 } else { u32::try_from(alphabet.iter().position(|value| value == byte).unwrap()).unwrap() };
        }
        let decoded = bits.to_be_bytes();
        out.extend_from_slice(&decoded[1..4 - chunk.iter().filter(|byte| **byte == b'=').count()]);
    }
    out
}
fn run(id: &str, references: u8, prompt: Option<&str>, failure: Failure) {
    let route = match id {
        "fal.rodin25" => RODIN_TEXT,
        "fal.rodin25-images" => RODIN_IMAGES,
        "fal.rodin25-fast" => RODIN_FAST_TEXT,
        "fal.rodin25-fast-images" => RODIN_FAST_IMAGES,
        _ => panic!("unknown fixture model"),
    };
    let images = id.ends_with("-images");
    let fast = id.contains("-fast");
    let price = if fast { 100_000 } else { 400_000 };
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut values: ParamValues = [("model".into(), Value::Text(id.into())), ("seed".into(), Value::Unsigned(Unsigned(13)))].into();
    if let Some(prompt) = prompt {
        values.insert("prompt".into(), Value::Text(prompt.into()));
    }
    if images {
        values.insert("use_original_alpha".into(), Value::Bool(true));
    }
    let card = kinds.get(MESH_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([41; 16]), "Public Rodin fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "rodin41".into(), wall_clock_ms: Arc::new(|| 0) };
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
    // Insertion and both source/edge IDs disagree with the desired order: edge.order must win.
    for index in (0..references).rev() {
        let source = NodeId::from_bytes([9 - index; 16]);
        let png = reference(index);
        import_files(
            &mut session,
            C,
            Position { x: -300.0, y: f32::from(index) * 200.0 },
            200.0,
            &[Incoming { name: "public-reference.png", bytes: &png }],
            &[source],
        )
        .unwrap();
        session.dispatch("edge.connect", &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([20-index;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":"images"},"order":index}})).unwrap();
    }
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], price, "{quote}");
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(http.requests.lock().unwrap().is_empty(), "missing paid consent must perform zero HTTP");
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
    let succeeded = matches!(failure, Failure::None);
    if succeeded {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let [Value::Media(asset)] = node.history.last().unwrap().outputs.as_slice() else { panic!("one typed mesh expected") };
        assert_eq!(asset.mime.essence(), "model/gltf-binary");
        let metrics = nodecraft_format::asset_source::StreamMetrics::default();
        let bytes = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap();
        assert_eq!(bytes, GLB);
        let mesh = nodecraft_media::decode_mesh(&bytes, &Default::default()).unwrap();
        assert_eq!((mesh.positions.len(), mesh.indices.len()), (3, 3));
    } else {
        assert!(node.history.is_empty());
        assert!(notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. })), "{notices:?}");
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost.micros, price);
    assert_eq!(entries[0].status, if succeeded { Charge::Actual } else { Charge::Unresolved });
    let requests = http.requests.lock().unwrap();
    let posts: Vec<_> = requests.iter().filter(|request| request.method == HttpMethod::Post).collect();
    assert_eq!(posts.len(), 1, "ambiguous paid submissions may neither replay nor fall back");
    assert_eq!(requests.len(), if succeeded { 4 } else { 1 }, "only canonical queue and packed GLB are fetched");
    let body: Json = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["seed"], 13);
    assert_eq!(body["tier"], if fast { "Gen-2.5-Extreme-Low" } else { "Gen-2.5-High" });
    assert_eq!(body["material"], if fast { "Shaded" } else { "PBR" });
    assert_eq!(body["quality_mesh_option"], "Auto");
    assert_eq!(body["geometry_file_format"], "glb");
    assert_eq!(body["enable_safety_checker"], true);
    for name in ["enable_creative_mode", "hd_texture", "texture_delight", "is_micro", "TAPose"] {
        assert_eq!(body[name], false);
    }
    for name in ["addons", "model", "image_url", "bbox_condition"] {
        assert!(body.get(name).is_none());
    }
    if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
        assert_eq!(body["prompt"], prompt);
    } else {
        assert!(body.get("prompt").and_then(Json::as_str).is_none_or(str::is_empty), "empty image prompt stays auto mode");
    }
    if images {
        let urls = body["image_urls"].as_array().unwrap();
        assert_eq!(urls.len(), usize::from(references));
        for (index, uri) in urls.iter().enumerate() {
            assert_eq!(reference_bytes(uri), reference(u8::try_from(index).unwrap()));
        }
        assert_eq!(body["use_original_alpha"], true);
        assert_eq!(body["preview_render"], false);
    } else {
        for name in ["image_urls", "use_original_alpha", "preview_render"] {
            assert!(body.get(name).is_none());
        }
    }
    for request in requests.iter().filter(|request| request.url == CDN) {
        assert!(request.headers.is_empty(), "packed GLB storage receives no API credential");
    }
}

#[test]
fn rodin41_actual_host_all_four_routes_produce_viewable_meshes_with_exact_consented_costs() {
    for id in MODELS {
        run(id, if id.ends_with("-images") { 1 } else { 0 }, Some("A simple paper boat"), Failure::None);
    }
}
#[test]
fn rodin41_actual_host_preserves_one_to_five_ordered_references_and_absent_or_empty_auto_prompts() {
    for id in ["fal.rodin25-images", "fal.rodin25-fast-images"] {
        for references in 1..=5 {
            for prompt in [None, Some("")] {
                run(id, references, prompt, Failure::None);
            }
        }
    }
}
#[test]
fn rodin41_lost_and_ambiguous_paid_replies_remain_unresolved_without_replay_or_fallback() {
    for id in MODELS {
        for failure in [Failure::LostReply, Failure::AmbiguousReply] {
            run(id, if id.ends_with("-images") { 5 } else { 0 }, Some("A simple paper boat"), failure);
        }
    }
}
