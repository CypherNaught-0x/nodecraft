use super::*;

#[derive(Default)]
struct Counts {
    reads: AtomicUsize,
    writes: AtomicUsize,
    sweeps: AtomicUsize,
}
struct CountedMemory {
    inner: MemoryBackend,
    counts: Arc<Counts>,
    first_read: Option<(SyncSender<()>, Receiver<()>)>,
    dropped: Option<SyncSender<(usize, usize, usize)>>,
}
impl Drop for CountedMemory {
    fn drop(&mut self) {
        if let Some(tx) = &self.dropped {
            let _ = tx.send((self.counts.reads.load(Ordering::SeqCst), self.counts.writes.load(Ordering::SeqCst), self.counts.sweeps.load(Ordering::SeqCst)));
        }
    }
}
impl StoreBackend for CountedMemory {
    fn read_index(&self) -> Result<Option<Vec<u8>>, Error> {
        if self.counts.reads.fetch_add(1, Ordering::SeqCst) == 0
            && let Some((entered, release)) = &self.first_read
        {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        self.inner.read_index()
    }
    fn write_index(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.counts.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write_index(bytes)
    }
    fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, Error> {
        self.inner.read_object(digest)
    }
    fn write_object(&mut self, digest: &PackageDigest, bytes: &[u8]) -> Result<(), Error> {
        self.inner.write_object(digest, bytes)
    }
    fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), Error> {
        self.inner.remove_object(digest)
    }
    fn list_objects(&self) -> Result<Vec<PackageDigest>, Error> {
        self.counts.sweeps.fetch_add(1, Ordering::SeqCst);
        self.inner.list_objects()
    }
}

#[test]
fn abandoned_terminals_are_reclaimed_without_replaying_operations() {
    let counts = Arc::new(Counts::default());
    let mut backend = Some(CountedMemory { inner: Default::default(), counts: Arc::clone(&counts), first_read: None, dropped: None });
    let worker = PluginWorker::spawn(move || Store::open(backend.take().unwrap()), host()).unwrap();
    let mut ids = Vec::new();
    for _ in 0..MAX_PENDING * 3 {
        let id = worker.submit(&json!({"op":"list"})).unwrap();
        assert_eq!(wait(&worker, &id)["status"], "completed");
        ids.push(id);
    }
    assert_eq!(counts.reads.load(Ordering::SeqCst), 1 + ids.len(), "one initial open and one refresh per admitted operation");
    assert_eq!(lock(&worker.state).slots.len(), MAX_PENDING);
    for id in &ids[..ids.len() - MAX_PENDING] {
        assert_eq!(worker.result(id).unwrap_err().kind, ErrorKind::NotFound);
    }
    for id in &ids[ids.len() - MAX_PENDING..] {
        assert_eq!(worker.result(id).unwrap()["status"], "completed");
    }
}

#[test]
fn last_owner_drop_stops_queued_mutations_without_joining_the_running_backend() {
    let counts = Arc::new(Counts::default());
    let (entered, at_read) = sync_channel(1);
    let (release, permit) = sync_channel(1);
    let (dropped, finished) = sync_channel(1);
    let mut backend =
        Some(CountedMemory { inner: Default::default(), counts: Arc::clone(&counts), first_read: Some((entered, permit)), dropped: Some(dropped) });
    let worker = PluginWorker::spawn(move || Store::open(backend.take().unwrap()), host()).unwrap();
    worker.submit(&json!({"op":"list"})).unwrap();
    at_read.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.submit(&json!({"op":"prune"})).unwrap();
    worker.submit(&json!({"op":"uninstall","package_id":"io.example.queued"})).unwrap();
    // Return while the actual backend is still gated; do not use an elapsed-time threshold.
    drop(worker);
    assert!(finished.try_recv().is_err());
    release.send(()).unwrap();
    let (reads, writes, sweeps) = finished.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!((reads, writes, sweeps), (2, 0, 1), "only the already-running open/list may access the backend; queued mutations must not start");
}

#[test]
fn state_contention_reports_updating_not_retained_result_saturation() {
    let worker = PluginWorker::spawn(memory, host()).unwrap();
    let held = lock(&worker.state);
    let error = worker.result("unknown").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.message.contains("updating"));
    assert!(!error.message.contains("full"));
    drop(held);
    assert_eq!(worker.result("unknown").unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn adversarial_worker_admission_never_accepts_invalid_owners_or_enters_the_factory() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let worker = PluginWorker::spawn(
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            memory()
        },
        host(),
    )
    .unwrap();
    let mut deep = Value::Null;
    for _ in 0..128 {
        deep = json!([deep]);
    }
    let mut corpus = vec![Value::Null, json!(true), json!(0), json!(-1), json!(u64::MAX), json!([]), deep, json!("x".repeat(1024 * 1024 + 1))];
    for raw in [Value::Null, json!(true), json!(u64::MAX), json!([]), json!({})] {
        corpus.push(json!({"op":raw}));
        corpus.push(json!({"op":"import","location":raw}));
        corpus.push(json!({"op":"enable","package_id":raw}));
        corpus.push(json!({"op":"rollback","package_id":"io.example.one","digest":raw}));
        if !raw.is_boolean() {
            corpus.push(json!({"op":"update","location":"never-opened","dry_run":raw}));
        }
    }
    corpus.extend([
        json!({"op":"list","extra":[]}),
        json!({"op":"prune","extra":"x"}),
        json!({"op":"import","location":"x".repeat(4097)}),
        json!({"op":"uninstall","package_id":"invalid"}),
    ]);
    for value in corpus {
        assert_eq!(worker.submit(&value).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(lock(&worker.state).slots.is_empty());
    assert_eq!(worker.next.load(Ordering::Relaxed), 1);
}
