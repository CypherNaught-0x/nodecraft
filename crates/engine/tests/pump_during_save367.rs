//! The frame's event pump keeps going while a background save is stuck in a disk write (#367):
//! provider traffic for a run in flight (the mock provider, polled on its own threads) goes on,
//! the run host's pump on the UI thread returns at once every time, and the finished result is
//! held by nobody, neither refused nor recorded into the document the save snapshotted, until
//! the save settles. Then it is recorded as usual. The UI thread holds no lock the save's thread
//! needs: the save is parked inside the storage's write while the test thread reads, pumps and
//! answers commands. And a change from a caller that must not block (`dispatch_without_waiting`)
//! is refused as busy while the save writes, rather than waiting for it.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    commands::Services,
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::{RunHost, RunNotice},
    save_runner::{SaveJob, SaveRunner},
};
use nodecraft_exec::driver::{Runtime, TaskFuture};
use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::TEXT_KIND;
use nodecraft_providers::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, Error, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([67; 16]);
const N: NodeId = NodeId::from_bytes([68; 16]);
const FILE: &str = "/projects/pumped.ncraft";

/// Each task on its own thread; sleeps on a timer thread, as the app's runtime does.
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
/// Runs each save on a thread of its own, as the desktop's runner does.
struct SaveThreads;
impl SaveRunner for SaveThreads {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        std::thread::spawn(move || job.run());
        Ok(())
    }
}

type Hold = (mpsc::Sender<()>, mpsc::Receiver<()>);
/// Memory storage whose destination write can be parked, as a stalled disk would park it.
#[derive(Clone, Default)]
struct Stalling {
    memory: Arc<Mutex<MemoryStorage>>,
    hold: Arc<Mutex<Option<Hold>>>,
}
impl ProjectStorage for Stalling {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.memory.lock().unwrap().load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        // Taken before waiting: nothing of the test's is locked while the write is parked.
        let hold = self.hold.lock().unwrap().take();
        if let Some((entered, go)) = hold {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        self.memory.lock().unwrap().save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.memory.lock().unwrap().checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.memory.lock().unwrap().append(b)
    }
}

fn history(session: &Session) -> usize {
    session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().history.len()
}

#[test]
fn the_run_pump_keeps_going_while_a_save_is_parked_in_a_disk_write() {
    // A builtin text model, answered by the mock provider after a few polls: no network.
    let catalog = GenerationCatalog::builtin().unwrap();
    let kinds = catalog.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([69; 16]), "Pumped").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let params = [("model".into(), Value::Text("anthropic.claude-opus-5-5".into())), ("prompt".into(), Value::Text("Describe the sea".into()))];
    let node = kinds.get(TEXT_KIND).unwrap().instantiate_instance(N, Default::default(), &params.into_iter().collect()).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let storage = Stalling::default();
    let snapshot = Snapshot { project, assets: Default::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(storage.clone())).unwrap();
    session.set_save_runner(Arc::new(SaveThreads));
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "pump367".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    session = session.with_services(services);
    let mock = MockProvider::new("anthropic", [("claude-opus-5-5".to_owned(), MockModel { polls: 3, ..MockModel::new(MockOutput::Text) })]);
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(N, "anthropic", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);

    // The run is accepted and started; then ⌘S starts a background save whose write stalls.
    let request = json!({"canvas": C, "mode": "selected", "nodes": [N]});
    let quote = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    let amounts: serde_json::Map<_, _> = quote["totals"].as_object().unwrap().iter().map(|(c, t)| (c.clone(), t["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token": quote["token"], "partial": false, "unpriced_jobs": 0, "amounts": amounts});
    session.dispatch("run.start", &start).unwrap();
    let mut notices = host.pump(&mut session);
    let (entered, parked) = mpsc::channel();
    let (go, wait) = mpsc::channel();
    *storage.hold.lock().unwrap() = Some((entered, wait));
    let snapshotted = session.document().clone();
    assert_eq!(session.dispatch("file.save_as", &json!({"location": FILE, "background": true})).unwrap()["saving"], true);
    parked.recv_timeout(Duration::from_secs(10)).unwrap();

    // If anything on this thread waited for the save, the watchdog would have to free it.
    let freed = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let (freed, go) = (Arc::clone(&freed), go.clone());
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            if matches!(stopped.recv_timeout(Duration::from_secs(30)), Err(mpsc::RecvTimeoutError::Timeout)) {
                freed.store(true, Ordering::SeqCst);
                let _ = go.send(());
            }
        });
        (stop, thread)
    };
    // Frames: every pump returns at once while the provider job is polled to its end on its
    // own threads.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut frames = 0u32;
    let mut slowest = Duration::ZERO;
    while mock.polls() < 3 || host.is_busy() && frames < 50 {
        let frame = Instant::now();
        notices.extend(host.pump(&mut session));
        session.dispatch("perf.report", &json!({})).unwrap();
        assert!(session.poll_file().is_none(), "the save is still parked");
        slowest = slowest.max(frame.elapsed());
        frames += 1;
        assert!(Instant::now() < deadline, "the provider job never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(mock.polls() >= 3, "the provider was polled while the save was parked");
    assert_eq!(mock.requests().len(), 1);
    assert!(frames > 3 && slowest < Duration::from_secs(1), "{frames} frames, slowest {slowest:?}");
    assert!(!freed.load(Ordering::SeqCst), "no frame waited for the disk");
    // The finished result waits: not recorded into the snapshotted document, not refused, held
    // by nobody.
    assert_eq!(history(&session), 0);
    assert_eq!(session.document(), &snapshotted);
    assert!(host.held_results().is_empty());
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    assert!(session.file_status().saving);

    // The disk comes back: the save settles, then the next pumps record the result.
    go.send(()).unwrap();
    let saved = loop {
        if let Some(outcome) = session.poll_file() {
            break outcome.unwrap();
        }
        assert!(Instant::now() < deadline + Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(saved["saved"], true);
    let deadline = Instant::now() + Duration::from_secs(20);
    while host.is_busy() || history(&session) == 0 {
        notices.extend(host.pump(&mut session));
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(notices.contains(&RunNotice::Recorded { canvas: C, node: N }), "{notices:?}");
    assert_eq!(history(&session), 1);
    assert!(host.held_results().is_empty());
    // The file holds the state the save snapshotted, without the later result.
    let bytes = storage.memory.lock().unwrap().files.get(&StorageLocation::from_command(FILE.into()).unwrap()).cloned().unwrap();
    assert_eq!(nodecraft_format::decode(&bytes).unwrap().project, snapshotted);
    let (stop, thread) = watchdog;
    drop(stop);
    thread.join().unwrap();
    assert!(!freed.load(Ordering::SeqCst));
}

/// Holds each job until the test releases it.
#[derive(Default)]
struct Gate(Mutex<Vec<SaveJob>>);
impl SaveRunner for Gate {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}
impl Gate {
    fn release(&self) {
        let jobs = std::mem::take(&mut *self.0.lock().unwrap());
        jobs.into_iter().for_each(SaveJob::run);
    }
}
fn moved(x: i32) -> serde_json::Value {
    json!({"canvas":crate::common::canvas(),"id":crate::common::node_id(1),"pos":{"x":x,"y":0}})
}
fn busy(error: &nodecraft_types::Error) -> bool {
    error.kind == nodecraft_types::ErrorKind::Busy && error.message.contains("file.cancel")
}

/// A caller that must not block on the disk (the desktop's control channel, MCP and agent
/// answer on the UI thread) is refused while a save writes, instead of waiting for it.
#[test]
fn a_change_that_must_not_wait_is_refused_while_a_save_writes_and_lands_once_it_finished() {
    let gate = Arc::new(Gate::default());
    let snapshot = Snapshot { project: crate::common::project(), assets: Default::default(), epoch: 0 };
    let mut session = Session::with_sendable_storage(snapshot, Box::new(MemoryStorage::default())).unwrap();
    session.set_save_runner(gate.clone());
    session.dispatch("node.move", &moved(10)).unwrap();
    let snapshotted = session.document().clone();
    assert_eq!(session.dispatch("file.save_as", &json!({"location": "/projects/saving.ncraft", "background": true})).unwrap()["saving"], true);
    // With the job held, waiting would never return: the refusal is immediate and actionable.
    let error = session.dispatch_without_waiting("node.move", &moved(20)).unwrap_err();
    assert!(busy(&error) && error.message.contains("saved"), "{error:?}");
    assert!(session.settle_without_waiting().is_err_and(|error| busy(&error)));
    assert_eq!(session.document(), &snapshotted);
    // Reads answer meanwhile.
    session.dispatch_without_waiting("perf.report", &json!({})).unwrap();
    assert!(session.file_status().saving);
    // Once the job has finished, the next change settles it first and lands; the outcome waits
    // for the host's poll.
    gate.release();
    session.dispatch_without_waiting("node.move", &moved(30)).unwrap();
    assert!(!session.file_status().saving);
    assert_eq!(session.poll_file().unwrap().unwrap()["saved"], true);
    assert!(session.file_status().dirty, "saved, then edited");
    // Outside a save it is plain dispatch.
    session.dispatch_without_waiting("node.move", &moved(40)).unwrap();
    session.settle_without_waiting().unwrap();
}
