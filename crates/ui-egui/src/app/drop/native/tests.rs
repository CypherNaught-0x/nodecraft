use super::*;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize},
    mpsc::{Receiver, SyncSender, sync_channel},
};
#[derive(Debug)]
struct Dropped(PathBuf);
impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        panic!("never use egui unbounded bytes")
    }
}
fn handles(names: &[&str]) -> Vec<egui::DroppedFileHandle> {
    names.iter().map(|s| Arc::new(Dropped((*s).into())) as egui::DroppedFileHandle).collect()
}
struct FakeClock {
    base: Instant,
    ms: AtomicU64,
}
impl FakeClock {
    fn new() -> Arc<Self> {
        Arc::new(Self { base: Instant::now(), ms: AtomicU64::new(0) })
    }
    fn advance(&self, ms: u64) {
        self.ms.fetch_add(ms, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_millis(self.ms.load(Ordering::SeqCst))
    }
}
struct File {
    bytes: std::io::Cursor<Vec<u8>>,
}
impl Read for File {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.bytes.read(out)
    }
}
impl OpenedFile for File {
    fn metadata(&self) -> io::Result<(bool, u64)> {
        Ok((true, self.bytes.get_ref().len() as u64))
    }
}
struct GateBackend {
    entered: SyncSender<()>,
    permit: Mutex<Receiver<()>>,
    opens: AtomicUsize,
    clock: Arc<FakeClock>,
    advance: u64,
}
impl Acquirer for GateBackend {
    fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.entered.try_send(()).ok();
        self.permit.lock().unwrap().recv_timeout(Duration::from_secs(10)).map_err(|_| io::Error::other("bounded fixture gate expired"))?;
        self.clock.advance(self.advance);
        Ok(Box::new(File { bytes: std::io::Cursor::new(b"words".to_vec()) }))
    }
}
struct Release(SyncSender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}
fn gated(advance: u64) -> (TestService, Arc<GateBackend>, Receiver<()>, Release) {
    let (entered, ready) = sync_channel(1);
    let (permit, receive) = sync_channel(1);
    let clock = FakeClock::new();
    let backend = Arc::new(GateBackend { entered, permit: Mutex::new(receive), opens: AtomicUsize::new(0), clock: clock.clone(), advance });
    let service = Service::spawn(backend.clone(), clock).unwrap();
    (TestService { service }, backend, ready, Release(permit))
}
fn start(service: &Service, files: &[egui::DroppedFileHandle]) -> Ticket {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match service.start(files) {
            Ok(ticket) => return ticket,
            Err(Failure::Busy) if !service.0.busy.load(Ordering::Acquire) && Instant::now() < end => {
                // try_lock may meet the fixed worker's short mailbox handoff.
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("initial fixture admission: {e:?}"),
        }
    }
}
fn wait(ticket: &mut Ticket) -> Poll {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let p = ticket.poll();
        if !matches!(p, Poll::Pending) {
            return p;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn idle(service: &Service) {
    let end = Instant::now() + Duration::from_secs(10);
    while service.0.busy.load(Ordering::Acquire) {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn shared_lease_survives_cancel_and_late_result_without_replacement_worker() {
    let (worker, backend, entered, release) = gated(0);
    let mut ticket = start(&worker.service, &handles(&["path"]));
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(ticket.poll(), Poll::Pending));
    for _ in 0..50 {
        assert!(matches!(worker.service.clone().start(&handles(&["next"])), Err(Failure::Busy)));
    }
    drop(ticket);
    assert!(matches!(worker.service.start(&handles(&["next"])), Err(Failure::Busy)));
    drop(release);
    idle(&worker.service);
    assert_eq!(backend.opens.load(Ordering::SeqCst), 1);
}
#[test]
fn completed_batch_holds_slot_and_poll_is_single_use() {
    let (worker, _, entered, release) = gated(0);
    let mut ticket = start(&worker.service, &handles(&["path"]));
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(release);
    let Poll::Ready(batch) = wait(&mut ticket) else { panic!("ready") };
    assert!(matches!(ticket.poll(), Poll::Failed(Failure::Cancelled)), "single-use handoff");
    drop(ticket);
    assert_eq!(batch.files[0].bytes, b"words");
    assert!(batch.check().is_ok());
    assert!(matches!(worker.service.start(&handles(&["next"])), Err(Failure::Busy)));
    drop(batch);
    idle(&worker.service);
}
#[test]
fn absolute_deadline_rejects_blocked_and_already_queued_results() {
    for queued in [false, true] {
        let (worker, backend, entered, release) = gated(0);
        let mut ticket = start(&worker.service, &handles(&["path"]));
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        if queued {
            drop(release);
            let end = Instant::now() + Duration::from_secs(10);
            while ticket.control.result.lock().unwrap().is_none() {
                assert!(Instant::now() < end);
                std::thread::yield_now();
            }
        } else {
            backend.clock.advance(10000);
            assert!(matches!(ticket.poll(), Poll::Failed(Failure::Timeout)));
            drop(release);
        }
        backend.clock.advance(10000);
        assert!(matches!(ticket.poll(), Poll::Failed(Failure::Timeout)));
        drop(ticket);
        idle(&worker.service);
    }
}
#[test]
fn global_service_is_one_shared_nonreplaceable_worker() {
    let a = Service::global().unwrap();
    let b = Service::global().unwrap();
    assert!(Arc::ptr_eq(&a.0, &b.0));
    assert!(Arc::ptr_eq(&a.0.busy, &b.0.busy));
}
#[test]
fn type_shells_are_explicit_and_source_ownership_is_bounded() {
    use std::mem::size_of;
    let shells = MAX_FILES * (size_of::<FileRequest>() + size_of::<ReadFile>())
        + size_of::<Job>()
        + size_of::<Control>()
        + size_of::<State>()
        + size_of::<Lease>()
        + size_of::<Ticket>()
        + size_of::<Batch>()
        + size_of::<Summary>()
        + size_of::<Shared>();
    let logical = MAX_DROP_TOTAL_BYTES + MAX_FILES * (PATH_BYTES + NAME_BYTES) + 3 * PATH_BYTES + shells;
    println!(
        "native335 ownership: body={} path={} name={} count={} shells={} logical={} requested_stack={}",
        MAX_DROP_TOTAL_BYTES, PATH_BYTES, NAME_BYTES, MAX_FILES, shells, logical, STACK_BYTES
    );
    assert!(logical < 50 * 1024 * 1024);
    assert_eq!(MAX_FILES, 32);
}
#[test]
fn ui_frame_remains_responsive_and_same_revision_replacement_cannot_receive_late_drop() {
    use crate::app::{NodeCraftApp, tests::harness};
    let (worker, _, entered, release) = gated(0);
    let mut h = harness();
    h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
    h.input_mut().dropped_files = handles(&["slow"]);
    h.step();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let doc = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let canvas = h.state().canvas;
    h.state_mut().view.pan += egui::vec2(20.0, 30.0);
    h.step();
    assert_eq!(h.state().session.document(), &doc);
    assert!(h.state().drop_pending());
    h.state_mut().session = nodecraft_engine::Session::new(doc.clone()).unwrap();
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().canvas, canvas);
    h.step();
    assert!(!h.state().drop_pending());
    drop(release);
    idle(&worker.service);
    h.step();
    assert_eq!(h.state().session.document(), &doc);
    let _ = std::any::type_name::<NodeCraftApp>();
}

struct Generated {
    left: usize,
    len: u64,
    calls: Arc<AtomicUsize>,
}
impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let n = self.left.min(out.len());
        out[..n].fill(7);
        self.left -= n;
        Ok(n)
    }
}
impl OpenedFile for Generated {
    fn metadata(&self) -> io::Result<(bool, u64)> {
        Ok((true, self.len))
    }
}
struct Generate {
    len: usize,
    opens: AtomicUsize,
    calls: Arc<AtomicUsize>,
}
impl Acquirer for Generate {
    fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Generated { left: self.len, len: self.len as u64, calls: self.calls.clone() }))
    }
}
#[test]
fn aggregate_capacity_and_oversize_admission_precede_read_and_retention() {
    for len in [MAX_DROP_BYTES, MAX_DROP_BYTES + 1] {
        let backend = Arc::new(Generate { len, opens: AtomicUsize::new(0), calls: Arc::default() });
        let worker = TestService { service: Service::spawn(backend.clone(), Arc::new(Monotonic)).unwrap() };
        let mut ticket = start(&worker.service, &handles(&["a", "b", "c", "d"]));
        let Poll::Ready(batch) = wait(&mut ticket) else { panic!("ready") };
        if len == MAX_DROP_BYTES {
            assert_eq!(batch.files.len(), 3);
            assert_eq!(batch.files.iter().map(|f| f.bytes.capacity()).sum::<usize>(), MAX_DROP_TOTAL_BYTES);
            assert_eq!(backend.opens.load(Ordering::SeqCst), 3);
        } else {
            assert!(batch.files.is_empty());
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        }
        drop(batch);
        drop(ticket);
        idle(&worker.service);
    }
}
#[test]
fn one_deadline_spans_all_files_and_worker_panic_does_not_spawn_a_replacement() {
    struct Timed(Arc<FakeClock>, AtomicUsize);
    impl Acquirer for Timed {
        fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.advance(6000);
            Ok(Box::new(File { bytes: std::io::Cursor::new(b"x".to_vec()) }))
        }
    }
    let clock = FakeClock::new();
    let backend = Arc::new(Timed(clock.clone(), AtomicUsize::new(0)));
    let worker = TestService { service: Service::spawn(backend.clone(), clock).unwrap() };
    let mut ticket = start(&worker.service, &handles(&["a", "b", "c"]));
    assert!(matches!(wait(&mut ticket), Poll::Failed(Failure::Timeout)));
    drop(ticket);
    idle(&worker.service);
    assert_eq!(backend.1.load(Ordering::SeqCst), 2);
    struct Panics(AtomicUsize);
    impl Acquirer for Panics {
        fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("test backend panic")
        }
    }
    let backend = Arc::new(Panics(AtomicUsize::new(0)));
    let worker = TestService { service: Service::spawn(backend.clone(), Arc::new(Monotonic)).unwrap() };
    let identity = Arc::clone(&worker.service.0);
    for _ in 0..2 {
        let mut t = start(&worker.service, &handles(&["a"]));
        assert!(matches!(wait(&mut t), Poll::Failed(Failure::Unexpected)));
        drop(t);
        idle(&worker.service);
        assert!(Arc::ptr_eq(&identity, &worker.service.0));
    }
    assert_eq!(backend.0.load(Ordering::SeqCst), 2);
}
#[test]
fn intake_reads_only_first_thirty_two_paths_without_retaining_original_handles() {
    use std::sync::Weak;
    struct Counted {
        path: PathBuf,
        me: Weak<Counted>,
        calls: Arc<AtomicUsize>,
        owners: Arc<AtomicUsize>,
    }
    impl std::fmt::Debug for Counted {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("counted")
        }
    }
    impl egui::DroppedFile for Counted {
        fn path(&self) -> &Path {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let me = self.me.upgrade().unwrap();
            self.owners.fetch_max(Arc::strong_count(&me), Ordering::SeqCst);
            &self.path
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            panic!("never read handle bytes")
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let owners = Arc::new(AtomicUsize::new(0));
    let mut files = Vec::new();
    for _ in 0..1000 {
        let value = Arc::new_cyclic(|me| Counted { path: "somewhere".into(), me: me.clone(), calls: calls.clone(), owners: owners.clone() });
        files.push(value as egui::DroppedFileHandle);
    }
    let clone = files.clone();
    clone[0].path();
    assert_eq!(owners.load(Ordering::SeqCst), 3, "positive clone counter");
    drop(clone);
    calls.store(0, Ordering::SeqCst);
    owners.store(0, Ordering::SeqCst);
    let backend = Arc::new(Generate { len: 1, opens: AtomicUsize::new(0), calls: Arc::default() });
    let worker = TestService { service: Service::spawn(backend, Arc::new(Monotonic)).unwrap() };
    let mut ticket = start(&worker.service, &files);
    assert_eq!(calls.load(Ordering::SeqCst), 32);
    assert_eq!(owners.load(Ordering::SeqCst), 2);
    let Poll::Ready(batch) = wait(&mut ticket) else { panic!("ready") };
    assert_eq!(batch.files.len(), 32);
    assert_eq!(batch.skipped.count, 968);
    drop(batch);
    drop(ticket);
    idle(&worker.service);
}
#[test]
fn oversized_and_non_utf8_native_names_are_bounded_without_truncating_the_path() {
    let backend = Arc::new(Generate { len: 1, opens: AtomicUsize::new(0), calls: Arc::default() });
    let worker = TestService { service: Service::spawn(backend.clone(), Arc::new(Monotonic)).unwrap() };
    let files = handles(&[&"p".repeat(PATH_BYTES + 1)]);
    let mut t = start(&worker.service, &files);
    let Poll::Ready(batch) = wait(&mut t) else { panic!("ready") };
    assert_eq!(backend.opens.load(Ordering::SeqCst), 0);
    assert_eq!(batch.skipped.count, 1);
    drop(batch);
    drop(t);
    idle(&worker.service);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let file = Arc::new(Dropped(PathBuf::from(std::ffi::OsString::from_vec(vec![0xff; 400])))) as egui::DroppedFileHandle;
        let mut t = start(&worker.service, &[file]);
        let Poll::Ready(batch) = wait(&mut t) else { panic!("ready") };
        assert!(batch.files[0].name.chars().count() <= 80);
        assert!(batch.files[0].name.capacity() <= NAME_BYTES);
        drop(batch);
        drop(t);
        idle(&worker.service);
    }
}
#[test]
fn ui_revision_canvas_close_and_cancel_button_never_deliver_late_bytes() {
    use crate::app::tests::harness;
    use egui_kittest::kittest::Queryable;
    for mode in 0..4 {
        let (worker, _, entered, release) = gated(0);
        let mut h = harness();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        let before = h.state().session.document().clone();
        h.input_mut().dropped_files = handles(&["slow"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        h.run();
        match mode {
            0 => {
                let canvas = h.state().canvas;
                let id = *h.state().selection.nodes().iter().next().unwrap_or(&nodecraft_types::NodeId::from_bytes([3; 16]));
                h.state_mut().session.dispatch("node.move", &serde_json::json!({"canvas":canvas,"id":id,"pos":{"x":1,"y":2}})).unwrap();
                h.step();
                assert!(!h.state().drop_pending());
            }
            1 => {
                h.state_mut().canvas = nodecraft_types::CanvasId::from_bytes([99; 16]);
                h.step();
                assert!(!h.state().drop_pending());
            }
            2 => {
                h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel file reading").click();
                h.run();
                assert!(!h.state().drop_pending());
                assert_eq!(h.state().session.document(), &before);
            }
            _ => {
                drop(h);
                drop(release);
                idle(&worker.service);
                continue;
            }
        }
        let after = h.state().session.document().clone();
        drop(release);
        idle(&worker.service);
        h.step();
        assert_eq!(h.state().session.document(), &after);
    }
}
#[test]
fn pending_pointer_control_owns_initial_held_release_in_both_event_orders() {
    use crate::app::tests::harness;
    for reversed in [false, true] {
        let (worker, _, entered, release) = gated(0);
        let mut h = harness();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        h.input_mut().dropped_files = handles(&["slow"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let selection = h.state().selection.clone();
        let at = h.state().drop_acquisition.rect.center();
        let press = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
        if reversed {
            h.event(press);
            h.hover_at(at);
        } else {
            h.hover_at(at);
            h.event(press);
        }
        h.step();
        assert!(h.state().drop_pointer_owned());
        let ctx = h.ctx.clone();
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.pan_to", &serde_json::json!({"x":0,"y":0})).is_err());
        h.hover_at(at + egui::vec2(400.0, -200.0));
        h.step();
        assert!(h.state().drop_pointer_owned());
        h.event(egui::Event::PointerButton {
            pos: at + egui::vec2(400.0, -200.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().selection, selection);
        h.state_mut().drop_acquisition.pending = None;
        drop(release);
        idle(&worker.service);
    }
}

#[test]
fn public_persistent_open_cancels_but_save_with_unchanged_revision_accepts_fresh_import() {
    use crate::app::{NodeCraftApp, tests::harness};
    use nodecraft_format::{MemoryStorage, Snapshot, StorageLocation};
    for operation in ["open", "save", "failed-open"] {
        let (worker, _, entered, release) = gated(0);
        let mut h = harness();
        let canvas = h.state().canvas;
        let original = h.state().session.document().clone();
        let snapshot = Snapshot { project: original.clone(), assets: Default::default(), epoch: 0 };
        let archive = nodecraft_format::encode(&snapshot).unwrap();
        let mut storage = MemoryStorage::default();
        storage.files.insert(StorageLocation::Path("same-project".into()), archive);
        let session = nodecraft_engine::Session::with_storage(snapshot, Box::new(storage)).unwrap();
        *h.state_mut() = NodeCraftApp::from_session(session, canvas).unwrap();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        h.step();
        h.input_mut().dropped_files = handles(&["slow"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        let captured = h.state().drop_acquisition.pending.as_ref().unwrap().position;
        let revision = h.state().session.revision();
        let identity = crate::navigation::Geometry::identity(&mut h.state_mut().session);
        match operation {
            "open" => {
                h.state_mut().session.dispatch("file.open", &serde_json::json!({"location":"same-project","discard_dirty":true})).unwrap();
                assert!(h.state().session.revision() > revision);
                assert!(Arc::ptr_eq(&identity, &crate::navigation::Geometry::identity(&mut h.state_mut().session)));
            }
            "save" => {
                let result = h.state_mut().session.dispatch("file.save_as", &serde_json::json!({"location":"saved"})).unwrap();
                assert_eq!(result["saved"], true);
                assert_eq!(h.state().session.revision(), revision);
            }
            _ => {
                assert!(h.state_mut().session.dispatch("file.open", &serde_json::json!({"location":"missing","discard_dirty":true})).is_err());
                assert_eq!(h.state().session.revision(), revision);
            }
        }
        h.step();
        if operation == "open" {
            assert!(!h.state().drop_pending());
        }
        h.state_mut().view.pan += egui::vec2(40.0, 20.0);
        drop(release);
        let end = Instant::now() + Duration::from_secs(10);
        while h.state().drop_pending() {
            h.step();
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        idle(&worker.service);
        h.step();
        if operation == "open" {
            assert_eq!(h.state().session.document(), &original);
            assert!(!h.state().session.can_undo());
        } else {
            let scene = &h.state().session.document().canvases()[&canvas];
            assert_eq!(scene.nodes().len(), original.canvases()[&canvas].nodes().len() + 1);
            let id = *h.state().selection.nodes().iter().next().unwrap();
            assert_eq!(scene.nodes()[&id].pos, captured);
            h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
            assert_eq!(h.state().session.document(), &original);
        }
    }
}
#[test]
fn visible_pending_cancel_unavailable_and_success_are_real_ui_states() {
    use crate::app::tests::harness;
    use egui_kittest::kittest::Queryable;
    let (worker, _, entered, release) = gated(0);
    let mut h = harness();
    h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
    h.step();
    h.input_mut().dropped_files = handles(&["public-note.txt"]);
    h.step();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel file reading").click();
    h.run();
    assert!(!h.state().drop_pending());
    h.step();
    h.input_mut().dropped_files = handles(&["retry.txt"]);
    h.step();
    assert!(!h.state().drop_pending());
    assert!(h.state().notice.as_ref().unwrap().contains("still finishing"));
    drop(release);
    idle(&worker.service);
    let files = TestService::filesystem();
    h.state_mut().drop_acquisition.service = Ok(files.service.clone());
    // This final example uses the same real filesystem UI test route, with a public-safe fixture.
    let path = std::env::temp_dir().join(format!("nodecraft-public-drop335-{}.txt", std::process::id()));
    std::fs::write(&path, b"A public example note.").unwrap();
    h.input_mut().dropped_files = vec![Arc::new(Dropped(path.clone()))];
    h.step();
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().drop_pending() {
        h.step();
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    h.step();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn outside_press_does_not_transfer_ownership_when_pointer_moves_inside_control() {
    use crate::app::tests::harness;
    for reversed in [false, true] {
        let (worker, _, entered, release) = gated(0);
        let mut h = harness();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        h.input_mut().dropped_files = handles(&["slow"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        h.run();
        let at = h.state().drop_acquisition.rect.center();
        let outside = egui::pos2(800.0, 500.0);
        let press = egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
        if reversed {
            h.event(press);
            h.hover_at(at);
        } else {
            h.hover_at(at);
            h.event(press);
        }
        h.step();
        assert!(!h.state().drop_pointer_owned(), "press origin owns gesture");
        h.hover_at(at);
        h.step();
        assert!(!h.state().drop_pointer_owned());
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.step();
        assert!(h.state().drop_pending(), "release does not become Cancel");
        h.state_mut().drop_acquisition.pending = None;
        drop(release);
        idle(&worker.service);
    }
}
#[test]
fn cancel_has_priority_over_an_already_completed_queued_result() {
    use crate::app::tests::harness;
    use egui_kittest::kittest::Queryable;
    let (worker, _, entered, release) = gated(0);
    let mut h = harness();
    h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
    h.input_mut().dropped_files = handles(&["slow"]);
    h.step();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    h.run();
    let rect = h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel file reading").rect();
    let at = rect.center();
    h.hover_at(at);
    h.run();
    let document = h.state().session.document().clone();
    drop(release);
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().drop_acquisition.pending.as_ref().unwrap().ticket.control.result.lock().unwrap().is_none() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().drop_pointer_owned());
    assert!(h.state().drop_pending(), "ready must not remove held Cancel");
    assert_eq!(h.state().session.document(), &document);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(!h.state().drop_pending());
    assert_eq!(h.state().session.document(), &document);
    assert!(!h.state().session.can_undo());
    assert!(h.state().notice.as_deref().unwrap().contains("cancelled"));
    idle(&worker.service);
}
#[cfg(unix)]
#[test]
fn real_symlink_to_regular_file_uses_opened_handle_metadata() {
    let dir = std::env::temp_dir().join(format!("nodecraft-drop335-symlink-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target.txt");
    let link = dir.join("link.txt");
    std::fs::write(&target, b"real opened content").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let worker = TestService::filesystem();
    let mut ticket = start(&worker.service, &[Arc::new(Dropped(link))]);
    let Poll::Ready(batch) = wait(&mut ticket) else { panic!("ready") };
    assert_eq!(batch.files[0].bytes, b"real opened content");
    drop(batch);
    drop(ticket);
    idle(&worker.service);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn opened_object_failures_skip_individually_and_keep_a_later_regular_file() {
    struct NotRegular;
    impl Read for NotRegular {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("must not read nonregular opened object")
        }
    }
    impl OpenedFile for NotRegular {
        fn metadata(&self) -> io::Result<(bool, u64)> {
            Ok((false, 1))
        }
    }
    struct Sequence(AtomicUsize);
    impl Acquirer for Sequence {
        fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
            match self.0.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(Box::new(NotRegular)),
                1 => Err(io::ErrorKind::PermissionDenied.into()),
                _ => Ok(Box::new(File { bytes: std::io::Cursor::new(b"later regular".to_vec()) })),
            }
        }
    }
    let worker = TestService { service: Service::spawn(Arc::new(Sequence(AtomicUsize::new(0))), Arc::new(Monotonic)).unwrap() };
    let mut ticket = start(&worker.service, &handles(&["regular-before-open", "private", "later"]));
    let Poll::Ready(batch) = wait(&mut ticket) else { panic!("ready") };
    assert_eq!(batch.files.len(), 1);
    assert_eq!(batch.files[0].bytes, b"later regular");
    assert_eq!(batch.skipped.count, 2);
    assert_eq!(batch.skipped.first.as_ref().unwrap().1, Reason::Nonregular);
    drop(batch);
    drop(ticket);
    idle(&worker.service);
}
#[test]
fn a_folder_that_cannot_be_opened_is_skipped_as_not_a_file() {
    struct Folder;
    impl Acquirer for Folder {
        fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
            Err(io::ErrorKind::IsADirectory.into())
        }
    }
    let worker = TestService { service: Service::spawn(Arc::new(Folder), Arc::new(Monotonic)).unwrap() };
    let mut ticket = start(&worker.service, &handles(&["folder"]));
    let Poll::Ready(batch) = wait(&mut ticket) else { panic!("ready") };
    assert!(batch.files.is_empty());
    assert_eq!(batch.skipped.count, 1);
    assert_eq!(batch.skipped.first.as_ref().unwrap().1, Reason::Nonregular);
    drop(batch);
    drop(ticket);
    idle(&worker.service);
}
#[test]
fn ready_delivery_resumes_after_an_owned_release_outside_cancel() {
    use crate::app::tests::harness;
    let (worker, _, entered, release) = gated(0);
    let mut h = harness();
    h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
    h.input_mut().dropped_files = handles(&["slow"]);
    h.step();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    h.run();
    let before = h.state().session.document().clone();
    let at = h.state().drop_acquisition.rect.center();
    h.hover_at(at);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().drop_pointer_owned());
    drop(release);
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().drop_acquisition.pending.as_ref().unwrap().ticket.control.result.lock().unwrap().is_none() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    let outside = egui::pos2(800.0, 500.0);
    h.hover_at(outside);
    h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert_eq!(h.state().session.document(), &before, "release frame cannot click through");
    h.step();
    assert!(!h.state().drop_pending());
    assert!(!h.state().drop_pointer_owned());
    assert_ne!(h.state().session.document(), &before);
    assert_eq!(h.state().selection.nodes().len(), 1);
    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
    idle(&worker.service);
}
#[test]
fn pending_control_yields_to_focused_text_and_unavailable_service_is_explicit() {
    use crate::app::tests::harness;
    use egui_kittest::kittest::Queryable;
    let (worker, _, entered, release) = gated(0);
    let mut h = harness();
    h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
    h.input_mut().dropped_files = handles(&["slow"]);
    h.step();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search canvas").click();
    h.run();
    assert!(crate::canvas::text_input_focused(&h.ctx));
    let at = h.state().drop_acquisition.rect.center();
    h.hover_at(at);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(!h.state().drop_pointer_owned());
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert!(h.state().drop_pending());
    h.state_mut().drop_acquisition.pending = None;
    drop(release);
    idle(&worker.service);
    h.state_mut().drop_acquisition.service = Err(Failure::Unavailable);
    h.input_mut().dropped_files = handles(&["not-opened"]);
    h.step();
    assert!(!h.state().drop_pending());
    assert!(h.state().notice.as_deref().unwrap().contains("Restart the app"));
}

fn shortcut_fixture() -> egui_kittest::Harness<'static, crate::app::NodeCraftApp> {
    let mut h = crate::app::tests::harness();
    let canvas = h.state().canvas;
    let id = *h.state().session.document().canvases()[&canvas].nodes().keys().next().unwrap();
    h.state_mut().create_node("source.text", egui::pos2(400.0, 0.0));
    for title in ["First title", "Redo title"] {
        h.state_mut().session.dispatch("node.title", &serde_json::json!({"canvas":canvas,"id":id,"title":title})).unwrap();
    }
    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    h.state_mut().selection.only(id);
    h.run();
    assert!(h.state().session.can_undo() && h.state().session.can_redo());
    assert_eq!(h.state().session.document().canvases()[&canvas].nodes().len(), 2);
    h
}
fn shortcut_cases() -> [(egui::Key, egui::Modifiers); 10] {
    use egui::{Key, Modifiers};
    [
        (Key::T, Modifiers::NONE),
        (Key::I, Modifiers::NONE),
        (Key::Z, Modifiers::COMMAND),
        (Key::Z, Modifiers::COMMAND | Modifiers::SHIFT),
        (Key::Delete, Modifiers::NONE),
        (Key::Backspace, Modifiers::NONE),
        (Key::A, Modifiers::COMMAND),
        (Key::Escape, Modifiers::NONE),
        (Key::F, Modifiers::COMMAND),
        (Key::F, Modifiers::CTRL),
    ]
}
fn key_events(key: egui::Key, modifiers: egui::Modifiers) -> [egui::Event; 2] {
    [true, false].map(|pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers })
}
#[test]
fn owned_pending_control_excludes_canvas_shortcuts_and_search_in_all_pointer_phases() {
    for reversed in [false, true] {
        for (key, modifiers) in shortcut_cases() {
            let (worker, _, entered, release) = gated(0);
            let mut h = shortcut_fixture();
            h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
            h.input_mut().dropped_files = handles(&["parked"]);
            h.step();
            entered.recv_timeout(Duration::from_secs(10)).unwrap();
            h.run();
            let before = h.state().session.document().clone();
            let revision = h.state().session.revision();
            let events = h.state().session.events_since(0).unwrap();
            let history = (h.state().session.can_undo(), h.state().session.can_redo());
            let selection = h.state().selection.clone();
            let view = h.state().view;
            let focus = h.ctx.memory(|m| m.focused());
            let at = h.state().drop_acquisition.rect.center();
            let outside = egui::pos2(800.0, 500.0);
            for phase in 0..3 {
                let pointer = egui::Event::PointerButton {
                    pos: if phase == 2 { outside } else { at },
                    button: egui::PointerButton::Primary,
                    pressed: phase != 2,
                    modifiers: egui::Modifiers::NONE,
                };
                let moved = egui::Event::PointerMoved(if phase == 2 { outside } else { at });
                // RawInput events deliberately share ONE real egui frame. Harness::event
                // would run each event in a separate frame and miss ordering regressions.
                let input = h.input_mut();
                input.events.push(egui::Event::ModifiersChanged(modifiers));
                if phase != 1 {
                    if reversed {
                        input.events.extend([pointer, moved]);
                    } else {
                        input.events.extend([moved, pointer]);
                    }
                }
                input.events.extend(key_events(key, modifiers));
                h.step();
                assert!(h.state().drop_pointer_owned(), "{key:?} phase{phase} order{reversed}");
                assert_eq!(h.state().session.document(), &before, "{key:?} phase{phase} order{reversed}");
                assert_eq!(h.state().session.revision(), revision);
                assert_eq!(h.state().session.events_since(0).unwrap(), events);
                assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
                assert_eq!(h.state().selection, selection);
                assert_eq!(h.state().view, view);
                assert_eq!(h.ctx.memory(|m| m.focused()), focus);
                assert!(!h.state().search.open);
                assert!(h.state().drop_pending(), "{key:?} phase{phase} order{reversed} notice{:?}", h.state().notice);
                if matches!(key, egui::Key::T | egui::Key::I | egui::Key::F) {
                    assert!(
                        h.ctx.input(|i| i.events.iter().any(|e| matches!(e,egui::Event::Key{key:k,pressed:true,..} if *k==key))),
                        "owned key must not be consumed"
                    );
                }
                let ctx = h.ctx.clone();
                assert!(h.state_mut().search_enabled(&ctx, "canvas.search_open", &serde_json::json!({})).is_err());
                assert!(h.state_mut().dispatch_search(&ctx, "canvas.search_open", &serde_json::json!({})).is_err());
            }
            h.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
            h.step();
            assert!(!h.state().drop_pointer_owned());
            assert!(h.state().drop_pending());
            h.state_mut().drop_acquisition.pending = None;
            drop(release);
            idle(&worker.service);
        }
    }
}
#[test]
fn nonowned_pending_read_keeps_ordinary_canvas_shortcuts_and_search_available() {
    for (key, modifiers) in shortcut_cases() {
        let (worker, _, entered, release) = gated(0);
        let mut h = shortcut_fixture();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        h.input_mut().dropped_files = handles(&["parked"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let canvas = h.state().canvas;
        assert!(!h.state().drop_pointer_owned());
        let ctx = h.ctx.clone();
        assert!(h.state_mut().search_enabled(&ctx, "canvas.search_open", &serde_json::json!({})).is_ok());
        h.input_mut().events.push(egui::Event::ModifiersChanged(modifiers));
        h.input_mut().events.extend(key_events(key, modifiers));
        h.step();
        match key {
            egui::Key::A => assert_eq!(h.state().selection.nodes().len(), 2),
            egui::Key::Escape => assert!(h.state().selection.nodes().is_empty()),
            egui::Key::F => assert!(h.state().search.open),
            egui::Key::T | egui::Key::I => assert_eq!(h.state().session.document().canvases()[&canvas].nodes().len(), 3),
            egui::Key::Delete | egui::Key::Backspace => assert_eq!(h.state().session.document().canvases()[&canvas].nodes().len(), 1),
            _ => {
                assert_ne!(h.state().session.document(), &before);
                assert_eq!(h.state().session.revision(), revision + 1);
            }
        }
        if matches!(key, egui::Key::A | egui::Key::Escape | egui::Key::F) {
            assert!(h.state().drop_pending());
        }
        h.state_mut().drop_acquisition.pending = None;
        drop(release);
        idle(&worker.service);
    }
}

#[test]
fn cancel_uses_actual_same_frame_release_positions_and_explicit_nonpointer_activation() {
    use crate::app::tests::harness;
    use egui_kittest::kittest::Queryable;
    for reversed in [false, true] {
        for mode in 0..4 {
            let (worker, _, entered, release) = gated(0);
            let mut h = harness();
            h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
            h.input_mut().dropped_files = handles(&["parked"]);
            h.step();
            entered.recv_timeout(Duration::from_secs(10)).unwrap();
            h.run();
            let at = h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel file reading").rect().center();
            let before = h.state().session.document().clone();
            let press = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
            let moved = egui::Event::PointerMoved(at);
            h.input_mut().events.extend(if reversed { [press, moved] } else { [moved, press] });
            h.step();
            assert!(h.state().drop_pointer_owned());
            let outside = egui::pos2(800.0, 500.0);
            let mut positions = match mode {
                0 => vec![at],
                1 => vec![outside],
                2 => vec![at, outside, at],
                _ => vec![at; 33], //66 events: conservative bounded-scan refusal.
            };
            for pos in positions.drain(..) {
                let release = egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE };
                let moved = egui::Event::PointerMoved(pos);
                h.input_mut().events.extend(if reversed { [release, moved] } else { [moved, release] });
            }
            h.step();
            assert_eq!(h.state().drop_pending(), mode != 0, "release mode{mode} order{reversed}");
            assert_eq!(h.state().session.document(), &before);
            h.state_mut().drop_acquisition.pending = None;
            drop(release);
            idle(&worker.service);
        }
    }
    for accesskit in [false, true] {
        let (worker, _, entered, release) = gated(0);
        let mut h = harness();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        h.input_mut().dropped_files = handles(&["parked"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        h.run();
        if accesskit {
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel file reading").click_accesskit();
        } else {
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel file reading").focus();
            h.run();
            h.key_press(egui::Key::Enter);
        }
        h.run();
        assert!(!h.state().drop_pending(), "explicit activation accesskit{accesskit}");
        drop(release);
        idle(&worker.service);
    }
}

#[test]
fn model_picker_yields_to_drop_control_through_release_but_not_ordinary_pending_io() {
    use crate::app::tests::harness;
    use nodecraft_types::{ErrorKind, NodeId, ParamValues, Value};
    for reversed in [false, true] {
        let (worker, _, entered, release) = gated(0);
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = NodeId::from_bytes([157; 16]);
        let values: ParamValues =
            [("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("Public model choice".into()))].into();
        let node = h.state().session.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas":canvas,"node":node})).unwrap();
        h.state_mut().drop_acquisition.service = Ok(worker.service.clone());
        h.input_mut().dropped_files = handles(&["slow"]);
        h.step();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let at = h.state().drop_acquisition.rect.center();
        let press = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
        if reversed {
            h.event(press);
            h.hover_at(at);
        } else {
            h.hover_at(at);
            h.event(press);
        }
        h.step();
        assert!(h.state().drop_pointer_owned());
        assert_eq!(h.state_mut().open_model_picker(canvas, vec![id], revision).unwrap_err().kind, ErrorKind::Conflict);
        let outside = at + egui::vec2(400.0, -200.0);
        h.hover_at(outside);
        h.step();
        assert!(h.state().drop_pointer_owned());
        assert_eq!(h.state_mut().open_model_picker(canvas, vec![id], revision).unwrap_err().kind, ErrorKind::Conflict);
        h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.step();
        assert!(h.state().drop_pointer_owned());
        assert_eq!(h.state_mut().open_model_picker(canvas, vec![id], revision).unwrap_err().kind, ErrorKind::Conflict);
        h.step();
        assert!(!h.state().drop_pointer_owned());
        assert!(h.state().drop_pending());
        h.state_mut().open_model_picker(canvas, vec![id], revision).unwrap();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().model_choice.is_none());
        assert_eq!(h.state().session.document(), &before);
        h.state_mut().drop_acquisition.pending = None;
        drop(release);
        idle(&worker.service);
    }
}
