//! Held results survive a restart (#614): a paid result the run couldn't keep is saved, and a new
//! host on the same store holds it again, to be recorded, put on the canvas or discarded, without
//! calling the provider again. Damaged records are set aside, never a panic. A canned provider
//! only; no live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    held_cmds::MAX_HELD,
    held_store::{HeldStore, Listing, MemoryHeld},
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

const C: CanvasId = CanvasId::from_bytes([73; 16]);
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
    /// A prompt that stays running whatever the gate.
    stuck: Arc<Mutex<Option<String>>>,
}

impl Canned {
    fn answer(&self, prompt: &str, outputs: Vec<Output>) {
        self.answers.lock().unwrap().push((prompt.into(), outputs));
    }
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
    fn waits(&self, prompt: &str) -> bool {
        self.gated.load(Ordering::SeqCst) || self.stuck.lock().unwrap().as_deref() == Some(prompt)
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
        let submission = if self.waits(&prompt) {
            Submission::Queued(JobHandle { remote_id: prompt, data: serde_json::Value::Null })
        } else {
            Submission::Finished(self.result(&prompt))
        };
        Box::pin(async move { Ok(submission) })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let progress = if self.waits(&handle.remote_id) {
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
    /// What opening the store said.
    opened: Result<(), nodecraft_types::Error>,
}

/// An image card prompted "a fox", or, `batched`, fed a batch of two prompts (one job each),
/// its host keeping held results in `store`.
fn fixture(batched: bool, store: &MemoryHeld) -> Fixture {
    let f = fixture_on(batched, Box::new(store.clone()));
    assert!(f.opened.is_ok(), "{:?}", f.opened);
    f
}

fn fixture_on(batched: bool, store: Box<dyn HeldStore>) -> Fixture {
    let canned = Canned::default();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([74; 16]), "Held restart").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let prompt = if batched { "" } else { "a fox" };
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(prompt.into()))].into();
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
        session: "held-restart".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let opened = host.open_held(store);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(canned.clone())).unwrap();
    routing.select(PAINTER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut f = Fixture { session, host, canned, budget, opened };
    if !batched {
        return f;
    }
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

/// Starts the run with the provider gated, removes the painter once its `jobs` prompts are sent,
/// then lets the provider answer.
fn run_then_remove_the_card(f: &mut Fixture, jobs: usize) -> Vec<RunNotice> {
    f.canned.gated.store(true, Ordering::SeqCst);
    start(f);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut notices = Vec::new();
    while f.canned.requests() < jobs {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline, "the prompts were never sent");
        std::thread::sleep(Duration::from_millis(1));
    }
    f.session.dispatch("node.remove", &json!({"canvas": C, "id": PAINTER})).unwrap();
    f.canned.gated.store(false, Ordering::SeqCst);
    notices.extend(drive(f));
    notices
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

/// The store's records, read as a new host would.
fn saved(store: &MemoryHeld) -> Vec<u64> {
    store.list().unwrap().records.into_iter().map(|(id, _)| id).collect()
}

#[test]
fn a_held_result_is_recorded_after_a_restart_without_calling_the_provider() {
    let store = MemoryHeld::default();
    let mut f = fixture(false, &store);
    f.canned.answer("a fox", vec![image()]);
    run_then_remove_the_card(&mut f, 1);
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("{held:?}") };
    assert!(view.retry && view.saved, "{view:?}");
    assert_eq!(saved(&store).len(), 1);
    drop(f);

    // A new launch, with the card back (the project as last saved).
    let mut f = fixture(false, &store);
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("{held:?}") };
    assert_eq!((view.canvas, view.node, view.outputs, view.retry, view.saved), (C, PAINTER, 1, true, true));
    let listed = f.session.dispatch("run.held", &json!({})).unwrap();
    assert_eq!(listed["held"][0]["saved"], true, "{listed}");
    let notices = act(&mut f, "run.held_retry", view.id);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: PAINTER }), "{notices:?}");
    assert_eq!(history(&f).len(), 1);
    assert!(f.host.held_results().is_empty());
    assert_eq!(saved(&store).len(), 0, "a recorded result's record is removed");
    assert_eq!((f.canned.requests(), charges(&f)), (0, 0), "nothing is sent or charged again");
}

#[test]
fn held_results_are_placed_and_discarded_after_a_restart() {
    let store = MemoryHeld::default();
    let mut f = fixture(true, &store);
    f.canned.answer("a fox", vec![image()]);
    f.canned.answer("an owl", vec![image(), image()]);
    run_then_remove_the_card(&mut f, 2);
    let mut f2 = fixture(false, &store);
    f2.canned.answer("a fox", vec![image()]);
    run_then_remove_the_card(&mut f2, 1);
    assert_eq!(saved(&store).len(), 2);
    drop((f, f2));

    let mut f = fixture(true, &store);
    let held = f.host.held_results();
    let [fanned, single] = held.as_slice() else { panic!("{held:?}") };
    assert_eq!((fanned.outputs, fanned.retry, single.outputs, single.retry), (3, false, 1, true), "in the order they were held");
    let notices = act(&mut f, "run.held_place", fanned.id);
    assert!(notices.contains(&RunNotice::Placed { canvas: C, node: PAINTER }), "{notices:?}");
    assert_eq!(placed(&f), vec!["source.image"; 3]);
    act(&mut f, "run.held_discard", single.id);
    assert!(f.host.held_results().is_empty());
    assert_eq!(saved(&store).len(), 0);
    assert_eq!((f.canned.requests(), charges(&f)), (0, 0));

    // Nothing comes back on the launch after.
    let f = fixture(true, &store);
    assert!(f.host.held_results().is_empty());
}

#[test]
fn a_damaged_record_is_set_aside_and_the_rest_restored() {
    let store = MemoryHeld::default();
    let mut f = fixture(false, &store);
    f.canned.answer("a fox", vec![image()]);
    run_then_remove_the_card(&mut f, 1);
    drop(f);
    let good = saved(&store);
    store.put_raw(good[0] + 1, b"NCHELD1\n\xff\xff\xff\xffgarbage".to_vec());
    store.put_raw(good[0] + 2, Vec::new());

    let mut host = RunHost::new(
        Arc::new(Threads(Instant::now())),
        JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: "held-restart".into(),
            wall_clock_ms: Arc::new(|| 0),
        },
    );
    let error = host.open_held(Box::new(store.clone())).unwrap_err();
    assert!(error.message.contains("couldn't be restored") && error.message.contains("set aside"), "{}", error.message);
    assert_eq!(host.held_results().len(), 1, "the good record is held again");
    assert_eq!(saved(&store), good, "damaged records are set aside");
    assert_eq!(store.set_aside_records(), vec![good[0] + 1, good[0] + 2], "not deleted");
    assert_eq!(host.open_held(Box::new(store.clone())).unwrap_err().kind, nodecraft_types::ErrorKind::Conflict, "a host opens one store");
}

/// A store that can't save.
struct Full;
impl HeldStore for Full {
    fn list(&self) -> Result<Listing, nodecraft_types::Error> {
        Ok(Listing::default())
    }
    fn read(&self, _id: u64) -> Result<Vec<u8>, nodecraft_types::Error> {
        Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::NotFound, "none"))
    }
    fn write(&self, _id: u64, _bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
        Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "the disk is full"))
    }
    fn remove(&self, _id: u64) -> Result<(), nodecraft_types::Error> {
        Ok(())
    }
    fn set_aside(&self, _id: u64) -> Result<(), nodecraft_types::Error> {
        Ok(())
    }
}

#[test]
fn a_result_that_cant_be_saved_is_still_held_and_says_so() {
    let mut f = fixture_on(false, Box::new(Full));
    f.opened.clone().unwrap();
    f.canned.answer("a fox", vec![image()]);
    run_then_remove_the_card(&mut f, 1);
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("{held:?}") };
    assert!(!view.saved, "{view:?}");
}

/// One saved record's bytes, from a real run.
fn a_record() -> Vec<u8> {
    let store = MemoryHeld::default();
    let mut f = fixture(false, &store);
    f.canned.answer("a fox", vec![image()]);
    run_then_remove_the_card(&mut f, 1);
    drop(f);
    let [id] = saved(&store)[..] else { panic!("one record") };
    store.read(id).unwrap()
}

#[test]
fn more_records_than_can_be_held_come_back_over_later_launches() {
    let record = a_record();
    let store = MemoryHeld::default();
    for id in 1..=(MAX_HELD as u64 + 3) {
        store.put_raw(id, record.clone());
    }
    let mut f = fixture_on(false, Box::new(store.clone()));
    let error = f.opened.clone().unwrap_err();
    assert!(error.message.contains("3 more are saved"), "{}", error.message);
    let held = f.host.held_results();
    assert_eq!(held.len(), MAX_HELD, "as many as can be held");
    for view in held {
        act(&mut f, "run.held_discard", view.id);
    }
    assert_eq!(saved(&store), vec![MAX_HELD as u64 + 1, MAX_HELD as u64 + 2, MAX_HELD as u64 + 3], "the rest wait");
    drop(f);

    let f = fixture(false, &store);
    assert_eq!(f.host.held_results().len(), 3, "and come back at the next launch");
}

#[test]
fn a_new_record_never_takes_a_set_aside_ones_number() {
    let store = MemoryHeld::default();
    store.put_raw(5, b"garbage".to_vec());
    let mut f = fixture_on(false, Box::new(store.clone()));
    assert!(f.opened.as_ref().is_err_and(|e| e.message.contains("set aside")), "{:?}", f.opened);
    f.canned.answer("a fox", vec![image()]);
    run_then_remove_the_card(&mut f, 1);
    assert_eq!(saved(&store), vec![6]);
    assert_eq!(store.set_aside_records(), vec![5]);
}

#[test]
fn a_record_from_a_newer_nodecraft_is_left_in_place() {
    let mut newer = a_record();
    let at = newer.windows(11).position(|w| w == br#""version":1"#).unwrap();
    newer[at + 10] = b'9';
    let store = MemoryHeld::default();
    store.put_raw(1, newer);
    let f = fixture_on(false, Box::new(store.clone()));
    let error = f.opened.clone().unwrap_err();
    assert!(error.message.contains("left in place"), "{}", error.message);
    assert!(f.host.held_results().is_empty());
    assert_eq!((saved(&store), store.set_aside_records()), (vec![1], vec![]), "neither restored nor set aside");
}
