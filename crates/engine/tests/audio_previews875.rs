//! Streamed previews of a sound (#875) reach the run host's job view, never the document: no
//! changeset, no history, no undo step, nothing autosaved. The finished sound is recorded as
//! before, and its job then holds no preview. Mock provider, no network.
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
    run_jobs::{AudioPreview, JobState, Made, Take},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::AUDIO_KIND;
use nodecraft_providers::Output;
use nodecraft_providers::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([75; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const MODEL: &str = "fixture.speech";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-audio"
label = "Fixture speech"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-audio"
output = "audio"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .unwrap()
}

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

/// An MP3 of `len` bytes: an ID3v2.4 header, an MPEG-1 Layer III frame header, then filler.
fn mp3(len: usize) -> Arc<[u8]> {
    let mut bytes = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64".to_vec();
    bytes.resize(len, 7);
    bytes.into()
}

fn sound(len: usize) -> Output {
    Output::Bytes { mime: "audio/mpeg".into(), bytes: mp3(len) }
}

/// A speech card on a mock model that sends `previews` and then takes `polls` polls.
fn fixture(previews: Vec<Output>, polls: u32, on: bool) -> (Session, RunHost) {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([75; 16]), "Speech").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Hello from a paper boat".into()))].into();
    let card = kinds.get(AUDIO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "speech".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() }).with_previews(on);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut model = MockModel::new(MockOutput::Audio);
    model.polls = polls;
    let mock = MockProvider::new("mock", [("mock-audio".to_owned(), model)]).with_previews("mock-audio", previews);
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock)).unwrap();
    routing.select(CARD, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    (session, host)
}

fn start(session: &mut Session) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":amounts});
    session.dispatch("run.start", &start).unwrap();
}

fn preview(host: &RunHost) -> Option<AudioPreview> {
    host.jobs().into_iter().rev().find(|view| view.node == CARD).and_then(|view| view.preview)
}

fn history(session: &Session) -> Vec<nodecraft_graph::Generation> {
    session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap().history.clone()
}

fn pump_until(session: &mut Session, host: &mut RunHost, what: &str, done: impl Fn(&RunHost) -> bool) -> Vec<RunNotice> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    while !done(host) {
        notices.extend(host.pump(session));
        assert!(Instant::now() < deadline, "{what}: {:?}", host.jobs());
        std::thread::sleep(Duration::from_millis(1));
    }
    notices
}

#[test]
fn previews_of_a_sound_reach_its_job_and_never_the_document() {
    let (mut session, mut host) = fixture(vec![sound(1000), sound(3000)], u32::MAX, true);
    start(&mut session);
    let (revision, undo, before) = (session.revision(), session.can_undo(), session.document().clone());
    pump_until(&mut session, &mut host, "the last preview arrives", |host| preview(host).is_some_and(|p| p.bytes.len() == 3000));
    let shown = preview(&host).unwrap();
    assert_eq!(shown.mime, "audio/mpeg");
    assert_eq!(shown.bytes, mp3(3000));
    let view = host.jobs().into_iter().find(|view| view.node == CARD).unwrap();
    assert_eq!(shown.take, Take { job: view.key, attempt: 1 }, "a preview names the take making it");
    assert!(matches!(view.state, JobState::Running { .. }), "{:?}", view.state);
    assert_eq!(session.revision(), revision, "previews change nothing in the document");
    assert!(history(&session).is_empty(), "nor its history");
    assert_eq!(session.can_undo(), undo, "nor its undo");
    assert_eq!(session.document(), &before, "so autosave has nothing new to write");

    host.cancel_all();
    let deadline = Instant::now() + Duration::from_secs(20);
    while host.is_busy() {
        host.pump(&mut session);
        assert!(Instant::now() < deadline, "the run never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(preview(&host), None, "a cancelled sound's preview is gone");
    assert!(history(&session).is_empty());
    let view = host.jobs().into_iter().find(|view| view.node == CARD).unwrap();
    assert_eq!(view.made, None, "and it made nothing a card could play on into");
}

#[test]
fn the_finished_sound_is_recorded_and_its_job_keeps_no_preview() {
    let (mut session, mut host) = fixture(vec![sound(1000)], 3, true);
    start(&mut session);
    let notices = pump_until(&mut session, &mut host, "the run ends", |host| !host.is_busy());
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
    let history = history(&session);
    assert_eq!(history.len(), 1, "one result, one undo step");
    let [Value::Media(recorded)] = history[0].outputs.as_slice() else { panic!("{:?}", history[0].outputs) };
    assert_eq!(recorded.mime.essence(), "audio/wav");
    let view = host.jobs().into_iter().find(|view| view.node == CARD).unwrap();
    assert_eq!(view.state, JobState::Done);
    assert_eq!(view.preview, None, "the card plays the result now");
    // Only the sound this job recorded continues its preview, never one a card had before.
    assert_eq!(view.made, Some(Made { attempt: 1, assets: vec![recorded.hash] }));
}

#[test]
fn a_runner_that_asks_for_no_previews_gets_none() {
    // The CLI's host, and the browser's: nothing would play them.
    let (mut session, mut host) = fixture(vec![sound(1000)], 50, false);
    start(&mut session);
    let mut seen = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while host.is_busy() {
        host.pump(&mut session);
        seen |= preview(&host).is_some();
        assert!(Instant::now() < deadline, "the run never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!seen, "previews are off unless asked for");
    assert_eq!(history(&session).len(), 1);
}
