use super::*;
use crate::Session;
use nodecraft_plugin_store::MemoryBackend;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

fn host() -> PluginHost {
    PluginHost { trust: None, taken: Default::default(), bindings: Default::default() }
}
fn memory() -> Result<Store<MemoryBackend>, Error> {
    Store::open(MemoryBackend::default())
}
fn wait(worker: &PluginWorker, id: &str) -> Value {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        match worker.result(id) {
            Ok(value) if matches!(value["status"].as_str(), Some("completed" | "failed")) => return value,
            Ok(_) => {}
            Err(error) => assert_eq!(error.kind, ErrorKind::Busy),
        }
        assert!(Instant::now() < until, "plugin operation didn't finish");
        // Sleep rather than spin: a tight try_lock loop can starve the worker's blocking lock
        // on a loaded runner, since std's mutex is not fair.
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn session(worker: Arc<PluginWorker>) -> Session {
    let mut services = Services::default();
    register_plugin_worker(&mut services, worker).unwrap();
    Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "Plugin fixture").unwrap()).unwrap().with_services(services)
}

#[test]
fn initialization_is_off_thread_and_public_results_repeat_until_acknowledged() {
    let (entered, calls) = sync_channel(1);
    let (release, permits) = sync_channel(1);
    let caller = std::thread::current().id();
    let worker = PluginWorker::spawn(
        move || {
            entered.send(std::thread::current().id()).unwrap();
            permits.recv_timeout(Duration::from_secs(10)).unwrap();
            memory()
        },
        host(),
    )
    .unwrap();
    assert!(calls.try_recv().is_err(), "initialization is lazy");
    let mut s = session(Arc::clone(&worker));
    let before = (s.document().clone(), s.revision(), s.events_since(0).unwrap(), s.can_undo(), s.can_redo());
    let answer = s.dispatch("plugins.list", &json!({})).unwrap();
    let id = answer["pending"].as_str().unwrap();
    assert_ne!(calls.recv_timeout(Duration::from_secs(10)).unwrap(), caller);
    assert_eq!(s.dispatch("plugins.result", &json!({"pending":id})).unwrap()["status"], "running");
    assert_eq!(s.dispatch("plugins.forget", &json!({"pending":id})).unwrap_err().kind, ErrorKind::Conflict);
    release.send(()).unwrap();
    let done = wait(&worker, id);
    assert_eq!(done["status"], "completed");
    assert_eq!(done["result"], json!({"packages":[]}));
    assert_eq!(s.dispatch("plugins.result", &json!({"pending":id})).unwrap(), done);
    assert_eq!(s.dispatch("plugins.result", &json!({"pending":id})).unwrap(), done);
    assert_eq!(s.dispatch("plugins.forget", &json!({"pending":id})).unwrap(), json!({"forgotten":true}));
    assert_eq!(s.dispatch("plugins.result", &json!({"pending":id})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!((s.document().clone(), s.revision(), s.events_since(0).unwrap(), s.can_undo(), s.can_redo()), before);
}

#[test]
fn unfinished_capacity_is_preserved_and_oldest_terminals_yield_to_new_requests() {
    let (entered, calls) = sync_channel(1);
    let (release, permits) = sync_channel(1);
    let worker = PluginWorker::spawn(
        move || {
            entered.send(()).unwrap();
            permits.recv_timeout(Duration::from_secs(10)).unwrap();
            memory()
        },
        host(),
    )
    .unwrap();
    let mut ids = vec![worker.submit(&json!({"op":"list"})).unwrap()];
    calls.recv_timeout(Duration::from_secs(10)).unwrap();
    for _ in 1..MAX_PENDING {
        ids.push(worker.submit(&json!({"op":"list"})).unwrap());
    }
    assert_eq!(worker.submit(&json!({"op":"list"})).unwrap_err().kind, ErrorKind::Busy);
    release.send(()).unwrap();
    let values: Vec<_> = ids.iter().map(|id| wait(&worker, id)).collect();
    for (id, value) in ids.iter().zip(&values) {
        assert_eq!(value["pending"], *id);
        assert_eq!(value["result"], json!({"packages":[]}));
        assert_eq!(worker.result(id).unwrap(), *value);
    }
    let next = worker.submit(&json!({"op":"prune"})).unwrap();
    assert!(!ids.contains(&next));
    assert_eq!(wait(&worker, &next)["result"], json!({"removed":[]}));
    assert_eq!(worker.result(&ids[0]).unwrap_err().kind, ErrorKind::NotFound, "oldest terminal was reclaimed");
    for (id, value) in ids.iter().zip(&values).skip(1) {
        assert_eq!(worker.result(id).unwrap(), *value);
    }
    // Forget swaps a later entry with the last physical slot; order remains admission order.
    worker.forget(&ids[3]).unwrap();
    let fresh = worker.submit(&json!({"op":"list"})).unwrap();
    wait(&worker, &fresh);
    assert_eq!(worker.result(&ids[1]).unwrap(), values[1], "free capacity does not evict");
    let newest = worker.submit(&json!({"op":"list"})).unwrap();
    wait(&worker, &newest);
    assert_eq!(worker.result(&ids[1]).unwrap_err().kind, ErrorKind::NotFound, "oldest by sequence, not physical Vec position");
    assert_eq!(worker.result(&ids[2]).unwrap(), values[2]);
    assert_eq!(worker.result(&next).unwrap()["status"], "completed");
}

struct ReadGate {
    memory: MemoryBackend,
    panic: Arc<std::sync::atomic::AtomicBool>,
    entered: SyncSender<()>,
    release: Receiver<()>,
}
impl StoreBackend for ReadGate {
    fn read_index(&self) -> Result<Option<Vec<u8>>, Error> {
        if self.panic.load(Ordering::Relaxed) {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
            panic!("private fixture panic");
        }
        self.memory.read_index()
    }
    fn write_index(&mut self, b: &[u8]) -> Result<(), Error> {
        self.memory.write_index(b)
    }
    fn read_object(&self, d: &PackageDigest) -> Result<Vec<u8>, Error> {
        self.memory.read_object(d)
    }
    fn write_object(&mut self, d: &PackageDigest, b: &[u8]) -> Result<(), Error> {
        self.memory.write_object(d, b)
    }
    fn remove_object(&mut self, d: &PackageDigest) -> Result<(), Error> {
        self.memory.remove_object(d)
    }
    fn list_objects(&self) -> Result<Vec<PackageDigest>, Error> {
        self.memory.list_objects()
    }
}
#[test]
fn a_panicking_backend_fails_active_and_queued_requests_but_keeps_earlier_results() {
    let panic = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&panic);
    let (entered, calls) = sync_channel(1);
    let (release, permit) = sync_channel(1);
    let mut backend = Some(ReadGate { memory: Default::default(), panic: flag, entered, release: permit });
    let worker = PluginWorker::spawn(move || Store::open(backend.take().unwrap()), host()).unwrap();
    let old = worker.submit(&json!({"op":"list"})).unwrap();
    let old_value = wait(&worker, &old);
    panic.store(true, Ordering::Relaxed);
    let current = worker.submit(&json!({"op":"list"})).unwrap();
    calls.recv_timeout(Duration::from_secs(10)).unwrap();
    let queued = worker.submit(&json!({"op":"prune"})).unwrap();
    release.send(()).unwrap();
    let failed = wait(&worker, &current);
    let not_started = wait(&worker, &queued);
    assert_eq!(failed["pending"], current);
    assert_eq!(not_started["pending"], queued);
    assert_eq!(failed["status"], "failed");
    assert_eq!(not_started["status"], "failed");
    let e: Error = serde_json::from_value(failed["error"].clone()).unwrap();
    assert_eq!(e.kind, ErrorKind::Internal);
    assert!(e.message.contains("may already have changed"));
    assert!(!e.message.contains("private fixture"));
    assert!(not_started["error"]["message"].as_str().unwrap().contains("before this request started"));
    assert_eq!(worker.result(&old).unwrap(), old_value);
    assert_eq!(worker.submit(&json!({"op":"list"})).unwrap_err().kind, ErrorKind::Internal, "a stopped/disconnected worker accepts no new owners");
    assert_eq!(worker.result(&current).unwrap(), failed);
}

#[test]
fn initialization_errors_are_typed_and_only_a_later_explicit_request_retries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let worker = PluginWorker::spawn(
        move || {
            if counter.fetch_add(1, Ordering::Relaxed) == 0 { Err(Error::new(ErrorKind::Busy, "fixture store locked")) } else { memory() }
        },
        host(),
    )
    .unwrap();
    let id = worker.submit(&json!({"op":"list"})).unwrap();
    let failed = wait(&worker, &id);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let error: Error = serde_json::from_value(failed["error"].clone()).unwrap();
    assert_eq!(error.kind, ErrorKind::Busy);
    let next = worker.submit(&json!({"op":"list"})).unwrap();
    assert_eq!(wait(&worker, &next)["status"], "completed");
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(worker.result(&id).unwrap(), failed);
}

#[test]
fn invalid_inputs_foreign_ids_and_overflow_do_not_enter_the_factory() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let worker = PluginWorker::spawn(
        move || {
            count.fetch_add(1, Ordering::Relaxed);
            memory()
        },
        host(),
    )
    .unwrap();
    for bad in [
        Value::Null,
        json!({"op":"list","extra":[]}),
        json!({"op":"import","location":"x".repeat(4097)}),
        json!({"op":"enable","package_id":"bad"}),
        json!({"op":"rollback","package_id":"io.example.one","digest":"bad"}),
        json!({"op":"grant","package_id":"bad"}),
        json!({"op":"grant","package_id":"io.example.one","grant_id":"g1"}),
        json!({"op":"revoke","grant_id":"g0"}),
        json!({"op":"revoke","grant_id":1}),
        json!({"op":"revoke","grant_id":"g1","package_id":"io.example.one"}),
    ] {
        assert_eq!(worker.submit(&bad).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    assert_eq!(worker.result(&"x".repeat(MAX_PENDING_ID + 1)).unwrap_err().kind, ErrorKind::InvalidInput);
    worker.next.store(u64::MAX, Ordering::Relaxed);
    assert_eq!(worker.submit(&json!({"op":"list"})).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let a = PluginWorker::spawn(memory, host()).unwrap();
    let b = PluginWorker::spawn(memory, host()).unwrap();
    // Well-formed grant requests reach the store, which answers them.
    let grant = a.submit(&json!({"op":"grant","package_id":"io.example.one"})).unwrap();
    assert_eq!(wait(&a, &grant)["status"], "failed");
    let revoke = a.submit(&json!({"op":"revoke","grant_id":"g1"})).unwrap();
    assert_eq!(wait(&a, &revoke)["status"], "failed");
    let aid = a.submit(&json!({"op":"list"})).unwrap();
    let bid = b.submit(&json!({"op":"list"})).unwrap();
    assert_ne!(aid, bid);
    wait(&a, &aid);
    wait(&b, &bid);
    assert_eq!(b.result(&aid).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(b.forget(&aid).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn the_result_wire_limit_counts_escaping_and_keeps_an_explicit_failure() {
    let id = "fixture";
    let overhead = serde_json::to_vec(&envelope(id, Ok(Value::String(String::new())))).unwrap().len();
    let exact = terminal(id, Ok(Value::String("a".repeat(MAX_RESULT_BYTES - overhead))));
    assert_eq!(exact["status"], "completed");
    assert_eq!(serde_json::to_vec(exact.as_ref()).unwrap().len(), MAX_RESULT_BYTES);
    let over = terminal(id, Ok(Value::String("a".repeat(MAX_RESULT_BYTES - overhead + 1))));
    assert_eq!(over["status"], "failed");
    // Logical text fits, but escaping would exceed the encoded result owner.
    let escaped = terminal(id, Ok(Value::String("\n".repeat(MAX_RESULT_BYTES / 2))));
    assert_eq!(escaped["status"], "failed");
    assert!(escaped["error"]["message"].as_str().unwrap().contains("may already have committed"));
    assert!(serde_json::to_vec(escaped.as_ref()).unwrap().len() < 1024);
}

#[test]
fn registration_conflicts_cannot_leave_an_admission_route_without_its_results() {
    let mut services = Services::default();
    services.register(RESULT_SERVICE, |_| Ok(Value::Null)).unwrap();
    assert!(register_plugin_worker(&mut services, PluginWorker::spawn(memory, host()).unwrap()).is_err());
    assert!(!services.contains(PLUGIN_SERVICE));
}

mod lifecycle;

#[test]
fn referenced_digests_are_bounded_checked_and_only_for_uninstall_and_prune() {
    let digest = format!("blake3:{}", "0".repeat(64));
    let ok = [
        json!({"op": "prune", "referenced": [digest]}),
        json!({"op": "uninstall", "package_id": "io.example.text", "referenced": [digest]}),
        json!({"op": "prune", "referenced": vec![digest.clone(); crate::plugin_runtime::MAX_REFERENCED]}),
    ];
    for request in ok {
        assert!(bounded_request(&request).is_ok(), "{request}");
    }
    let bad = [
        json!({"op": "prune", "referenced": ["blake3:00"]}),
        json!({"op": "prune", "referenced": digest}),
        json!({"op": "prune", "referenced": vec![digest.clone(); crate::plugin_runtime::MAX_REFERENCED + 1]}),
        json!({"op": "enable", "package_id": "io.example.text", "referenced": [digest]}),
    ];
    for request in bad {
        assert_eq!(bounded_request(&request).unwrap_err().kind, ErrorKind::InvalidInput);
    }
}
