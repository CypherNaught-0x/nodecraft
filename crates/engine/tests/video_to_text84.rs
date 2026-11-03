//! Video to text and the declared context window on text cards (#84), through quote,
//! admission, the Driver, the mock provider and run.record. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generate,
    generation_catalog::GenerationCatalog,
    jobs::{CompletionDelivery, JobBudget, ProviderRunner},
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    run::RunOptions,
    scheduler::{JobId, JobState, Scheduler},
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::{JobInput, mock::MockProvider};
use nodecraft_types::{AssetRef, CanvasId, EdgeId, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const C: CanvasId = CanvasId::from_bytes([84; 16]);
const WATCHER: NodeId = NodeId::from_bytes([1; 16]);
const CLIP: NodeId = NodeId::from_bytes([2; 16]);
const MODEL: &str = "fixture.watcher";
/// An MP4 file's first bytes: enough to be stored and sent as one; nothing here decodes it.
const MP4: &[u8] = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41";

/// A text model with a small context window on its text mode and a video mode that reads a
/// frame a second, at most a minute of video.
fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-text"
label = "Fixture watcher"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
reads = {{ context_tokens = 4 }}
[[model.modes]]
id = "video-to-text"
output = "text"
inputs = [
  {{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }},
  {{ role = "video", modality = "video", min = 1, max = 2 }},
]
reads = {{ context_tokens = 1000, frames = {{ every_ms = 1000, max = 60 }} }}
"#
    ))
    .unwrap()
}

#[derive(Default)]
struct Immediate(Arc<AtomicU64>);
impl Runtime for Immediate {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let w = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_ready());
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let t = self.0.clone();
        Box::pin(async move {
            t.fetch_add(ms, Ordering::SeqCst);
        })
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    session: Session,
    captured: Arc<Mutex<Vec<PreparedRun>>>,
    mock: MockProvider,
}

/// A watcher card with `prompt`; `clip` wires a stored video of that type and length into its
/// video role.
fn fixture(prompt: &str, clip: Option<(&str, Option<u64>)>) -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([85; 16]), "Video to text").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let node = kinds
        .get(TEXT_KIND)
        .unwrap()
        .instantiate_instance(WATCHER, Default::default(), &[("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(prompt.into()))].into())
        .unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    let captured: Arc<Mutex<Vec<PreparedRun>>> = Arc::default();
    let sink = captured.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |p| {
            sink.lock().unwrap().push(p);
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    let mock = MockProvider::standard();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WATCHER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    if let Some((mime, duration_ms)) = clip {
        let hash = session.import_asset(MP4).unwrap();
        let asset = Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: MediaMeta { duration_ms, ..Default::default() } });
        let source = session.kinds().get("source.video").unwrap().instantiate_instance(CLIP, Default::default(), &[("asset".into(), asset)].into()).unwrap();
        session.dispatch("node.add", &json!({"canvas":C,"node":source})).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([3; 16]),
            from: Endpoint { node: CLIP, port: "out".into() },
            to: Endpoint { node: WATCHER, port: "video".into() },
            order: 0,
        };
        session.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    }
    Fixture { session, captured, mock }
}

fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[WATCHER]})
}

/// The one reason the quote is blocked.
fn blocked(f: &mut Fixture) -> String {
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    let [reason] = q["blocked"].as_array().unwrap().as_slice() else { panic!("one blocked reason: {q}") };
    reason.as_str().unwrap().to_owned()
}

/// Quote, accept, drive the one job through the mock with the session's asset store and record
/// it; returns the answer.
fn run(f: &mut Fixture) -> Value {
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let mut start = request();
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    f.session.dispatch("run.start", &start).unwrap();
    let prepared = f.captured.lock().unwrap().pop().unwrap();
    let (mut run, progress) =
        f.session.authorize_prepared_run(prepared, RunOptions { id: [86; 16], first_job: 1, limits: Default::default(), retry: Default::default() }).unwrap();
    let spec = &progress.jobs[0];
    let queued = run.queued_job_with_media(spec.id, &spec.idempotency_key, f.session.assets()).unwrap();
    let deliveries: Arc<Mutex<BTreeMap<JobId, CompletionDelivery>>> = Arc::default();
    let out = deliveries.clone();
    let runtime = Arc::new(Immediate::default());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "video-fixture".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let runner = Arc::new(
        ProviderRunner::new(
            runtime.clone(),
            Default::default(),
            Arc::new(move |id, d| {
                assert!(out.lock().unwrap().insert(id, d).is_none());
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(budget.clone()),
    );
    runner.register(spec.id, queued);
    let mut scheduler = Scheduler::new(Default::default());
    scheduler.add(progress.jobs.clone()).unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let end = futures::executor::block_on(driver.run(runner, runtime, |_| {}));
    assert_eq!(end.state(spec.id), Some(&JobState::Done));
    let delivery = deliveries.lock().unwrap().remove(&spec.id).unwrap();
    let nodecraft_nodes::JobSpec::Generate { outputs: routes, .. } = run.work(spec.id).unwrap() else { panic!("not a generation job") };
    let converted = generate::produced(routes, &delivery.completed).unwrap();
    let done = run.job_done(f.session.kinds(), spec.id, converted.outputs);
    assert!(done.failed.is_empty());
    let record = json!({"canvas":C,"results":[{"node":WATCHER,"generation":done.finished[0].generation}]});
    f.session.dispatch("run.record", &record).unwrap();
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&WATCHER).unwrap().clone();
    node.history[0].outputs[0].clone()
}

#[test]
fn a_wired_video_reaches_a_text_model_through_the_mock_and_its_answer_is_recorded() {
    let mut f = fixture("Describe the clip", Some(("video/mp4", Some(12_000))));
    let answer = run(&mut f);
    let Value::Text(answer) = answer else { panic!("a text answer") };
    assert!(answer.starts_with("mock-text: Describe the clip #"), "{answer}");
    let [job] = f.mock.requests().try_into().unwrap_or_else(|r: Vec<_>| panic!("one request: {r:?}"));
    let [JobInput::Text { role, text }, JobInput::Media { role: video, mime, bytes }] = job.inputs.as_slice() else { panic!("{:?}", job.inputs) };
    assert_eq!((role.as_str(), text.as_str()), ("prompt", "Describe the clip"));
    assert_eq!((video.as_str(), mime.as_str(), bytes.as_ref()), ("video", "video/mp4", MP4), "the stored video's bytes are sent as the video role");
}

#[test]
fn a_video_longer_than_the_mode_reads_is_refused_before_anything_is_sent() {
    let mut f = fixture("Describe the clip", Some(("video/mp4", Some(61_000))));
    let reason = blocked(&mut f);
    assert!(reason.contains(&WATCHER.to_string()), "names the card: {reason}");
    assert!(reason.contains("the video video is 61 s long") && reason.contains("at most 60 s") && reason.contains("trim it first"), "{reason}");
    assert!(f.mock.requests().is_empty());
    // A video whose length isn't known goes through; the provider checks it.
    let mut f = fixture("Describe the clip", Some(("video/mp4", None)));
    assert!(matches!(run(&mut f), Value::Text(_)));
}

#[test]
fn a_prompt_clearly_past_the_declared_window_is_refused_before_it_is_quoted_or_sent() {
    // The text mode reads 4 tokens: 24 bytes at the estimate's six bytes a token.
    let mut f = fixture(&"x".repeat(25), None);
    let reason = blocked(&mut f);
    assert!(nodecraft_providers::is_context_limit_text(&reason), "reads as a context refusal: {reason}");
    assert!(reason.contains(&WATCHER.to_string()) && reason.contains(MODEL) && reason.contains("reads at most 4"), "names the card and model: {reason}");
    assert!(reason.contains(nodecraft_providers::CONTEXT_LIMIT_FIX), "says what to do: {reason}");
    let mut start = request();
    start["accept"] = json!({"token":"none","partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    assert!(f.session.dispatch("run.start", &start).is_err(), "nor can it start");
    assert!(f.captured.lock().unwrap().is_empty() && f.mock.requests().is_empty(), "nothing reached the host or the provider");
    // At the window it runs: the estimate refuses only what is clearly over.
    let mut f = fixture(&"x".repeat(24), None);
    assert!(matches!(run(&mut f), Value::Text(_)));
    assert_eq!(f.mock.requests().len(), 1);
}
