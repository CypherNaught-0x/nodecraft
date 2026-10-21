//! Paid results of a fanned step are held, not dropped (#611): when a batch of prompts runs
//! through one card and its result can't be recorded, every item's result is held as one, to be
//! put on the canvas or discarded; an item that failed doesn't take its siblings' results with it.
//! None of this calls the provider again. A canned provider only; no live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Generation, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, Completed, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([71; 16]);
const BATCH: NodeId = NodeId::from_bytes([1; 16]);
const PAINTER: NodeId = NodeId::from_bytes([2; 16]);
const MODEL: &str = "fixture.painter";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "canned-image"
label = "Fixture painter"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .unwrap()
}

/// Each task on its own thread; sleeps on a timer thread. Real concurrency, like the app's.
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

/// A prompt and the outputs it gets.
type Answer = (String, Vec<Output>);

/// Answers the prompt it is sent from `answers` (by prompt text), counting requests. While gated,
/// jobs queue and run until the gate opens.
#[derive(Clone, Default)]
struct Canned {
    answers: Arc<Mutex<Vec<Answer>>>,
    requests: Arc<AtomicUsize>,
    gated: Arc<AtomicBool>,
}

impl Canned {
    fn answer(&self, prompt: &str, outputs: Vec<Output>) {
        self.answers.lock().unwrap().push((prompt.into(), outputs));
    }
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
    fn result(&self, prompt: &str) -> Completed {
        let answers = self.answers.lock().unwrap();
        Completed::new(answers.iter().find(|(p, _)| p == prompt).map(|(_, outputs)| outputs.clone()).unwrap_or_default())
    }
}

fn prompt_of(job: &ProviderJob) -> String {
    job.inputs
        .iter()
        .find_map(|i| match i {
            nodecraft_providers::JobInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

impl Provider for Canned {
    fn id(&self) -> &str {
        "mock"
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let prompt = prompt_of(job);
        let submission = if self.gated.load(Ordering::SeqCst) {
            Submission::Queued(JobHandle { remote_id: prompt, data: serde_json::Value::Null })
        } else {
            Submission::Finished(self.result(&prompt))
        };
        Box::pin(async move { Ok(submission) })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let progress = if self.gated.load(Ordering::SeqCst) {
            JobProgress::Running { fraction: None, message: None, partial: None }
        } else {
            JobProgress::Done(self.result(&handle.remote_id))
        };
        Box::pin(async move { Ok(progress) })
    }
    fn cancel<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
    fn poll_interval_ms(&self, _progress: &JobProgress) -> u64 {
        5
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    canned: Canned,
    budget: JobBudget,
}

/// A batch of two prompts into an image card: one job per prompt.
fn fixture() -> Fixture {
    let canned = Canned::default();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([72; 16]), "Held fan-out").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(String::new()))].into();
    let painter = kinds.get(KIND).unwrap().instantiate_instance(PAINTER, Default::default(), &values).unwrap();
    let mut batch = kinds.get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(BATCH, Default::default(), &Default::default()).unwrap();
    batch.params.insert("text".into(), Value::Text("a fox\nan owl".into()));
    for node in [batch, painter] {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "held-fanout".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(canned.clone())).unwrap();
    routing.select(PAINTER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut f = Fixture { session, host, canned, budget };
    let edge = json!({"id": EdgeId::from_bytes([10; 16]), "from": {"node": BATCH, "port": "item"}, "to": {"node": PAINTER, "port": "prompt"}, "order": 0});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    f
}

/// Quotes and accepts a run of the painter.
fn start(f: &mut Fixture) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[PAINTER]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":q["complete"] != true,"unpriced_jobs":0,"amounts":amounts});
    f.session.dispatch("run.start", &start).unwrap();
}

/// Pumps like a frame loop until the host is idle.
fn drive(f: &mut Fixture) -> Vec<RunNotice> {
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            return notices;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Starts the run with the provider gated, removes the painter once both prompts are sent, then
/// lets the provider answer.
fn run_then_remove_the_card(f: &mut Fixture) -> Vec<RunNotice> {
    f.canned.gated.store(true, Ordering::SeqCst);
    start(f);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut notices = Vec::new();
    while f.canned.requests() < 2 {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline, "the prompts were never sent");
        std::thread::sleep(Duration::from_millis(1));
    }
    f.session.dispatch("node.remove", &json!({"canvas": C, "id": PAINTER})).unwrap();
    f.canned.gated.store(false, Ordering::SeqCst);
    notices.extend(drive(f));
    notices
}

fn failed(notices: &[RunNotice], node: NodeId) -> Option<String> {
    notices.iter().find_map(|n| match n {
        RunNotice::Failed { node: failed, reason, .. } if *failed == node => Some(reason.clone()),
        _ => None,
    })
}

fn history(f: &Fixture) -> Vec<Generation> {
    f.session.document().canvases().get(&C).unwrap().nodes().get(&PAINTER).map(|n| n.history.clone()).unwrap_or_default()
}

/// The kinds of the nodes on the canvas besides the batch and the painter.
fn placed(f: &Fixture) -> Vec<String> {
    let canvas = f.session.document().canvases().get(&C).unwrap();
    canvas.nodes().values().filter(|n| n.id != BATCH && n.id != PAINTER).map(|n| n.kind.clone()).collect()
}

fn image() -> Output {
    let pixels: Vec<u8> = (0..4u32 * 4).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    let png = nodecraft_media::encode_png(4, 4, &pixels, &nodecraft_media::Limits::default()).unwrap();
    Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png) }
}

fn charges(f: &Fixture) -> usize {
    f.budget.log.lock().unwrap().entries().count()
}

/// Asks for `command` on held result `id`, then pumps until it is no longer pending.
fn act(f: &mut Fixture, command: &str, id: u64) -> Vec<RunNotice> {
    f.session.dispatch(command, &json!({"id": id})).unwrap();
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.host.held_results().iter().any(|h| h.id == id && h.pending.is_some()) {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline, "{command} never settled");
    }
    notices
}

#[test]
fn a_fanned_result_that_is_recorded_holds_nothing() {
    let mut f = fixture();
    f.canned.answer("a fox", vec![image()]);
    f.canned.answer("an owl", vec![image()]);
    start(&mut f);
    let notices = drive(&mut f);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: PAINTER }), "{notices:?}");
    assert_eq!(history(&f)[0].items.len(), 2);
    assert!(f.host.held_results().is_empty(), "{:?}", f.host.held_results());
}

#[test]
fn a_fanned_result_that_cant_be_recorded_holds_every_items_result_as_one() {
    let mut f = fixture();
    f.canned.answer("a fox", vec![image()]);
    f.canned.answer("an owl", vec![image(), image()]);
    let notices = run_then_remove_the_card(&mut f);
    let reason = failed(&notices, PAINTER).unwrap_or_else(|| panic!("{notices:?}"));
    assert!(reason.contains("the paid result is held"), "{reason}");
    assert_eq!((f.canned.requests(), charges(&f)), (2, 2), "both prompts are paid for");

    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("{held:?}") };
    assert_eq!((view.canvas, view.node, view.outputs), (C, PAINTER, 3), "every item's images, in one");
    assert!(!view.retry, "a fanned step can't be recorded from it alone");
    let refused = f.session.dispatch("run.held_retry", &json!({"id": view.id})).unwrap_err();
    assert!(refused.message.contains("put on the canvas or discarded"), "{}", refused.message);

    // The card is gone: undoing its removal lets the images be placed beside it.
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    let notices = act(&mut f, "run.held_place", view.id);
    assert!(notices.contains(&RunNotice::Placed { canvas: C, node: PAINTER }), "{notices:?}");
    assert_eq!(placed(&f), vec!["source.image"; 3]);
    assert!(f.host.held_results().is_empty());
    assert_eq!((f.canned.requests(), charges(&f)), (2, 2), "placing calls no provider and charges nothing");
}

#[test]
fn an_item_that_fails_doesnt_take_its_siblings_result_with_it() {
    let mut f = fixture();
    // The owl's answer isn't an image: its item fails and its result is held on its own (#369).
    f.canned.answer("a fox", vec![image()]);
    f.canned.answer("an owl", vec![Output::Text("not an image".into())]);
    run_then_remove_the_card(&mut f);
    let held = f.host.held_results();
    assert_eq!(held.len(), 2, "the owl's unconvertible result and the fox's unrecorded one: {held:?}");
    assert_eq!(held.iter().map(|h| h.outputs).sum::<usize>(), 2);
    assert!(held.iter().all(|h| h.node == PAINTER && !h.retry), "{held:?}");
    for view in held {
        act(&mut f, "run.held_discard", view.id);
    }
    assert!(f.host.held_results().is_empty());
    assert_eq!((f.canned.requests(), charges(&f)), (2, 2));
}
