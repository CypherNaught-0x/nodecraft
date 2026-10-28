//! Grok through shipping catalog, explicit cost consent, image resolver, ledger and saved fal queue. Offline.
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
use nodecraft_nodes::generation::{KIND, VIDEO_KIND};
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([31; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
struct Transport {
    model: &'static str,
    video: bool,
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
            let root = if self.video { "xai/grok-imagine-video" } else { "xai/grok-imagine-image" };
            let task = format!("https://queue.fal.run/{root}/requests/public-task-31");
            let body = if submit {
                assert_eq!(url, format!("https://queue.fal.run/{}", self.model));
                serde_json::to_vec(&json!({"request_id":"public-task-31","status":"IN_QUEUE","status_url":format!("{task}/status"),"response_url":task,"cancel_url":format!("{task}/cancel")})).unwrap()
            } else if url.ends_with("/status") {
                serde_json::to_vec(&if self.failure == 2 {
                    json!({"status":"COMPLETED","error":"content policy violation"})
                } else {
                    json!({"status":"COMPLETED"})
                })
                .unwrap()
            } else if url == task {
                let file = json!({"url":"https://v3.fal.media/files/public/grok-output"});
                serde_json::to_vec(&if self.video { json!({"video":file}) } else { json!({"images":[file.clone(),file]}) }).unwrap()
            } else {
                assert_eq!(url, "https://v3.fal.media/files/public/grok-output");
                if self.video { include_bytes!("../../providers/src/fal/fixtures/blue.mp4").to_vec() } else { image() }
            };
            Ok(HttpResponse { status: 200, headers: vec![], body })
        })
    }
}
fn image() -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[120; 4 * 4 * 4], &Default::default()).unwrap()
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

fn run(id: &str, references: u8, failure: u8) {
    run_routed(id, references, failure, 0);
}
fn run_routed(id: &str, references: u8, failure: u8, route_mode: u8) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let video = id.contains("video");
    let kind = if video { VIDEO_KIND } else { KIND };
    let mut params: nodecraft_types::ParamValues =
        [("model".into(), Value::Text(id.into())), ("prompt".into(), Value::Text("A blue circle moves across a plain background".into()))].into();
    if !video {
        params.insert("num_images".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    }
    let card = kinds.get(kind).unwrap().instantiate_instance(CARD, Default::default(), &params).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([31; 16]), "Public Grok fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "grok31".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host =
        RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let route: &'static str = match id {
        "fal.grok-image2" => nodecraft_providers::fal::GROK_IMAGE,
        "fal.grok-image2-edit" => nodecraft_providers::fal::GROK_EDIT,
        "fal.grok-video15" => nodecraft_providers::fal::GROK_VIDEO,
        _ => nodecraft_providers::fal::GROK_I2V,
    };
    let http = Arc::new(Transport { model: route, video, failure, requests: Mutex::default() });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let image = image();
    let router = NodeId::from_bytes([40; 16]);
    let tail = NodeId::from_bytes([41; 16]);
    if route_mode > 0 {
        for id in [router, tail] {
            let node = session
                .kinds()
                .get(nodecraft_nodes::router::ID)
                .unwrap()
                .instantiate_instance(id, nodecraft_nodes::router::config(nodecraft_types::Modality::Image), &Default::default())
                .unwrap();
            session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
        }
        for (n, a, b, port) in [(40, router, tail, "in"), (41, tail, CARD, "images")] {
            session
                .dispatch(
                    "edge.connect",
                    &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([n;16]),"from":{"node":a,"port":"out"},"to":{"node":b,"port":port},"order":0}}),
                )
                .unwrap();
        }
    }
    let mut folder_files = Vec::new();
    for index in 0..references {
        let source = NodeId::from_bytes([index + 2; 16]);
        import_files(
            &mut session,
            C,
            Position { x: -300.0, y: f32::from(index) * 200.0 },
            200.0,
            &[Incoming { name: "reference.png", bytes: &image }],
            &[source],
        )
        .unwrap();
        if route_mode == 2 {
            let node = session.document().canvases().get(&C).unwrap().nodes().get(&source).unwrap();
            folder_files.push((format!("reference-{index}.png"), node.params.get("asset").unwrap().clone()));
        } else {
            let role = if route_mode > 0 {
                "in"
            } else if video {
                "first_frame"
            } else {
                "images"
            };
            let target = if route_mode > 0 { router } else { CARD };
            session.dispatch("edge.connect",&json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([index+2;16]),"from":{"node":source,"port":"out"},"to":{"node":target,"port":role},"order":index}})).unwrap();
        }
    }
    if route_mode == 2 {
        let folder = NodeId::from_bytes([42; 16]);
        let config = nodecraft_nodes::folder::config(nodecraft_types::Modality::Image, "Public references", folder_files).unwrap();
        let node = session.kinds().get(nodecraft_nodes::folder::ID).unwrap().instantiate_instance(folder, config, &Default::default()).unwrap();
        session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
        session
            .dispatch(
                "edge.connect",
                &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([42;16]),"from":{"node":folder,"port":"files"},"to":{"node":router,"port":"in"},"order":0}}),
            )
            .unwrap();
    }
    let price = if video { 840_000 } else { 120_000 } + u64::from(references) * 10_000;
    let mut request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["complete"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], price, "full output plus reference-image fees: {quote}");
    assert!(session.dispatch("run.start", &request).is_err());
    assert!(http.requests.lock().unwrap().is_empty());
    request["accept"] = json!({"token":quote["token"],"amounts":{"USD":price},"partial":false,"unpriced_jobs":0});
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
    if failure > 0 {
        assert!(node.history.is_empty());
        assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
    } else {
        assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
        let outputs = &node.history.last().unwrap().outputs;
        let media: Vec<_> = if video {
            outputs.iter().collect()
        } else {
            let [Value::List(images)] = outputs.as_slice() else { panic!("image list expected") };
            images.iter().collect()
        };
        assert_eq!(media.len(), if video { 1 } else { 2 });
        for value in media {
            let Value::Media(asset) = value else { panic!("media expected") };
            assert_eq!(asset.mime.essence(), if video { "video/mp4" } else { "image/png" });
        }
    }
    let log = budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cost.micros, price);
    assert_eq!(entries[0].status, if failure > 0 { Charge::Unresolved } else { Charge::Actual }, "moderation failures retain their reservation");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == nodecraft_providers::HttpMethod::Post).count(), 1, "possible paid creates never replay");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    if references > 0 {
        if video {
            assert!(body["image_url"].is_string());
            assert!(body.get("aspect_ratio").is_none());
        } else {
            assert_eq!(body["image_urls"].as_array().unwrap().len(), usize::from(references));
        }
    }
    for r in requests.iter().filter(|r| r.url.starts_with("https://v3.fal.media")) {
        assert!(r.headers.is_empty());
    }
}
#[test]
fn grok31_actual_host_generates_images_and_videos_with_exact_quotes() {
    run("fal.grok-image2", 0, 0);
    run("fal.grok-video15", 0, 0);
}
#[test]
fn grok31_actual_host_quotes_and_resolves_all_edit_references_and_first_frame() {
    run("fal.grok-image2-edit", 5, 0);
    run("fal.grok-video15-i2v", 1, 0);
}
#[test]
fn grok31_actual_host_keeps_possible_moderation_fees_and_does_not_retry_lost_creates() {
    run("fal.grok-image2", 0, 1);
    run("fal.grok-image2-edit", 1, 2);
    run("fal.grok-video15-i2v", 1, 2);
}

#[test]
fn router949_paid_reference_fees_and_actual_submit_keep_all_five_images() {
    run_routed("fal.grok-image2-edit", 5, 0, 1);
    run_routed("fal.grok-image2-edit", 5, 0, 2);
}
