//! A batch item that may have been billed is run again under its old key (#517): a prompt whose
//! reply was lost on submit, or one the provider ran whose output then failed here, is kept with
//! its key in the partial result, and running the card again sends it under that key, so a
//! provider that keeps jobs by key answers with the job it already ran instead of charging for a
//! new one. An item the provider reported failed, or a cancelled one, runs under a fresh key: the
//! old key would only answer with the failure. A keyed fake provider only; no live calls.
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
    run_jobs::JobState,
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Generation, Missing, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, Completed, ErrorKind, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeMap;
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

/// How the provider treats a prompt sent under a key it hasn't seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Runs it at once.
    Answer,
    /// Runs it, bills it and loses the reply: the submit fails as a dropped connection.
    LoseReply,
    /// Queues it; the first poll fails with a reply that can't be read, later ones find it done.
    SpoilFirstPoll,
    /// Queues it; every poll fails with a reply that can't be read.
    SpoilAlways,
    /// Queues it and reports it failed.
    Fail,
    /// Queues it and never finishes.
    Hang,
}

/// A job the provider holds under its key.
#[derive(Clone, Debug)]
struct Held {
    prompt: String,
    failed: bool,
    polls: usize,
}

/// Keeps every job by its idempotency key, as providers that honour keys do: a key it has seen
/// answers with the job it holds, and only a new key is billed.
#[derive(Clone, Default)]
struct Keeper {
    modes: Arc<Mutex<BTreeMap<String, Mode>>>,
    jobs: Arc<Mutex<BTreeMap<String, Held>>>,
    /// Every key a submit was sent under, in order.
    sent: Arc<Mutex<Vec<String>>>,
    /// Says sending a job again under its key isn't safe, as providers that ignore keys do.
    ignores_keys: Arc<std::sync::atomic::AtomicBool>,
}

impl Keeper {
    fn set(&self, prompt: &str, mode: Mode) {
        self.modes.lock().unwrap().insert(prompt.into(), mode);
    }
    fn mode(&self, prompt: &str) -> Mode {
        self.modes.lock().unwrap().get(prompt).copied().unwrap_or(Mode::Answer)
    }
    /// How many jobs it billed: one per new key.
    fn billed(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
    fn hold(&self, key: &str, prompt: &str, failed: bool) {
        self.jobs.lock().unwrap().insert(key.into(), Held { prompt: prompt.into(), failed, polls: 0 });
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

fn queued(key: &str) -> Submission {
    Submission::Queued(JobHandle { remote_id: key.into(), data: serde_json::Value::Null })
}

impl Provider for Keeper {
    fn id(&self) -> &str {
        "mock"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        !self.ignores_keys.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        let key = job.idempotency_key.clone();
        self.sent.lock().unwrap().push(key.clone());
        let prompt = prompt_of(job);
        let known = self.jobs.lock().unwrap().get(&key).cloned();
        let answer = match known {
            Some(held) if held.failed => Ok(queued(&key)),
            Some(_) => Ok(Submission::Finished(Completed::new(vec![image()]))),
            None => match self.mode(&prompt) {
                Mode::Answer => {
                    self.hold(&key, &prompt, false);
                    Ok(Submission::Finished(Completed::new(vec![image()])))
                }
                Mode::LoseReply => {
                    self.hold(&key, &prompt, false);
                    Err(ProviderError::new(ErrorKind::Network, "the connection dropped"))
                }
                Mode::SpoilFirstPoll | Mode::SpoilAlways | Mode::Hang => {
                    self.hold(&key, &prompt, false);
                    Ok(queued(&key))
                }
                Mode::Fail => {
                    self.hold(&key, &prompt, true);
                    Ok(queued(&key))
                }
            },
        };
        Box::pin(async move { answer })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let progress = {
            let mut jobs = self.jobs.lock().unwrap();
            match jobs.get_mut(&handle.remote_id) {
                None => Err(ProviderError::new(ErrorKind::NotFound, "no such job")),
                Some(held) if held.failed => Ok(JobProgress::Failed(ProviderError::new(ErrorKind::JobFailed, "the model refused"))),
                Some(held) => {
                    held.polls += 1;
                    match self.modes.lock().unwrap().get(&held.prompt).copied() {
                        Some(Mode::Hang) => Ok(JobProgress::Running { fraction: None, message: None, partial: None }),
                        Some(Mode::SpoilFirstPoll) if held.polls == 1 => Err(ProviderError::new(ErrorKind::Malformed, "the reply can't be read")),
                        Some(Mode::SpoilAlways) => Err(ProviderError::new(ErrorKind::Malformed, "the reply can't be read")),
                        _ => Ok(JobProgress::Done(Completed::new(vec![image()]))),
                    }
                }
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

struct Fixture {
    session: Session,
    host: RunHost,
    keeper: Keeper,
    budget: JobBudget,
}

/// A batch of two prompts into an image card: one job per prompt.
fn fixture() -> Fixture {
    let keeper = Keeper::default();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([74; 16]), "Batch keys").unwrap();
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
        session: "batch-keys".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(keeper.clone())).unwrap();
    routing.select(PAINTER, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut f = Fixture { session, host, keeper, budget };
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

fn run(f: &mut Fixture) -> Vec<RunNotice> {
    start(f);
    drive(f)
}

fn active(f: &Fixture) -> Generation {
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&PAINTER).unwrap().clone();
    node.history.iter().find(|g| Some(g.id) == node.active).unwrap().clone()
}

fn image() -> Output {
    let pixels: Vec<u8> = (0..4u32 * 4).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    let png = nodecraft_media::encode_png(4, 4, &pixels, &nodecraft_media::Limits::default()).unwrap();
    Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png) }
}

/// The owl's item of the active partial result: why it is missing and the keys kept with it.
fn missing_owl(f: &Fixture) -> (Missing, Vec<String>) {
    let generation = active(f);
    let incomplete = generation.incomplete.clone().unwrap_or_else(|| panic!("a partial result: {generation:?}"));
    let [owl] = incomplete.missing.as_slice() else { panic!("{incomplete:?}") };
    assert_eq!(owl.item, 1, "the owl is the second prompt");
    (owl.why, owl.keys.iter().map(|k| k.key.clone()).collect())
}

/// The usage log's entries for `key`.
fn charges_under(f: &Fixture, key: &str) -> usize {
    f.budget.log.lock().unwrap().entries().filter(|e| e.idempotency_key == key).count()
}

#[test]
fn a_lost_reply_is_sent_again_under_its_old_key_and_not_charged_twice() {
    let mut f = fixture();
    f.keeper.set("an owl", Mode::LoseReply);
    run(&mut f);
    let (why, keys) = missing_owl(&f);
    let [key] = keys.as_slice() else { panic!("the owl's key is kept: {keys:?}") };
    assert_eq!(why, Missing::Failed);
    assert_eq!(f.keeper.billed(), 2, "the provider ran both");

    f.keeper.set("an owl", Mode::Answer);
    run(&mut f);
    let generation = active(&f);
    assert!(generation.incomplete.is_none(), "the owl's job completes the result: {generation:?}");
    assert_eq!(f.keeper.billed(), 2, "the owl's job is the one already paid for");
    assert_eq!(f.keeper.sent().last(), Some(key), "sent again under its old key");
    assert_eq!(f.keeper.sent().len(), 3, "the fox isn't sent again");
    assert_eq!(charges_under(&f, key), 1, "one charge in the usage log");
}

#[test]
fn an_output_that_failed_here_is_followed_again_rather_than_bought_again() {
    let mut f = fixture();
    f.keeper.set("an owl", Mode::SpoilFirstPoll);
    run(&mut f);
    let (why, keys) = missing_owl(&f);
    let [key] = keys.as_slice() else { panic!("the owl's key is kept: {keys:?}") };
    assert_eq!(why, Missing::Failed);

    run(&mut f);
    assert!(active(&f).incomplete.is_none(), "the owl's job completes the result");
    assert_eq!(f.keeper.billed(), 2, "nothing new is billed");
    assert_eq!(f.keeper.sent().len(), 2, "the provider's handle is polled again, nothing is submitted");
    assert_eq!(charges_under(&f, key), 1);
}

#[test]
fn a_job_the_provider_reported_failed_runs_under_a_fresh_key() {
    let mut f = fixture();
    f.keeper.set("an owl", Mode::Fail);
    run(&mut f);
    let (why, keys) = missing_owl(&f);
    assert_eq!((why, keys.len()), (Missing::Failed, 0), "nothing to recover: the provider says it failed");
    let failed = f.keeper.sent().last().cloned().unwrap();

    f.keeper.set("an owl", Mode::Answer);
    run(&mut f);
    assert!(active(&f).incomplete.is_none(), "a fresh job completes the result");
    let again = f.keeper.sent().last().cloned().unwrap();
    assert_ne!(again, failed, "the old key would only answer with the failure");
    assert_eq!(f.keeper.billed(), 3);
}

#[test]
fn a_cancelled_item_runs_under_a_fresh_key() {
    let mut f = fixture();
    f.keeper.set("an owl", Mode::Hang);
    start(&mut f);
    let deadline = Instant::now() + Duration::from_secs(5);
    // The fox is answered at once; wait until its job is done, so only the owl's is cancelled.
    while f.keeper.sent().len() < 2 || !f.host.jobs().iter().any(|job| job.state == JobState::Done) {
        f.host.pump(&mut f.session);
        assert!(Instant::now() < deadline, "the fox was never answered");
        std::thread::sleep(Duration::from_millis(1));
    }
    for job in f.host.jobs().into_iter().filter(|job| !job.state.is_final()) {
        let _ = f.host.cancel(job.key);
    }
    drive(&mut f);
    let (why, keys) = missing_owl(&f);
    assert_eq!((why, keys.len()), (Missing::Cancelled, 0));
    let cancelled = f.keeper.sent().last().cloned().unwrap();

    f.keeper.set("an owl", Mode::Answer);
    run(&mut f);
    assert!(active(&f).incomplete.is_none());
    assert_ne!(f.keeper.sent().last(), Some(&cancelled));
}

#[test]
fn a_provider_that_ignores_keys_runs_the_item_under_a_fresh_key() {
    // Sending the job again under its key would bill it again while the usage log kept one charge.
    let mut f = fixture();
    f.keeper.ignores_keys.store(true, std::sync::atomic::Ordering::SeqCst);
    f.keeper.set("an owl", Mode::SpoilFirstPoll);
    run(&mut f);
    let (why, keys) = missing_owl(&f);
    assert_eq!((why, keys.len()), (Missing::Failed, 0));
    let spoiled = f.keeper.sent().last().cloned().unwrap();

    f.keeper.set("an owl", Mode::Answer);
    run(&mut f);
    assert!(active(&f).incomplete.is_none());
    assert_ne!(f.keeper.sent().last(), Some(&spoiled));
    assert_eq!(f.keeper.billed(), 3);
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), 3, "each job the provider billed has its charge");
}

#[test]
fn an_output_that_fails_here_every_time_is_bought_again_on_the_third_run() {
    let mut f = fixture();
    f.keeper.set("an owl", Mode::SpoilAlways);
    run(&mut f);
    let (_, keys) = missing_owl(&f);
    let [key] = keys.as_slice() else { panic!("the owl's key is kept: {keys:?}") };

    // Followed again, and it fails the same way: the key isn't kept again.
    run(&mut f);
    let (why, keys) = missing_owl(&f);
    assert_eq!((why, keys.len()), (Missing::Failed, 0), "a failure that repeats isn't followed forever");
    assert_eq!(f.keeper.billed(), 2);

    f.keeper.set("an owl", Mode::Answer);
    run(&mut f);
    assert!(active(&f).incomplete.is_none());
    assert_ne!(f.keeper.sent().last(), Some(key), "a fresh job");
    assert_eq!(f.keeper.billed(), 3);
}
