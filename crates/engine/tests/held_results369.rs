//! Paid results a run couldn't keep are held, not dropped (#369): a result that doesn't fit its
//! card's ports, can't be converted or can't be recorded waits in the run host until someone
//! retries recording it, puts it on the canvas or discards it, and none of these calls the
//! provider again. While the store is full, new jobs are refused before they are sent.
//! A canned provider only; no live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::{HeldAction, MAX_HELD, RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Generation, Project};
use nodecraft_nodes::generation::{KIND, TEXT_KIND};
use nodecraft_providers::{BoxFuture, Completed, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([69; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const PAINTER: NodeId = NodeId::from_bytes([2; 16]);
const WRITER_MODEL: &str = "fixture.writer";
const PAINTER_MODEL: &str = "fixture.painter";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{WRITER_MODEL}"
provider = "mock"
provider_model = "canned-text"
label = "Fixture writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]

[[model]]
id = "{PAINTER_MODEL}"
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

/// Returns whatever result it is given, counting requests. While gated, jobs queue and run until
/// the gate opens.
#[derive(Clone, Default)]
struct Canned {
    result: Arc<Mutex<Completed>>,
    requests: Arc<AtomicUsize>,
    gated: Arc<AtomicBool>,
}

impl Canned {
    fn answer(&self, outputs: Vec<Output>) {
        *self.result.lock().unwrap() = Completed::new(outputs);
    }
    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Provider for Canned {
    fn id(&self) -> &str {
        "mock"
    }
    fn submit<'a>(&'a self, _job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let submission = if self.gated.load(Ordering::SeqCst) {
            Submission::Queued(JobHandle { remote_id: "canned".into(), data: serde_json::Value::Null })
        } else {
            Submission::Finished(self.result.lock().unwrap().clone())
        };
        Box::pin(async move { Ok(submission) })
    }
    fn poll<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let progress = if self.gated.load(Ordering::SeqCst) {
            JobProgress::Running { fraction: None, message: None, partial: None }
        } else {
            JobProgress::Done(self.result.lock().unwrap().clone())
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

/// A text card and an image card, each with its own prompt.
fn fixture() -> Fixture {
    fixture_with(nodecraft_exec::run::RunLimits::default())
}

/// [`fixture`] whose runs keep at most `limits`.
fn fixture_with(limits: nodecraft_exec::run::RunLimits) -> Fixture {
    let canned = Canned::default();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([70; 16]), "Held results").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind, model) in [(WRITER, TEXT_KIND, WRITER_MODEL), (PAINTER, KIND, PAINTER_MODEL)] {
        let values = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Describe the sea".into()))].into();
        let node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "held-results".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone())
        .with_retry(RetryPolicy { max_attempts: 1, ..Default::default() })
        .with_run_limits(limits);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(canned.clone())).unwrap();
    for node in [WRITER, PAINTER] {
        routing.select(node, "mock", binding.target()).unwrap();
    }
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    Fixture { session, host, canned, budget }
}

/// Quotes and accepts a run of `node` alone.
fn start(f: &mut Fixture, node: NodeId) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[node]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":amounts});
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

fn run(f: &mut Fixture, node: NodeId) -> Vec<RunNotice> {
    start(f, node);
    drive(f)
}

fn failed(notices: &[RunNotice], node: NodeId) -> Option<String> {
    notices.iter().find_map(|n| match n {
        RunNotice::Failed { node: failed, reason, .. } if *failed == node => Some(reason.clone()),
        _ => None,
    })
}

fn history(f: &Fixture, node: NodeId) -> Vec<Generation> {
    f.session.document().canvases().get(&C).unwrap().nodes().get(&node).map(|n| n.history.clone()).unwrap_or_default()
}

fn node_count(f: &Fixture) -> usize {
    f.session.document().canvases().get(&C).unwrap().nodes().len()
}

/// The nodes on the canvas that weren't there at the start, top to bottom.
fn placed(f: &Fixture) -> Vec<(String, Value)> {
    let canvas = f.session.document().canvases().get(&C).unwrap();
    let mut nodes: Vec<_> = canvas.nodes().values().filter(|n| n.id != WRITER && n.id != PAINTER).collect();
    nodes.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y));
    nodes.into_iter().map(|n| (n.kind.clone(), n.params.values().next().cloned().unwrap_or(Value::Text(String::new())))).collect()
}

fn png() -> Vec<u8> {
    let pixels: Vec<u8> = (0..4u32 * 4).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(4, 4, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

fn image() -> Output {
    Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png()) }
}

fn text(s: &str) -> Output {
    Output::Text(s.into())
}

/// Lists the held results through the command, as an agent would.
fn listed(f: &mut Fixture) -> Vec<serde_json::Value> {
    let listed = f.session.dispatch("run.held", &json!({})).unwrap();
    assert_eq!(listed["max"], MAX_HELD);
    listed["held"].as_array().unwrap().clone()
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
fn a_result_that_fits_its_card_is_recorded_and_nothing_is_held() {
    let mut f = fixture();
    f.canned.answer(vec![image()]);
    let notices = run(&mut f, PAINTER);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: PAINTER }), "{notices:?}");
    assert_eq!(history(&f, PAINTER).len(), 1);
    assert!(f.host.held_results().is_empty());
    assert!(listed(&mut f).is_empty());
}

#[test]
fn several_images_are_recorded_as_one_list() {
    let mut f = fixture();
    f.canned.answer(vec![image(), image(), image()]);
    let notices = run(&mut f, PAINTER);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: PAINTER }), "{notices:?}");
    let shown = history(&f, PAINTER);
    assert!(matches!(shown[0].outputs.as_slice(), [Value::List(images)] if images.len() == 3), "{shown:?}");
    assert!(f.host.held_results().is_empty());
}

#[test]
fn mixed_or_extra_outputs_on_a_text_card_are_held_and_placed_in_order() {
    for (outputs, kinds) in [
        (vec![text("a caption"), image()], vec!["source.text", "source.image"]),
        (vec![image(), text("a caption")], vec!["source.image", "source.text"]),
        (vec![text("one"), text("two")], vec!["source.text", "source.text"]),
    ] {
        let mut f = fixture();
        f.canned.answer(outputs);
        let notices = run(&mut f, WRITER);
        let reason = failed(&notices, WRITER).unwrap_or_else(|| panic!("{notices:?}"));
        assert!(reason.contains("the paid result is held"), "{reason}");
        assert!(history(&f, WRITER).is_empty(), "nothing is recorded");
        assert_eq!(f.budget.log.lock().unwrap().entries().count(), 1, "the call is charged");

        let held = f.host.held_results();
        let [view] = held.as_slice() else { panic!("{held:?}") };
        assert_eq!((view.canvas, view.node, view.outputs), (C, WRITER, 2));
        assert!(view.retry, "its step was its alone");
        let row = &listed(&mut f)[0];
        assert_eq!(row["id"], view.id);
        assert_eq!(row["retry"], true);

        let nodes = node_count(&f);
        let notices = act(&mut f, "run.held_place", view.id);
        assert_eq!(node_count(&f), nodes + 2);
        assert!(notices.contains(&RunNotice::Placed { canvas: C, node: WRITER }), "{notices:?}");
        assert!(f.host.held_results().is_empty(), "placing it lets it go");
        let placed: Vec<String> = placed(&f).into_iter().map(|(kind, _)| kind).collect();
        assert_eq!(placed, kinds);
        assert_eq!(f.canned.requests(), 1, "placing calls no provider");
        assert_eq!(f.budget.log.lock().unwrap().entries().count(), 1, "nor charges again");
        f.session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(node_count(&f), nodes, "placing is one undo step");
    }
}

#[test]
fn placed_text_keeps_its_words_in_order() {
    let mut f = fixture();
    f.canned.answer(vec![text("first"), text("second")]);
    run(&mut f, WRITER);
    let id = f.host.held_results()[0].id;
    act(&mut f, "run.held_place", id);
    let words: Vec<Value> = placed(&f).into_iter().map(|(_, value)| value).collect();
    assert_eq!(words, vec![Value::Text("first".into()), Value::Text("second".into())]);
}

#[test]
fn a_result_that_cant_be_converted_is_held_whole_and_can_be_discarded() {
    let mut f = fixture();
    let garbage = Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"not an image at all"[..]) };
    f.canned.answer(vec![image(), garbage]);
    let notices = run(&mut f, PAINTER);
    let reason = failed(&notices, PAINTER).unwrap_or_else(|| panic!("{notices:?}"));
    assert!(reason.contains("the paid result is held"), "{reason}");
    assert!(history(&f, PAINTER).is_empty(), "no part of it is recorded");
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("{held:?}") };
    assert_eq!(view.outputs, 2, "the whole result is held");

    // Retrying it fails the same way, and it stays held.
    let notices = act(&mut f, "run.held_retry", view.id);
    let reason = failed(&notices, PAINTER).unwrap_or_else(|| panic!("{notices:?}"));
    assert!(reason.contains("still held"), "{reason}");
    assert_eq!(f.host.held_results().len(), 1);

    let discarded = f.session.dispatch("run.held_discard", &json!({"id": view.id})).unwrap();
    assert_eq!(discarded["discarded"], true);
    assert!(f.host.held_results().is_empty());
    assert!(f.session.dispatch("run.held_place", &json!({"id": view.id})).is_err(), "it is gone");
    assert_eq!(f.canned.requests(), 1);
}

#[test]
fn a_result_whose_card_went_away_is_held_and_recorded_once_it_is_back() {
    let mut f = fixture();
    f.canned.answer(vec![text("an answer")]);
    f.canned.gated.store(true, Ordering::SeqCst);
    start(&mut f, WRITER);
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.canned.requests() == 0 {
        f.host.pump(&mut f.session);
        assert!(Instant::now() < deadline, "the job was never sent");
        std::thread::sleep(Duration::from_millis(1));
    }
    f.session.dispatch("node.remove", &json!({"canvas": C, "id": WRITER})).unwrap();
    f.canned.gated.store(false, Ordering::SeqCst);
    let notices = drive(&mut f);
    let reason = failed(&notices, WRITER).unwrap_or_else(|| panic!("{notices:?}"));
    assert!(reason.contains("the paid result is held"), "{reason}");
    let held = f.host.held_results();
    let [view] = held.as_slice() else { panic!("{held:?}") };
    assert!(view.retry);

    // Retrying while the card is still gone fails and keeps it.
    let notices = act(&mut f, "run.held_retry", view.id);
    assert!(failed(&notices, WRITER).is_some_and(|r| r.contains("still held")), "{notices:?}");
    assert_eq!(f.host.held_results().len(), 1);

    f.session.dispatch("edit.undo", &json!({})).unwrap();
    let notices = act(&mut f, "run.held_retry", view.id);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: WRITER }), "{notices:?}");
    assert!(f.host.held_results().is_empty());
    let shown = history(&f, WRITER);
    assert!(matches!(shown.as_slice(), [g] if g.outputs == vec![Value::Text("an answer".into())]), "{shown:?}");
    assert_eq!(f.canned.requests(), 1, "recording it called no provider");
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), 1, "and charged once");
}

#[test]
fn a_pending_result_refuses_a_second_action_and_retry_needs_its_step() {
    let mut f = fixture();
    f.canned.answer(vec![text("one"), text("two")]);
    run(&mut f, WRITER);
    let id = f.host.held_results()[0].id;
    f.session.dispatch("run.held_place", &json!({"id": id})).unwrap();
    assert_eq!(f.host.held_results()[0].pending, Some(HeldAction::Place));
    for command in ["run.held_retry", "run.held_place", "run.held_discard"] {
        assert!(f.session.dispatch(command, &json!({"id": id})).is_err(), "{command} while pending");
    }
    f.host.pump(&mut f.session);
    assert!(f.host.held_results().is_empty());
    assert!(f.session.dispatch("run.held_retry", &json!({"id": 999})).is_err());
    assert!(f.session.dispatch("run.held", &json!({"extra": 1})).is_err());
}

#[test]
fn a_full_store_refuses_new_jobs_before_they_are_sent() {
    let mut f = fixture();
    f.canned.answer(vec![text("one"), text("two")]);
    for held in 1..MAX_HELD {
        run(&mut f, WRITER);
        assert_eq!(f.host.held_results().len(), held);
    }
    // One short of full: the next job is still sent, and its result held too.
    run(&mut f, WRITER);
    assert_eq!(f.host.held_results().len(), MAX_HELD);
    assert_eq!(f.canned.requests(), MAX_HELD);

    // Full: the next job is refused unsent.
    let notices = run(&mut f, WRITER);
    let reason = failed(&notices, WRITER).unwrap_or_else(|| panic!("{notices:?}"));
    assert!(reason.contains("waiting in the queue panel"), "{reason}");
    assert_eq!(f.canned.requests(), MAX_HELD, "nothing was sent");
    assert_eq!(f.host.held_results().len(), MAX_HELD);
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), MAX_HELD, "nothing more was charged");

    // Dealing with one makes room again.
    let id = f.host.held_results()[0].id;
    f.session.dispatch("run.held_discard", &json!({"id": id})).unwrap();
    run(&mut f, WRITER);
    assert_eq!(f.canned.requests(), MAX_HELD + 1);
}

#[test]
fn without_a_host_the_held_commands_are_unsupported() {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let project = Project::new(ProjectId::from_bytes([71; 16]), "No host").unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    assert!(session.dispatch("run.held", &json!({})).is_err());
    assert!(session.dispatch("run.held_place", &json!({"id": 1})).is_err());
}

/// A result too large to save on its card at any limit (admission, assembly, the graph or the
/// command's own) is held whole, never charged and lost (review of #369).
#[test]
fn a_result_too_large_to_save_is_held_whole_at_every_limit() {
    let most = nodecraft_graph::MAX_VALUE_ITEMS;
    for count in [most - 1, most, most + 1] {
        let mut f = fixture();
        f.canned.answer(vec![image(); count]);
        let notices = run(&mut f, PAINTER);
        assert!(failed(&notices, PAINTER).is_some_and(|r| r.contains("the paid result is held")), "{count}: {notices:?}");
        assert!(history(&f, PAINTER).is_empty());
        assert_eq!(f.host.held_results().first().map(|h| h.outputs), Some(count));
    }
}

#[test]
fn a_held_result_waits_while_its_project_is_closed() {
    let mut f = fixture();
    f.canned.answer(vec![text("one"), text("two")]);
    run(&mut f, WRITER);
    let id = f.host.held_results()[0].id;
    f.session.dispatch("run.held_place", &json!({"id": id})).unwrap();
    // The host's next frame comes with another project open.
    let mut other = Session::new(Project::new(ProjectId::from_bytes([72; 16]), "Other").unwrap()).unwrap();
    let notices = f.host.pump(&mut other);
    assert!(failed(&notices, WRITER).is_some_and(|r| r.contains("isn't open") && r.contains("still held")), "{notices:?}");
    assert_eq!(f.host.held_results().len(), 1);
    assert_eq!(f.host.held_results()[0].pending, None);
    act(&mut f, "run.held_place", id);
    assert!(f.host.held_results().is_empty());
    assert_eq!(placed(&f).len(), 2);
}

#[test]
fn a_paid_result_past_what_a_run_keeps_is_held_and_never_sent_again() {
    // #254: a run keeps at most `held_bytes` of results; one over it fails its step, and the
    // paid result is held rather than dropped.
    let mut f = fixture_with(nodecraft_exec::run::RunLimits { held_bytes: 64, ..Default::default() });
    let long = "a sea of words ".repeat(20);
    f.canned.answer(vec![text(&long)]);
    let notices = run(&mut f, WRITER);
    let reason = failed(&notices, WRITER).unwrap_or_else(|| panic!("{notices:?}"));
    assert!(reason.contains("past 64 bytes of results") && reason.contains("run fewer nodes together"), "{reason}");
    assert!(reason.contains("the paid result is held"), "{reason}");
    assert!(history(&f, WRITER).is_empty(), "nothing is recorded");
    let held = f.host.held_results();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(f.canned.requests(), 1);
    // Put on the canvas from where it is held, it is the whole result, and the provider isn't
    // asked again.
    let id = held[0].id;
    act(&mut f, "run.held_place", id);
    assert_eq!(placed(&f), vec![("source.text".to_owned(), Value::Text(long))]);
    assert_eq!(f.canned.requests(), 1, "held, never sent again");
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), 1, "charged once");
    assert!(f.host.held_results().is_empty());
}
