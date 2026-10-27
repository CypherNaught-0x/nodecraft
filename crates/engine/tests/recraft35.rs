//! Recraft runs through consent, the real fal queue, typed history, persistence and export.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::RunHost,
};
use nodecraft_exec::{
    cost::Charge,
    driver::{Runtime, TaskFuture},
    run::ActionExecutor,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, NodeId, ParamValues, ProjectId, Unsigned, Value};
use serde_json::{Value as Json, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([35; 16]);
const N: NodeId = NodeId::from_bytes([1; 16]);
pub const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#663399"/><circle cx="100" cy="50" r="30" fill="#44ddaa"/></svg>"##;
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
    vector: bool,
    lost: bool,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects prohibited")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let method = r.method;
        let url = r.url.clone();
        self.requests.lock().unwrap().push(r);
        Box::pin(async move {
            let root = "https://queue.fal.run/fal-ai/recraft/requests/public-35";
            let value = if method == HttpMethod::Post {
                if self.lost {
                    return Err(ProviderError::new(ErrorKind::Network, "lost paid reply"));
                }
                json!({"request_id":"public-35","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})
            } else if url == format!("{root}/status") {
                json!({"status":"COMPLETED"})
            } else if url == root {
                json!({"images":[{"url":"https://v3.fal.media/public-35/result","content_type":if self.vector {"image/svg+xml"}else{"image/png"}}]})
            } else {
                assert_eq!(url, "https://v3.fal.media/public-35/result");
                let body = if self.vector { SVG.to_vec() } else { nodecraft_media::encode_png(2, 2, &[150; 16], &Default::default()).unwrap() };
                return Ok(HttpResponse { status: 200, headers: vec![], body });
            };
            Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() })
        })
    }
}

#[test]
fn recraft35_real_runhost_consent_typed_svg_roundtrip_extraction_and_original_export() {
    for vector in [false, true] {
        for lost in [false, true] {
            let snapshot = GenerationCatalog::builtin().unwrap();
            let kinds = snapshot.registry().unwrap();
            let id = if vector { "fal.recraft41-vector" } else { "fal.recraft41" };
            let values: ParamValues = [("model".into(), Value::Text(id.into())), ("prompt".into(), Value::Text("An authored geometric logo".into()))].into();
            let card = kinds.get(nodecraft_nodes::generation::KIND).unwrap().instantiate_instance(N, Default::default(), &values).unwrap();
            let mut project = Project::new(ProjectId::from_bytes([35; 16]), "Public vector fixture").unwrap();
            Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
            Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
            let mut resources = Resources::default();
            resources.insert(snapshot.clone());
            resources.insert(snapshot.run_catalog());
            let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "recraft35".into(), wall_clock_ms: Arc::new(|| 0) };
            let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone());
            let mut services = Services::default();
            host.register(&mut services).unwrap();
            let wire = Arc::new(Wire { vector, lost, requests: Mutex::default() });
            let mut routing = ProviderRouting::new();
            let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), wire.clone()))).unwrap();
            routing.select(N, "fal", binding.target()).unwrap();
            let mut session =
                Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
            session.set_provider_routing(routing).unwrap();
            host.install(&mut session);
            let price = if vector { 80_000 } else { 35_000 };
            let mut request = json!({"canvas":C,"mode":"selected","nodes":[N]});
            let quote = session.dispatch("run.quote", &request).unwrap();
            assert_eq!(quote["totals"]["USD"]["micros"], price);
            assert_eq!(quote["complete"], true);
            assert!(session.dispatch("run.start", &request).is_err());
            assert!(wire.requests.lock().unwrap().is_empty());
            request["accept"] = json!({"token":quote["token"],"amounts":{"USD":price},"partial":false,"unpriced_jobs":0});
            session.dispatch("run.start", &request).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while host.is_busy() {
                host.pump(&mut session);
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            let entries = budget.log.lock().unwrap();
            let entries: Vec<_> = entries.entries().collect();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].cost.micros, price);
            assert_eq!(entries[0].status, if lost { Charge::Unresolved } else { Charge::Actual });
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Post).count(), 1);
            assert!(requests.iter().filter(|r| r.url.starts_with("https://v3.fal.media")).all(|r| r.headers.is_empty()));
            let node = session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
            if lost {
                assert!(node.history.is_empty());
                assert_eq!(requests.len(), 1);
                continue;
            }
            let result = node.history.last().unwrap();
            let generation = result.id;
            let Value::List(values) = &result.outputs[0] else { panic!("image list") };
            let Value::Media(asset) = &values[0] else { panic!("typed image") };
            let asset = asset.clone();
            assert_eq!(asset.mime.essence(), if vector { "image/svg+xml" } else { "image/png" });
            if vector {
                assert_eq!(session.assets().get(&asset.hash).unwrap(), SVG);
                assert_eq!((asset.meta.width, asset.meta.height), (Some(200), Some(100)));
                assert_eq!(nodecraft_media::svg::render(SVG, 128, &Default::default()).unwrap().width, 128);
                let temp = tempfile::tempdir().unwrap();
                let path = temp.path().join("original.svg");
                session.dispatch("history.export", &json!({"canvas":C,"id":N,"generation":generation,"item":0,"path":path})).unwrap();
                assert_eq!(std::fs::read(path).unwrap(), SVG);
                let out = NodeId::from_bytes([2; 16]);
                session.dispatch("history.extract", &json!({"canvas":C,"id":N,"generation":generation,"item":0,"node":out,"pos":{"x":300,"y":0}})).unwrap();
                assert_eq!(session.document().canvases().get(&C).unwrap().nodes().get(&out).unwrap().kind, "source.image");
                session.dispatch("edit.undo", &Json::Null).unwrap();
                session.dispatch("edit.redo", &Json::Null).unwrap();
                let archive =
                    nodecraft_format::encode(&nodecraft_format::Snapshot { project: session.document().clone(), assets: session.assets().clone(), epoch: 0 })
                        .unwrap();
                let path = temp.path().join("project.ncraft");
                std::fs::write(&path, archive).unwrap();
                let reopened = nodecraft_format::decode(&std::fs::read(path).unwrap()).unwrap();
                assert_eq!(reopened.assets.get(&asset.hash).unwrap(), SVG);
                reopened.assets.validate_reference(&asset).unwrap();
            }
        }
    }
}

#[test]
fn recraft35_import_and_explicit_rasterize_action_preserve_original_and_reject_implicit_conversion() {
    let mut project = Project::new(ProjectId::from_bytes([35; 16]), "SVG source").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let added = nodecraft_engine::import::import_files(
        &mut session,
        C,
        Position { x: 0.0, y: 0.0 },
        200.0,
        &[nodecraft_engine::import::Incoming { name: "original.svg", bytes: SVG }],
        &[N],
    )
    .unwrap();
    assert_eq!(added.nodes, vec![N]);
    let node = session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    assert!(nodecraft_engine::split::preview_image_of(node).is_some());
    assert!(nodecraft_engine::split::image_of(node).is_none(), "raster-only split/mask admission stays unchanged");
    assert_eq!(nodecraft_engine::split::image_rows(node).1, 1);
    let source = node.params.get("asset").unwrap().clone();
    let actions = nodecraft_engine::action_exec::LocalActions::new(session.assets().clone());
    let params: ParamValues = [("edge".into(), Value::Unsigned(Unsigned(256)))].into();
    let converted = actions.execute(nodecraft_nodes::svg::RASTERIZE, &params, std::slice::from_ref(&source)).unwrap();
    let Value::Media(png) = converted else { panic!() };
    assert_eq!(png.mime.essence(), "image/png");
    assert_eq!((png.meta.width, png.meta.height), (Some(256), Some(128)));
    assert!(actions.execute("image.resize", &Default::default(), std::slice::from_ref(&source)).is_err());
    let Value::Media(original) = source else { panic!() };
    assert_eq!(session.assets().get(&original.hash).unwrap(), SVG);
    assert!(actions.execute(nodecraft_nodes::svg::RASTERIZE, &[("edge".into(), Value::Unsigned(Unsigned(2049)))].into(), &[Value::Media(original)]).is_err());
}
