//! Alibaba's four shipped models through RunHost, consent, media resolution and fal's queue. Offline.
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
use nodecraft_nodes::generation::{KIND, VIDEO_KIND};
use nodecraft_providers::{
    BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError,
    fal::{Fal, QWEN_EDIT, QWEN_IMAGE, WAN3_I2V, WAN3_VIDEO},
    secrets::Secret,
};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ParamValues, ProjectId, Unsigned, Value};
use serde_json::{Value as Json, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([32; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const CLIP: &[u8] = include_bytes!("../../providers/src/fal/fixtures/blue.mp4");
const CDN: &str = "https://v3.fal.media/public-alibaba32";

#[derive(Clone, Copy, Debug)]
enum Failure {
    None,
    LostReply,
    AmbiguousReply,
}

struct Case {
    id: &'static str,
    preset: &'static str,
    references: u8,
    duration: u64,
    resolution: &'static str,
    audio: bool,
    prompt: &'static str,
}
impl Case {
    fn image(id: &'static str, preset: &'static str, references: u8) -> Self {
        Self { id, preset, references, duration: 5, resolution: "720p", audio: true, prompt: "A blue paper boat on a pond" }
    }
    fn video(id: &'static str, duration: u64, resolution: &'static str, audio: bool, references: u8) -> Self {
        Self { id, preset: "1k-square", references, duration, resolution, audio, prompt: "A blue paper boat on a pond" }
    }
    fn video_output(&self) -> bool {
        self.id.starts_with("fal.wan3")
    }
    fn route(&self) -> &'static str {
        match self.id {
            "fal.qwen-image3" => QWEN_IMAGE,
            "fal.qwen-image3-edit" => QWEN_EDIT,
            "fal.wan3" => WAN3_VIDEO,
            "fal.wan3-i2v" => WAN3_I2V,
            _ => panic!("unknown fixture model"),
        }
    }
    fn price(&self) -> u64 {
        if self.video_output() {
            self.duration
                * match self.resolution {
                    "480p" => 50_000,
                    "720p" => 100_000,
                    "1080p" => 200_000,
                    _ => panic!("unknown fixture resolution"),
                }
        } else {
            6 * if self.preset == "1k-square" { 40_000 } else { 75_000 }
        }
    }
}

struct Transport {
    route: &'static str,
    video: bool,
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
            let root = if self.video { "alibaba/wan-3.0" } else { "alibaba/qwen-image-3" };
            let task = format!("https://queue.fal.run/{root}/requests/public-task-32");
            let body = if submit {
                assert_eq!(url, format!("https://queue.fal.run/{}", self.route), "only the selected route may receive a paid create");
                match self.failure {
                    Failure::LostReply => return Err(ProviderError::new(ErrorKind::Network, "lost paid reply")),
                    Failure::AmbiguousReply => br#"{"request_id":"public-task-32","request_id":"another-task","status":"IN_QUEUE"}"#.to_vec(),
                    Failure::None => serde_json::to_vec(&json!({"request_id":"public-task-32","status":"IN_QUEUE","status_url":format!("{task}/status"),"response_url":task,"cancel_url":format!("{task}/cancel")})).unwrap(),
                }
            } else if url == format!("{task}/status") {
                serde_json::to_vec(&json!({"status":"COMPLETED"})).unwrap()
            } else if url == task {
                let response = if self.video {
                    json!({"video":{"url":format!("{CDN}/clip.mp4")}})
                } else {
                    json!({"images":(0..6).map(|index| json!({"url":format!("{CDN}/{index}")})).collect::<Vec<_>>()})
                };
                serde_json::to_vec(&response).unwrap()
            } else if self.video {
                assert_eq!(url, format!("{CDN}/clip.mp4"));
                CLIP.to_vec()
            } else {
                let index: u8 = url.strip_prefix(&format!("{CDN}/")).unwrap().parse().unwrap();
                assert!(index < 6);
                picture(index)
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

fn picture(index: u8) -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[100 + index; 4 * 4 * 4], &Default::default()).unwrap()
}
fn reference(index: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    nodecraft_media::image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode(&vec![40 + index * 60; 384 * 384 * 3], 384, 384, nodecraft_media::image::ExtendedColorType::Rgb8)
        .unwrap();
    bytes
}
/// Decode the fixture's standard base64 data URI so order is checked against original bytes.
fn reference_bytes(uri: &Json) -> Vec<u8> {
    let encoded = uri.as_str().unwrap().strip_prefix("data:image/jpeg;base64,").unwrap();
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

fn run(case: Case, failure: Failure) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut values: ParamValues = [("model".into(), Value::Text(case.id.into())), ("prompt".into(), Value::Text(case.prompt.into()))].into();
    values.insert("seed".into(), Value::Unsigned(Unsigned(13)));
    values.insert("enable_prompt_expansion".into(), Value::Bool(false));
    if case.video_output() {
        values.insert("duration_seconds".into(), Value::Unsigned(Unsigned(case.duration)));
        values.insert("resolution".into(), Value::Text(case.resolution.into()));
        values.insert("aspect_ratio".into(), Value::Text("9:16".into()));
        values.insert("audio".into(), Value::Bool(case.audio));
        values.insert("enable_thinking".into(), Value::Bool(true));
    } else {
        values.insert("image_preset".into(), Value::Text(case.preset.into()));
        values.insert("num_images".into(), Value::Unsigned(Unsigned(6)));
    }
    let kind = if case.video_output() { VIDEO_KIND } else { KIND };
    let card = kinds.get(kind).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([32; 16]), "Public Alibaba fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "alibaba32".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let http = Arc::new(Transport { route: case.route(), video: case.video_output(), failure, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    // Connect list references in reverse insertion order; edge.order, not insertion/ID order, is authoritative.
    for index in (0..case.references).rev() {
        let source = NodeId::from_bytes([index + 2; 16]);
        let jpeg = reference(index);
        import_files(
            &mut session,
            C,
            Position { x: -300.0, y: f32::from(index) * 200.0 },
            200.0,
            &[Incoming { name: "reference.jpg", bytes: &jpeg }],
            &[source],
        )
        .unwrap();
        let role = if !case.video_output() {
            "images"
        } else if index == 0 {
            "first_frame"
        } else {
            "last_frame"
        };
        let order = if case.video_output() { 0 } else { index };
        session.dispatch("edge.connect", &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([20-index;16]),"from":{"node":source,"port":"out"},"to":{"node":CARD,"port":role},"order":order}})).unwrap();
    }
    let price = case.price();
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], price, "{quote}");
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(http.requests.lock().unwrap().is_empty(), "missing consent must perform no HTTP");
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
    if !succeeded {
        assert!(node.history.is_empty());
        assert!(notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. })), "{notices:?}");
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let outputs = &node.history.last().unwrap().outputs;
        let media = if case.video_output() {
            outputs.as_slice()
        } else {
            let [Value::List(images)] = outputs.as_slice() else { panic!("one typed image list expected") };
            images.as_slice()
        };
        assert_eq!(media.len(), if case.video_output() { 1 } else { 6 });
        for (index, value) in media.iter().enumerate() {
            let Value::Media(asset) = value else { panic!("typed media expected") };
            let metrics = nodecraft_format::asset_source::StreamMetrics::default();
            let bytes = session.assets().open_verified(asset, &Default::default(), &metrics).unwrap().read_small(1024 * 1024, &metrics).unwrap();
            if case.video_output() {
                assert_eq!(asset.mime.essence(), "video/mp4");
                assert_eq!(bytes, CLIP);
                let movie = nodecraft_media::probe_movie(&bytes).unwrap();
                assert!(movie.duration_ms.is_some_and(|ms| ms > 0));
                assert!(movie.width.is_some_and(|width| width > 0));
                assert!(movie.height.is_some_and(|height| height > 0));
            } else {
                assert_eq!(asset.mime.essence(), "image/png");
                assert_eq!(bytes, picture(u8::try_from(index).unwrap()));
                let (meta, decoded) = nodecraft_media::decode_image(&bytes, &Default::default()).unwrap();
                assert_eq!((meta.width, meta.height), (4, 4));
                assert_eq!((decoded.width(), decoded.height()), (4, 4));
            }
        }
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost.micros, price);
    assert_eq!(entries[0].status, if succeeded { Charge::Actual } else { Charge::Unresolved });
    let requests = http.requests.lock().unwrap();
    let posts: Vec<_> = requests.iter().filter(|request| request.method == HttpMethod::Post).collect();
    assert_eq!(posts.len(), 1, "ambiguous paid submissions may neither replay nor fall back");
    if !succeeded {
        assert_eq!(requests.len(), 1, "a lost/ambiguous handle cannot be polled or downloaded");
    }
    let body: Json = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["seed"], 13);
    assert_eq!(body["enable_prompt_expansion"], false);
    assert_eq!(body["enable_safety_checker"], true);
    if case.video_output() {
        assert_eq!(body["duration"], case.duration);
        assert_eq!(body["resolution"], case.resolution);
        assert_eq!(body["audio"], case.audio);
        assert_eq!(body["aspect_ratio"], "9:16");
        assert_eq!(body["enable_thinking"], true);
        if case.references > 0 {
            assert_eq!(reference_bytes(&body["start_image_url"]), reference(0));
        }
        if case.references > 1 {
            assert_eq!(reference_bytes(&body["end_image_url"]), reference(1));
        } else {
            assert!(body.get("end_image_url").is_none());
        }
        if case.prompt.is_empty() {
            assert!(body.get("prompt").and_then(Json::as_str).is_none_or(str::is_empty));
        } else {
            assert_eq!(body["prompt"], case.prompt);
        }
    } else {
        let side = if case.preset == "1k-square" { 1024 } else { 2048 };
        assert_eq!(body["image_size"], json!({"width":side,"height":side}));
        assert_eq!(body["num_images"], 6);
        assert_eq!(body["output_format"], "png");
        assert_eq!(body["prompt"], case.prompt);
        if case.references > 0 {
            let references = body["image_urls"].as_array().unwrap();
            assert_eq!(references.len(), usize::from(case.references));
            for (index, reference_uri) in references.iter().enumerate() {
                assert_eq!(reference_bytes(reference_uri), reference(u8::try_from(index).unwrap()));
            }
        } else {
            assert!(body.get("image_urls").is_none());
        }
    }
    for request in requests.iter().filter(|request| request.url.starts_with(CDN)) {
        assert!(request.headers.is_empty(), "CDN receives no API credentials");
    }
}

#[test]
fn alibaba32_qwen_both_presets_produce_six_decodable_images_and_preserve_one_to_three_ordered_references() {
    for preset in ["1k-square", "2k-square"] {
        run(Case::image("fal.qwen-image3", preset, 0), Failure::None);
        for references in 1..=3 {
            run(Case::image("fal.qwen-image3-edit", preset, references), Failure::None);
        }
    }
}
#[test]
fn alibaba32_wan_both_modes_quote_all_three_rates_explicit_duration_bounds_and_audio_with_viewable_video() {
    for (duration, resolution, audio) in [(2, "480p", false), (5, "720p", true), (30, "1080p", true)] {
        run(Case::video("fal.wan3", duration, resolution, audio, 0), Failure::None);
        run(Case::video("fal.wan3-i2v", duration, resolution, audio, 2), Failure::None);
    }
    let mut optional = Case::video("fal.wan3-i2v", 2, "480p", false, 1);
    optional.prompt = "";
    run(optional, Failure::None);
}
#[test]
fn alibaba32_lost_and_ambiguous_paid_replies_stay_unresolved_without_replay_or_fallback() {
    for failure in [Failure::LostReply, Failure::AmbiguousReply] {
        run(Case::image("fal.qwen-image3", "1k-square", 0), failure);
        run(Case::image("fal.qwen-image3-edit", "2k-square", 1), failure);
        run(Case::video("fal.wan3", 5, "720p", true, 0), failure);
        run(Case::video("fal.wan3-i2v", 2, "480p", false, 1), failure);
    }
}
