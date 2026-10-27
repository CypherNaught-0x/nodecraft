//! Controlled worker stalls, actual pump/commands, cancellation and original large paid bytes.
#![allow(clippy::unwrap_used, clippy::panic)] // Test fixture helpers, including the deliberately blocked writer.
use super::*;
use crate::{
    commands::Services,
    import::{Incoming, import_files},
    jobs::JobBudget,
    provider_routing::{ProviderRouting, RegisteredProvider},
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::driver::TaskFuture;
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_providers::{BoxFuture, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ProjectId, Unsigned};
use serde_json::json;
use std::{
    io::Read,
    sync::{
        Condvar, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([137; 16]);
const N: NodeId = NodeId::from_bytes([138; 16]);
const IMAGE: NodeId = NodeId::from_bytes([139; 16]);
#[derive(Default)]
struct Flags {
    entered: bool,
    open: bool,
    expired: bool,
}
#[derive(Default)]
struct Gate {
    flags: Mutex<Flags>,
    changed: Condvar,
}
impl Gate {
    fn wait(&self) {
        let mut flags = self.flags.lock().unwrap();
        flags.entered = true;
        self.changed.notify_all();
        let (mut flags, timeout) = self.changed.wait_timeout_while(flags, Duration::from_secs(8), |flags| !flags.open).unwrap();
        if timeout.timed_out() {
            flags.expired = true;
        }
    }
    fn entered(&self) -> bool {
        self.flags.lock().unwrap().entered
    }
    fn release(&self) {
        self.flags.lock().unwrap().open = true;
        self.changed.notify_all();
    }
    fn still_blocked(&self) {
        let flags = self.flags.lock().unwrap();
        assert!(flags.entered && !flags.open && !flags.expired, "pump waited for its worker");
    }
    fn hook(self: &Arc<Self>) -> Hook {
        let gate = self.clone();
        Arc::new(move || gate.wait())
    }
}
struct Threads {
    started: Instant,
    tasks: Arc<AtomicUsize>,
}
impl Threads {
    fn new() -> Arc<Self> {
        Arc::new(Self { started: Instant::now(), tasks: Arc::default() })
    }
    fn idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.tasks.load(Ordering::SeqCst) > 0 {
            assert!(Instant::now() < deadline, "workers did not release their contexts");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
struct TaskCount(Arc<AtomicUsize>);
impl Drop for TaskCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        self.tasks.fetch_add(1, Ordering::SeqCst);
        let count = TaskCount(self.tasks.clone());
        std::thread::spawn(move || {
            let _count = count;
            futures::executor::block_on(task);
        });
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (send, done) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = send.send(());
        });
        Box::pin(async move {
            let _ = done.await;
        })
    }
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
    }
}
fn large_png() -> Arc<[u8]> {
    static PNG: OnceLock<Arc<[u8]>> = OnceLock::new();
    PNG.get_or_init(|| {
        let mut data = vec![0; 4096 * 2048 * 4];
        let mut value = 137u32;
        for pixel in data.as_chunks_mut::<4>().0 {
            for channel in pixel.iter_mut().take(3) {
                value ^= value << 13;
                value ^= value >> 17;
                value ^= value << 5;
                *channel = value as u8;
            }
            pixel[3] = 255;
        }
        let png = nodecraft_media::encode_png(4096, 2048, &data, &Default::default()).unwrap();
        assert!(png.len() > nodecraft_format::MAX_ASSET_BYTES);
        png.into()
    })
    .clone()
}
struct Answer {
    wait: AtomicBool,
    creates: AtomicUsize,
}
impl Provider for Answer {
    fn id(&self) -> &str {
        "fal"
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Submission::Queued(JobHandle { remote_id: "original137".into(), data: serde_json::Value::Null })) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            Ok(if self.wait.load(Ordering::SeqCst) {
                JobProgress::Running { fraction: None, message: None, partial: None }
            } else {
                JobProgress::Done(Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: large_png() }]))
            })
        })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        1
    }
}
struct Fixture {
    session: Session,
    host: RunHost,
    runtime: Arc<Threads>,
    answer: Arc<Answer>,
    binding: RegisteredProvider,
    dir: tempfile::TempDir,
    notices: Vec<RunNotice>,
}
fn empty() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([137; 16]), "Original worker fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Upscale") }.apply(&mut project).unwrap();
    project
}
fn fixture() -> Fixture {
    let _ = large_png(); // Authored data before the controlled timing interval.
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::with_storage(
        nodecraft_format::Snapshot { project: empty(), assets: Default::default(), epoch: 0 },
        Box::new(nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap()),
    )
    .unwrap();
    let node = session
        .kinds()
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text("fal.topaz-precision".into())), ("upscale_factor".into(), Value::Unsigned(Unsigned(4)))].into(),
        )
        .unwrap();
    session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    let pixels: Vec<_> = (0..1024 * 512).flat_map(|_| [100, 140, 180, 255]).collect();
    let png = nodecraft_media::encode_png(1024, 512, &pixels, &Default::default()).unwrap();
    import_files(&mut session, C, Position { x: -300., y: 0. }, 200., &[Incoming { name: "original.png", bytes: &png }], &[IMAGE]).unwrap();
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([140;16]),"from":{"node":IMAGE,"port":"out"},"to":{"node":N,"port":"image"},"order":0}}),
        )
        .unwrap();
    let runtime = Threads::new();
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "worker137".into(), wall_clock_ms: Arc::new(|| 0) };
    let host = RunHost::new(runtime.clone(), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    session = session.with_services(services);
    let answer = Arc::new(Answer { wait: AtomicBool::new(false), creates: AtomicUsize::new(0) });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(answer.clone()).unwrap();
    routing.select(N, "fal", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    Fixture { session, host, runtime, answer, binding, dir, notices: Vec::new() }
}
impl Fixture {
    fn start(&mut self) {
        let mut request = json!({"canvas":C,"mode":"selected","nodes":[N]});
        let quote = self.session.dispatch("run.quote", &request).unwrap();
        assert_eq!(quote["totals"]["USD"]["micros"], 80_000);
        request["accept"] = json!({"token":quote["token"],"amounts":{"USD":80_000},"partial":false,"unpriced_jobs":0});
        self.session.dispatch("run.start", &request).unwrap();
    }
    fn until(&mut self, mut ready: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready(self) {
            self.notices.extend(self.host.pump(&mut self.session));
            assert!(Instant::now() < deadline, "{:?}", self.notices);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn done(&mut self) {
        self.until(|f| !f.host.is_busy());
    }
    fn edit_while_blocked(&mut self, gate: &Gate) {
        assert!(self.host.is_preparing_results());
        self.notices.extend(self.host.pump(&mut self.session));
        self.session.dispatch("node.title", &json!({"canvas":C,"id":IMAGE,"title":"Edited during worker IO"})).unwrap();
        gate.still_blocked();
    }
    fn exact(&self) {
        let node = &self.session.document().canvases()[&C].nodes()[&N];
        let [Value::List(items)] = node.history.last().unwrap().outputs.as_slice() else { panic!("image list") };
        let [Value::Media(asset)] = items.as_slice() else { panic!("one result") };
        let metrics = StreamMetrics::default();
        let lease = self.session.assets().open_verified(asset, &Default::default(), &metrics).unwrap();
        let mut bytes = Vec::new();
        lease.reader(metrics).take(64 * 1024 * 1024).read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), large_png().as_ref());
        assert_eq!(self.answer.creates.load(Ordering::SeqCst), 1);
    }
    fn make_held(&mut self) {
        self.answer.wait.store(true, Ordering::SeqCst);
        self.start();
        self.until(|f| f.answer.creates.load(Ordering::SeqCst) == 1);
        self.session.dispatch("node.remove", &json!({"canvas":C,"id":N})).unwrap();
        self.answer.wait.store(false, Ordering::SeqCst);
        self.until(|f| !f.host.held_results().is_empty());
    }
}

#[test]
fn topaz37_blocked_completion_worker_keeps_pump_edit_and_cancel_live() {
    let mut f = fixture();
    let gate = Arc::new(Gate::default());
    f.host.preparation_hook = Some(gate.hook());
    f.start();
    f.until(|_| gate.entered());
    f.edit_while_blocked(&gate);
    f.host.cancel_all();
    gate.still_blocked();
    gate.release();
    f.done();
    assert_eq!(f.host.held_results().len(), 1, "known paid bytes survive cancellation");
    assert_eq!(f.answer.creates.load(Ordering::SeqCst), 1);
    f.runtime.idle();
}

#[test]
fn topaz37_blocked_held_retry_and_place_never_fall_back_to_frame_io() {
    for place in [false, true] {
        let mut f = fixture();
        f.make_held();
        f.done();
        let id = f.host.held_results()[0].id;
        if !place {
            f.session.dispatch("edit.undo", &json!({})).unwrap();
        }
        let gate = Arc::new(Gate::default());
        f.host.held_preparation.hook = Some(gate.hook());
        f.session.dispatch(if place { "run.held_place" } else { "run.held_retry" }, &json!({"id":id})).unwrap();
        f.until(|_| gate.entered());
        f.edit_while_blocked(&gate);
        gate.release();
        f.until(|f| f.host.held_results().is_empty());
        if place {
            let canvas = &f.session.document().canvases()[&C];
            let node = canvas.nodes().values().find(|node| node.id != IMAGE && node.kind == "source.image").unwrap();
            let Value::Media(asset) = node.params.get("asset").unwrap() else { panic!("asset") };
            assert_eq!(asset.hash, Blake3::from_bytes(*blake3::hash(&large_png()).as_bytes()));
        } else {
            f.exact();
        }
        assert_eq!(f.answer.creates.load(Ordering::SeqCst), 1);
        f.runtime.idle();
    }
}

#[derive(Clone)]
struct SlowStore {
    memory: crate::held_store::MemoryHeld,
    gate: Arc<Gate>,
    fail: bool,
}
impl crate::held_store::HeldStore for SlowStore {
    fn list(&self) -> Result<crate::held_store::Listing, Error> {
        self.memory.list()
    }
    fn read(&self, id: u64) -> Result<Vec<u8>, Error> {
        self.memory.read(id)
    }
    fn write(&self, id: u64, bytes: &[u8]) -> Result<(), Error> {
        self.gate.wait();
        if self.fail { Err(Error::new(ErrorKind::Internal, "controlled write failure")) } else { self.memory.write(id, bytes) }
    }
    fn remove(&self, id: u64) -> Result<(), Error> {
        self.memory.remove(id)
    }
    fn set_aside(&self, id: u64) -> Result<(), Error> {
        self.memory.set_aside(id)
    }
}
#[test]
fn topaz37_large_held_persistence_is_a_busy_worker_and_failure_keeps_the_paid_original() {
    for fail in [false, true] {
        let mut f = fixture();
        let gate = Arc::new(Gate::default());
        let store = SlowStore { memory: Default::default(), gate: gate.clone(), fail };
        f.host.open_held(Box::new(store)).unwrap();
        f.make_held();
        f.until(|_| gate.entered());
        f.edit_while_blocked(&gate);
        assert!(f.host.is_busy());
        gate.release();
        f.done();
        let held = f.host.held_results();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].saved, !fail);
        assert_eq!(f.answer.creates.load(Ordering::SeqCst), 1);
        f.runtime.idle();
    }
}

#[test]
fn topaz37_closed_context_drops_old_worker_and_resumed_job_prepares_off_frame_without_submit() {
    let mut f = fixture();
    let journal = crate::run_journal::MemoryJournal::default();
    f.host.open_journal(Box::new(journal.clone())).unwrap();
    let path = f.dir.path().join("original.ncraft");
    f.session.dispatch("file.save_as", &json!({"location":path})).unwrap();
    let gate = Arc::new(Gate::default());
    f.host.preparation_hook = Some(gate.hook());
    f.start();
    f.until(|_| gate.entered());
    assert!(f.host.is_preparing_results());
    f.host.shut_down();
    let Fixture { session, host, runtime, answer, binding, dir, .. } = f;
    drop(host);
    drop(session);
    gate.release();
    runtime.idle();
    let mut session = Session::with_storage(
        nodecraft_format::Snapshot { project: empty(), assets: Default::default(), epoch: 0 },
        Box::new(nodecraft_format::NativeStorage::new(dir.path().join("new-recovery")).unwrap()),
    )
    .unwrap();
    session.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
    assert!(session.document().canvases()[&C].nodes()[&N].history.is_empty());
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "resumed137".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut host = RunHost::new(runtime.clone(), budget);
    host.open_journal(Box::new(journal)).unwrap();
    host.install(&mut session);
    let resumed_gate = Arc::new(Gate::default());
    host.preparation_hook = Some(resumed_gate.hook());
    host.resume(&session, &binding);
    let mut f = Fixture { session, host, runtime, answer, binding, dir, notices: Vec::new() };
    f.until(|_| resumed_gate.entered());
    f.edit_while_blocked(&resumed_gate);
    resumed_gate.release();
    f.done();
    f.exact();
    f.runtime.idle();
}

#[test]
fn topaz37_late_held_writer_retains_its_old_store_after_host_and_project_close() {
    let mut f = fixture();
    let gate = Arc::new(Gate::default());
    let old = crate::held_store::MemoryHeld::default();
    f.host.open_held(Box::new(SlowStore { memory: old.clone(), gate: gate.clone(), fail: false })).unwrap();
    f.make_held();
    f.until(|_| gate.entered());
    f.host.shut_down();
    let Fixture { session, host, runtime, answer, .. } = f;
    drop(host);
    drop(session);
    let budget = JobBudget { log: Arc::default(), budgets: Arc::default(), session: "new-project137".into(), wall_clock_ms: Arc::new(|| 0) };
    let mut replacement = RunHost::new(runtime.clone(), budget);
    let new = crate::held_store::MemoryHeld::default();
    replacement.open_held(Box::new(new.clone())).unwrap();
    gate.still_blocked();
    gate.release();
    runtime.idle();
    assert_eq!(crate::held_store::HeldStore::list(&old).unwrap().records.len(), 1);
    assert!(crate::held_store::HeldStore::list(&new).unwrap().records.is_empty());
    assert!(replacement.held_results().is_empty());
    assert_eq!(answer.creates.load(Ordering::SeqCst), 1);
}

struct DroppedWorker;
impl Runtime for DroppedWorker {
    fn spawn(&self, _: TaskFuture<()>) {}
    fn sleep(&self, _: u64) -> TaskFuture<()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

#[test]
fn topaz37_dropped_held_writer_clears_busy_but_keeps_original_in_memory() {
    let held = crate::run_host::held::new(Arc::new(DroppedWorker));
    super::super::lock(&held).open(Box::<crate::held_store::MemoryHeld>::default()).unwrap();
    let id = crate::run_host::held::hold(
        &held,
        crate::run_host::held::HeldResult {
            project: ProjectId::from_bytes([137; 16]),
            canvas: C,
            node: N,
            provider: "fal".into(),
            model: "fal.topaz-precision".into(),
            reason: "card removed".into(),
            completed: Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: large_png() }]),
            routes: Vec::new(),
            recipe: None,
        },
    );
    let held = super::super::lock(&held);
    assert!(!held.is_saving());
    let views = held.views();
    let [view] = views.as_slice() else { panic!("original result retained") };
    assert_eq!(view.id, id);
    assert!(!view.saved);
    assert_eq!(view.outputs, 1);
}
