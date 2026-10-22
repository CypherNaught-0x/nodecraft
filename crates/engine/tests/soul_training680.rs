//! Higgsfield Soul ID training (#680) through the engine: a Train style card on the shipped
//! trainer, fed two pictures, is quoted at Higgsfield's price before anything is sent, trains
//! through the real Higgsfield adapter over a scripted transport, and keeps a descriptor naming the
//! Soul ID. A POST whose reply is lost is found on the account, never sent twice. No network.
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
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::STYLE_KIND;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, higgsfield::Higgsfield, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([68; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const PICTURES: [NodeId; 2] = [NodeId::from_bytes([2; 16]), NodeId::from_bytes([3; 16])];
const SOUL_ID: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms.min(5)));
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

/// Higgsfield's account as the adapter sees it: the references it has, the uploads, creation
/// and status. With `lose_first`, the first creation is made but its reply is lost.
#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
    /// The name each Soul ID on the account was created with.
    created: Mutex<Vec<String>>,
    lose_first: bool,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let mut requests = self.requests.lock().unwrap();
        let mut created = self.created.lock().unwrap();
        let n = requests.len();
        let creation = r.method == HttpMethod::Post && r.url.ends_with("/v1/custom-references");
        let reply = if r.url.ends_with("/files/generate-upload-url") {
            json!({"public_url": format!("https://upload.cdn.example.com/inputs/{n}"), "upload_url": "https://upload.storage.example.com/put", "content_type": "image/png"})
        } else if r.method == HttpMethod::Put {
            json!({})
        } else if r.url.contains("/v1/custom-references/list?page=1&") {
            let items: Vec<_> = created.iter().map(|name| json!({"id": SOUL_ID, "name": name, "status": "queued"})).collect();
            json!({"items": items})
        } else if r.url.contains("/v1/custom-references/list?") {
            json!({"items": []})
        } else if creation {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            let name = body["name"].as_str().unwrap().to_owned();
            created.push(name.clone());
            json!({"id": SOUL_ID, "name": name, "status": "not_ready"})
        } else if r.url.ends_with(&format!("/v1/custom-references/{SOUL_ID}")) {
            json!({"id": SOUL_ID, "name": created.first().cloned(), "status": "completed"})
        } else {
            panic!("unexpected request {}", r.url)
        };
        requests.push(r);
        if creation && self.lose_first && created.len() == 1 {
            return Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "connection reset")) });
        }
        let body = serde_json::to_vec(&reply).unwrap();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    transport: Arc<Transport>,
}

/// A Train style card on Higgsfield's Soul ID trainer, named Mara, fed two pictures.
fn fixture(lose_first: bool) -> Fixture {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([68; 16]), "Soul ID").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("higgsfield.soul-id-trainer".into())), ("name".into(), Value::Text("Mara".into()))].into();
    let card = kinds.get(STYLE_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "soul".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 3, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let transport = Arc::new(Transport { lose_first, ..Default::default() });
    let mut routing = ProviderRouting::new();
    let key = Secret::new("fixture-key-id-0001:fixture-secret-not-real-0001").unwrap();
    let binding = routing.bind_default(Arc::new(Higgsfield::new(key, transport.clone()))).unwrap();
    routing.select(CARD, "higgsfield", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let pngs: Vec<Vec<u8>> = [40u8, 200].iter().map(|shade| nodecraft_media::encode_png(4, 4, &[*shade; 64], &Default::default()).unwrap()).collect();
    let files: Vec<Incoming<'_>> = pngs.iter().map(|bytes| Incoming { name: "face.png", bytes }).collect();
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &files, &PICTURES).unwrap();
    for (n, picture) in PICTURES.iter().enumerate() {
        let edge =
            json!({"id": EdgeId::from_bytes([10 + n as u8; 16]), "from": {"node": picture, "port": "out"}, "to": {"node": CARD, "port": "images"}, "order": n});
        session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    Fixture { session, host, transport }
}

/// Quotes the card at 2.50 USD, accepts and runs it to the end.
fn run(f: &mut Fixture) -> Vec<RunNotice> {
    let request = json!({"canvas": C, "mode": "selected", "nodes": [CARD]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!((q["allowed"].as_bool(), q["complete"].as_bool()), (Some(true), Some(true)), "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], 2_500_000, "the price is shown before the run: {q}");
    assert!(f.transport.requests.lock().unwrap().is_empty(), "quoting sends nothing");
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": false, "unpriced_jobs": 0, "amounts": {"USD": 2_500_000}});
    f.session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    notices
}

/// The card's trained Soul ID, from its active result.
fn trained(f: &Fixture) -> nodecraft_media::style::Style {
    let card = f.session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    let result = card.history.iter().find(|g| Some(g.id) == card.active).expect("an active result");
    let asset = match result.outputs.as_slice() {
        [Value::Media(asset)] => asset.clone(),
        [Value::List(items)] => match items.as_slice() {
            [Value::Media(asset)] => asset.clone(),
            other => panic!("one asset: {other:?}"),
        },
        other => panic!("one asset: {other:?}"),
    };
    assert_eq!(asset.mime.essence(), nodecraft_media::style::MIME);
    nodecraft_media::style::decode(f.session.assets().get(&asset.hash).expect("the descriptor is stored")).unwrap()
}

fn posts(f: &Fixture) -> Vec<serde_json::Value> {
    let requests = f.transport.requests.lock().unwrap();
    requests
        .iter()
        .filter(|r| r.method == HttpMethod::Post && r.url.ends_with("/v1/custom-references"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[test]
fn a_soul_id_is_priced_first_trained_from_the_wired_pictures_and_kept_as_a_descriptor() {
    let mut f = fixture(false);
    let notices = run(&mut f);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let style = trained(&f);
    assert_eq!((style.provider.as_str(), style.family.as_str()), ("higgsfield", "soul-2"));
    assert_eq!(style.weights, format!("https://api.higgsfield.ai/v1/custom-references/{SOUL_ID}"));
    let [body] = posts(&f).try_into().unwrap();
    assert!(body["name"].as_str().unwrap().starts_with("Mara [nc-"), "{body}");
    assert_eq!(body["model_version"], "v2");
    assert_eq!(body["input_images"].as_array().unwrap().len(), 2, "both pictures are the dataset");
}

#[test]
fn a_lost_creation_reply_is_found_on_the_account_and_paid_for_once() {
    let mut f = fixture(true);
    let notices = run(&mut f);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    assert_eq!(trained(&f).weights, format!("https://api.higgsfield.ai/v1/custom-references/{SOUL_ID}"));
    assert_eq!(posts(&f).len(), 1, "the retry listed the account instead of training again");
}
