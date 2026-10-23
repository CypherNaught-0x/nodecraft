//! Two ComfyUI servers at once (#864): each server is its own provider instance, so two cards on
//! one canvas run on different servers, and a server's "jobs at once" caps how many of a run's
//! jobs it holds at a time. Fake servers only; no live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    provider_setup::comfy_instance,
    run_cmds::RunCatalog,
    run_host::RunHost,
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, Completed, ErrorKind, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([86; 16]);
const BATCH: NodeId = NodeId::from_bytes([1; 16]);
const STUDIO: NodeId = NodeId::from_bytes([2; 16]);
const LAPTOP: NodeId = NodeId::from_bytes([3; 16]);
const MODEL: &str = "fixture.comfy";
/// Polls a job takes to finish, so a server's jobs overlap when nothing caps them.
const POLLS: usize = 4;

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "comfyui"
provider_model = "fixture"
label = "Fixture workflow"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .unwrap()
}

/// Each task on its own thread; sleeps on a timer thread.
struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
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

/// A ComfyUI server that queues each job and finishes it after [`POLLS`] polls, noting how many
/// it held at once.
#[derive(Clone, Default)]
struct Server {
    jobs: Arc<Mutex<BTreeMap<String, usize>>>,
    received: Arc<Mutex<usize>>,
    most_at_once: Arc<Mutex<usize>>,
}

impl Server {
    fn received(&self) -> usize {
        *self.received.lock().unwrap()
    }
    fn most_at_once(&self) -> usize {
        *self.most_at_once.lock().unwrap()
    }
}

impl Provider for Server {
    fn id(&self) -> &str {
        "comfyui"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        let key = job.idempotency_key.clone();
        let mut jobs = self.jobs.lock().unwrap();
        jobs.insert(key.clone(), 0);
        *self.received.lock().unwrap() += 1;
        let mut most = self.most_at_once.lock().unwrap();
        *most = (*most).max(jobs.len());
        Box::pin(async move { Ok(Submission::Queued(JobHandle { remote_id: key, data: serde_json::Value::Null })) })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let mut jobs = self.jobs.lock().unwrap();
        let progress = match jobs.get_mut(&handle.remote_id) {
            None => Err(ProviderError::new(ErrorKind::NotFound, "no such job")),
            Some(polls) if *polls + 1 >= POLLS => {
                jobs.remove(&handle.remote_id);
                Ok(JobProgress::Done(Completed::new(vec![image()])))
            }
            Some(polls) => {
                *polls += 1;
                Ok(JobProgress::Running { fraction: None, message: None, partial: None })
            }
        };
        Box::pin(async move { progress })
    }
    fn cancel<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
    fn poll_interval_ms(&self, _progress: &JobProgress) -> u64 {
        5
    }
}

fn image() -> Output {
    let pixels: Vec<u8> = (0..4u32 * 4).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    let png = nodecraft_media::encode_png(4, 4, &pixels, &nodecraft_media::Limits::default()).unwrap();
    Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png) }
}

struct Fixture {
    session: Session,
    host: RunHost,
    studio: Server,
    laptop: Server,
}

/// A batch of three prompts into two cards: one on `studio` (one job at once), one on `laptop`
/// (no cap).
fn fixture() -> Fixture {
    let (studio, laptop) = (Server::default(), Server::default());
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([87; 16]), "Two servers").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(String::new()))].into();
    let mut batch = kinds.get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(BATCH, Default::default(), &Default::default()).unwrap();
    batch.params.insert("text".into(), Value::Text("a fox\nan owl\na hare".into()));
    Change::AddNode { canvas: C, node: batch }.apply(&mut project).unwrap();
    for card in [STUDIO, LAPTOP] {
        let node = kinds.get(KIND).unwrap().instantiate_instance(card, Default::default(), &values).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "two-servers".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    for (card, server, name, limit) in [(STUDIO, &studio, "studio", Some(1)), (LAPTOP, &laptop, "laptop", None)] {
        let binding = routing.bind_named_with_limit(Arc::new(server.clone()), comfy_instance(name).unwrap(), limit).unwrap();
        routing.select(card, "comfyui", binding.target()).unwrap();
    }
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    for (n, card) in [STUDIO, LAPTOP].into_iter().enumerate() {
        let edge =
            json!({"id": EdgeId::from_bytes([10 + n as u8; 16]), "from": {"node": BATCH, "port": "item"}, "to": {"node": card, "port": "prompt"}, "order": 0});
        session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    Fixture { session, host, studio, laptop }
}

fn run(f: &mut Fixture) {
    let request = json!({"canvas": C, "mode": "selected", "nodes": [STUDIO, LAPTOP]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != true, "unpriced_jobs": 0, "amounts": amounts});
    f.session.dispatch("run.start", &start).unwrap();
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            return;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn finished(f: &Fixture, card: NodeId) -> bool {
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&card).unwrap();
    node.history.iter().any(|g| Some(g.id) == node.active && g.incomplete.is_none())
}

#[test]
fn two_cards_run_on_two_servers_each_within_its_cap() {
    let mut f = fixture();
    run(&mut f);
    assert_eq!(f.studio.received(), 3, "the studio card's jobs went to the studio server only");
    assert_eq!(f.laptop.received(), 3, "the laptop card's jobs went to the laptop server only");
    assert_eq!(f.studio.most_at_once(), 1, "studio takes one job at a time");
    assert!(f.laptop.most_at_once() > 1, "laptop has no cap, so its jobs overlap");
    assert!(finished(&f, STUDIO) && finished(&f, LAPTOP));
}
