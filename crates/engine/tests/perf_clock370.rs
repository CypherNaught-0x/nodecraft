use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::json;
use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
fn session() -> Session {
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Clock safety").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    Session::new(p).unwrap()
}
fn install(s: &mut Session, fail_at: usize) -> Rc<Cell<usize>> {
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    s.perf_mut().set_clock(Some(Box::new(move || {
        let next = counter.get() + 1;
        counter.set(next);
        assert_ne!(next, fail_at, "fixture clock failure");
        next as u64 * 10
    })));
    s.perf_mut().set_enabled(true);
    calls
}
fn edit(s: &mut Session) -> serde_json::Value {
    s.dispatch("node.add", &json!({"canvas":C,"node":Node::new(N,"source.text")})).unwrap()
}
#[test]
fn first_clock_panic_does_not_prevent_exactly_one_edit_and_undo() {
    let mut s = session();
    let calls = install(&mut s, 1);
    let result = catch_unwind(AssertUnwindSafe(|| edit(&mut s)));
    println!("first-clock result_ok={} revision={} calls={}", result.is_ok(), s.revision(), calls.get());
    assert!(result.is_ok(), "optional timing must not escape dispatch");
    assert_eq!(result.unwrap(), json!({"id":N}));
    assert_eq!(s.revision(), 1);
    assert_eq!(s.document().canvases()[&C].nodes().len(), 1);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.document().canvases()[&C].nodes().is_empty());
    assert_eq!(calls.get(), 1);
}
#[test]
fn post_commit_clock_panic_preserves_success_result_and_undo() {
    let mut s = session();
    let calls = install(&mut s, 4);
    let result = catch_unwind(AssertUnwindSafe(|| edit(&mut s)));
    println!("post-clock result_ok={} revision={} calls={}", result.is_ok(), s.revision(), calls.get());
    assert!(result.is_ok(), "committed edit must return its actual success");
    assert_eq!(result.unwrap(), json!({"id":N}));
    assert_eq!(s.revision(), 1);
    assert_eq!(s.document().canvases()[&C].nodes().len(), 1);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.document().canvases()[&C].nodes().is_empty());
    assert_eq!(calls.get(), 4);
}

#[test]
fn failed_clock_preserves_command_refusal_and_replacement_restores_real_measurement() {
    let mut s = session();
    let calls = install(&mut s, 1);
    let result = catch_unwind(AssertUnwindSafe(|| s.dispatch("edit.undo", &json!({}))));
    assert!(result.is_ok());
    let error = result.unwrap().unwrap_err();
    let mut control = session();
    assert_eq!(error, control.dispatch("edit.undo", &json!({})).unwrap_err());
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo());
    assert_eq!(calls.get(), 1);
    assert!(!s.perf().report().clock);
    let ordinary = install(&mut s, usize::MAX);
    assert_eq!(edit(&mut s), json!({"id":N}));
    assert_eq!(ordinary.get(), 4);
    let report = s.perf().report();
    let dispatch = report.metrics.iter().find(|m| m.metric == nodecraft_engine::perf::Metric::Dispatch).unwrap();
    assert_eq!((dispatch.samples, dispatch.p50, dispatch.supported), (1, Some(30), true));
}

#[derive(Clone, Default)]
struct SharedStorage(Rc<std::cell::RefCell<nodecraft_format::MemoryStorage>>);
impl nodecraft_format::ProjectStorage for SharedStorage {
    fn load(&mut self, l: &nodecraft_format::StorageLocation) -> Result<Vec<u8>, nodecraft_types::Error> {
        nodecraft_format::ProjectStorage::load(&mut *self.0.borrow_mut(), l)
    }
    fn save(&mut self, l: &nodecraft_format::StorageLocation, b: &[u8]) -> Result<(), nodecraft_types::Error> {
        nodecraft_format::ProjectStorage::save(&mut *self.0.borrow_mut(), l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), nodecraft_types::Error> {
        nodecraft_format::ProjectStorage::checkpoint(&mut *self.0.borrow_mut(), b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), nodecraft_types::Error> {
        nodecraft_format::ProjectStorage::append(&mut *self.0.borrow_mut(), b)
    }
}
#[test]
fn post_commit_clock_failure_keeps_durable_edit_and_undo_in_same_recovery_owner() {
    let storage = SharedStorage::default();
    let original = session().document().clone();
    let snapshot = nodecraft_format::Snapshot { project: original.clone(), assets: Default::default(), epoch: 0 };
    let mut s = Session::with_storage(snapshot, Box::new(storage.clone())).unwrap();
    let calls = install(&mut s, 4);
    assert_eq!(edit(&mut s), json!({"id":N}));
    let recovered = nodecraft_format::journal::recover(&storage.0.borrow().recovery).unwrap();
    assert_eq!(recovered.applied, 1);
    assert_eq!(recovered.snapshot.project, *s.document());
    s.dispatch("edit.undo", &json!({})).unwrap();
    let recovered = nodecraft_format::journal::recover(&storage.0.borrow().recovery).unwrap();
    assert_eq!(recovered.applied, 2);
    assert_eq!(recovered.snapshot.project, original);
    assert_eq!(s.document(), &original);
    assert_eq!(calls.get(), 4);
}

#[test]
fn cleanup_panic_payload_cannot_hide_committed_edit_or_retry_clock() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Secondary(Arc<AtomicUsize>);
    impl Drop for Secondary {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
            panic!("secondary payload must not be dropped");
        }
    }
    struct Primary {
        drops: Arc<AtomicUsize>,
        secondary: Arc<AtomicUsize>,
    }
    impl Drop for Primary {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
            std::panic::panic_any(Secondary(self.secondary.clone()));
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let secondary = Arc::new(AtomicUsize::new(0));
    let calls = Rc::new(Cell::new(0));
    let (primary_count, secondary_count, clock_calls) = (drops.clone(), secondary.clone(), calls.clone());
    let mut s = session();
    s.perf_mut().set_clock(Some(Box::new(move || {
        clock_calls.set(clock_calls.get() + 1);
        if clock_calls.get() == 4 {
            std::panic::panic_any(Primary { drops: primary_count.clone(), secondary: secondary_count.clone() });
        }
        10
    })));
    s.perf_mut().set_enabled(true);
    assert_eq!(edit(&mut s), json!({"id":N}));
    assert_eq!(s.revision(), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(secondary.load(Ordering::Relaxed), 0);
    assert!(!s.perf().has_clock());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.document().canvases()[&C].nodes().is_empty());
    assert_eq!(calls.get(), 4);
}
