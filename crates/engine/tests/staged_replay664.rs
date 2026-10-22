//! A paid request cut off mid-send is sent again unchanged after a restart (#664): a Genjutsu
//! motion transfer through the real Higgsfield adapter stages its exact request in the run journal
//! before the POST; the process is killed while the POST hangs, and the next launch sends the same
//! bytes under the same Idempotency-Key, with no new uploads, charges it once and records the
//! result. Scripted transport, no network.
#![cfg(not(target_arch = "wasm32"))]
use futures::future::{AbortHandle, abortable};
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::{ProviderRouting, RegisteredProvider},
    run_host::{RunHost, RunNotice},
    run_journal::MemoryJournal,
};
use nodecraft_exec::{
    cost::{Budgets, Charge, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::VIDEO_KIND;
use nodecraft_providers::{BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError, higgsfield::Higgsfield, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([66; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const PICTURE: NodeId = NodeId::from_bytes([3; 16]);
const PROJECT: ProjectId = ProjectId::from_bytes([66; 16]);
/// Five seconds of 640×640, made for the Genjutsu tests.
const FIVE_SECONDS: &[u8] = include_bytes!("../../providers/src/higgsfield/fixtures/source5s.mp4");
const REQUEST: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
const SUBMIT: &str = "/motion-transfer/v1.0";

/// Each task on its own thread; `kill` aborts every task, as a process exit would.
#[derive(Default)]
struct Killable(Mutex<Vec<AbortHandle>>);
impl Killable {
    fn kill(&self) {
        for task in self.0.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}
impl Runtime for Killable {
    fn spawn(&self, task: TaskFuture<()>) {
        let (task, handle) = abortable(task);
        self.0.lock().unwrap().push(handle);
        std::thread::spawn(move || {
            let _ = futures::executor::block_on(task);
        });
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
        0
    }
}

/// Higgsfield as both launches see it. The first submit never answers (the process dies while it
/// is in flight); later ones are answered. Every upload grant hands out a new link, so a request
/// built again after the restart would differ from the one staged.
#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
    /// Refuse the submit sent after the restart, as Higgsfield does once its upload links expire.
    refuse_replay: bool,
}
impl Transport {
    fn matching(&self, f: impl Fn(&HttpRequest) -> bool) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().iter().filter(|r| f(r)).cloned().collect()
    }
    fn submits(&self) -> Vec<HttpRequest> {
        self.matching(|r| r.method == HttpMethod::Post && r.url.ends_with(SUBMIT))
    }
    fn uploads(&self) -> usize {
        self.matching(|r| r.method == HttpMethod::Put).len()
    }
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let mut requests = self.requests.lock().unwrap();
        let n = requests.len();
        let first_submit = r.url.ends_with(SUBMIT) && !requests.iter().any(|seen| seen.url.ends_with(SUBMIT));
        let r_is_submit = r.url.ends_with(SUBMIT);
        let reply = if r.url.ends_with("/files/generate-upload-url") {
            let mime: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            let mime = mime["content_type"].as_str().unwrap().to_owned();
            json!({"public_url": format!("https://upload.cdn.example.com/inputs/{n}"), "upload_url": "https://upload.storage.example.com/put", "content_type": mime})
        } else if r.method == HttpMethod::Put {
            json!({})
        } else if r.url.ends_with(SUBMIT) {
            json!({"status": "queued", "request_id": REQUEST})
        } else if r.url.ends_with("/status") {
            json!({"request_id": REQUEST, "status": "completed", "video": {"url": "https://cdn.example.com/out.mp4"}})
        } else if r.url == "https://cdn.example.com/out.mp4" {
            requests.push(r);
            return Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: FIVE_SECONDS.to_vec() }) });
        } else {
            panic!("unexpected request {}", r.url)
        };
        requests.push(r);
        if first_submit {
            return Box::pin(futures::future::pending());
        }
        if self.refuse_replay && r_is_submit {
            let body = serde_json::to_vec(&json!({"detail": "image_url could not be downloaded"})).unwrap();
            return Box::pin(async move { Ok(HttpResponse { status: 422, headers: vec![], body }) });
        }
        let body = serde_json::to_vec(&reply).unwrap();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    runtime: Arc<Killable>,
    binding: RegisteredProvider,
    budget: JobBudget,
}

/// A Generate video card on Genjutsu motion transfer, the clip and a picture wired in.
fn wire_inputs(f: &mut Session) {
    let picture = nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap();
    let files = [Incoming { name: "dance.mp4", bytes: FIVE_SECONDS }, Incoming { name: "subject.png", bytes: &picture }];
    import_files(f, C, Position { x: -300.0, y: 0.0 }, 200.0, &files, &[SOURCE, PICTURE]).unwrap();
    for (n, (from, port)) in [(SOURCE, "source"), (PICTURE, "image")].into_iter().enumerate() {
        let edge = json!({"id": EdgeId::from_bytes([10 + n as u8; 16]), "from": {"node": from, "port": "out"}, "to": {"node": CARD, "port": port}, "order": 0});
        f.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
}

/// A launch: a new session on `project_from` (or a new project), a new host restoring `journal`.
fn launch(transport: &Arc<Transport>, journal: &MemoryJournal, project_from: Option<Project>) -> Fixture {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let fresh = project_from.is_none();
    let project = project_from.unwrap_or_else(|| {
        let mut project = Project::new(PROJECT, "Replay").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let values = [("model".into(), Value::Text("higgsfield.genjutsu-motion-transfer".into())), ("resolution".into(), Value::Text("720p".into()))].into();
        let card = kinds.get(VIDEO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
        Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
        project
    });
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "replay".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let runtime = Arc::new(Killable::default());
    let mut host = RunHost::new(runtime.clone(), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    host.open_journal(Box::new(journal.clone())).unwrap();
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let key = Secret::new("fixture-key-id-0001:fixture-secret-not-real-0001").unwrap();
    let binding = routing.bind_default(Arc::new(Higgsfield::new(key, transport.clone()))).unwrap();
    routing.select(CARD, "higgsfield", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    if fresh {
        wire_inputs(&mut session);
    }
    Fixture { session, host, runtime, binding, budget }
}

fn pump_until(f: &mut Fixture, what: &str, done: impl Fn(&Fixture) -> bool) -> Vec<RunNotice> {
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(f) {
        notices.extend(f.host.pump(&mut f.session));
        assert!(Instant::now() < deadline, "{what}: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    notices
}

/// The first launch: the card is quoted and run, its request staged and sent, and the process
/// killed while the POST is in flight. Returns the project as it was saved.
fn kill_mid_send(transport: &Arc<Transport>, journal: &MemoryJournal) -> Project {
    let mut first = launch(transport, journal, None);
    let request = json!({"canvas": C, "mode": "selected", "nodes": [CARD]});
    let q = first.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["totals"]["USD"]["micros"], 3_405_000, "{q}");
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": false, "unpriced_jobs": 0, "amounts": {"USD": 3_405_000}});
    first.session.dispatch("run.start", &start).unwrap();
    pump_until(&mut first, "the submit was never sent", |_| !transport.submits().is_empty());
    // The request was saved before it went out, with no handle yet.
    let saved = journal.saved().unwrap();
    let [entry] = saved.submitted.as_slice() else { panic!("{saved:?}") };
    assert!(entry.handle.remote_id.is_empty(), "{entry:?}");
    let staged = entry.staged.clone().expect("the request is staged before it is sent");
    let [sent] = transport.submits().try_into().unwrap();
    assert_eq!(staged.body.as_bytes(), sent.body.as_slice(), "the bytes saved are the bytes sent");
    assert_eq!(saved.usage.entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Estimate]);

    // Kill while the POST is in flight; its reply never arrives.
    first.runtime.kill();
    let project = first.session.document().clone();
    drop(first);
    assert_eq!(transport.uploads(), 2, "the clip and the picture");
    project
}

/// The next launch, on the same journal, resumes and runs until nothing is left.
fn relaunch(transport: &Arc<Transport>, journal: &MemoryJournal, project: Project) -> (Fixture, Vec<RunNotice>) {
    let mut second = launch(transport, journal, Some(project));
    assert!(second.host.has_waiting());
    let binding = second.binding.clone();
    second.host.resume(&second.session, &binding);
    let notices = pump_until(&mut second, "the replayed job never ended", |f| !f.host.is_busy());
    (second, notices)
}

#[test]
fn a_request_cut_off_mid_send_is_sent_again_unchanged_after_a_restart_and_charged_once() {
    let transport = Arc::new(Transport::default());
    let journal = MemoryJournal::default();
    let project = kill_mid_send(&transport, &journal);
    let (second, notices) = relaunch(&transport, &journal, project);
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: CARD }), "{notices:?}");
    let [lost, replayed] = transport.submits().try_into().unwrap();
    assert_eq!(lost.body, replayed.body, "sent again unchanged");
    let key = |r: &HttpRequest| r.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("idempotency-key")).map(|(_, value)| value.clone());
    assert!(key(&lost).is_some());
    assert_eq!(key(&lost), key(&replayed), "under the same key, so Higgsfield bills it once");
    assert_eq!(transport.uploads(), 2, "nothing uploaded again");
    let card = second.session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    assert!(
        matches!(card.history.as_slice(), [g] if matches!(g.outputs.as_slice(), [Value::Media(a)] if a.mime.essence() == "video/mp4")),
        "{:?}",
        card.history
    );
    let log = second.budget.log.lock().unwrap().clone();
    assert_eq!(log.entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Actual], "charged once");
    let saved = journal.saved().unwrap();
    assert!(saved.submitted.is_empty(), "{saved:?}");
}

#[test]
fn a_replay_refused_after_a_restart_stays_possibly_billed() {
    let transport = Arc::new(Transport { refuse_replay: true, ..Transport::default() });
    let journal = MemoryJournal::default();
    let project = kill_mid_send(&transport, &journal);
    let (second, notices) = relaunch(&transport, &journal, project);
    let failed = notices.iter().find_map(|n| match n {
        RunNotice::Failed { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert!(failed.as_deref().is_some_and(|reason| reason.contains("may have expired")), "{notices:?}");
    assert_eq!(transport.submits().len(), 2, "sent again once, as saved");
    assert_eq!(transport.uploads(), 2, "never uploaded and sent anew");
    // The lost POST may have been accepted: the refusal of its replay proves nothing about it.
    let log = second.budget.log.lock().unwrap().clone();
    assert_eq!(log.entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved], "kept as possibly billed");
    let saved = journal.saved().unwrap();
    assert!(saved.submitted.is_empty(), "never replayed again: {saved:?}");
}
