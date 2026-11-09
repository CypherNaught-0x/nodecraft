//! A grant revoked in another window (#174): two NodeCraft windows share one plugin store on
//! disk, each with its own handle. A revoke in one stops the other's broker before its next paid
//! request, without that window reading the store again, and the other window's worker
//! withdraws the provider at its next read.
#![cfg(not(target_arch = "wasm32"))]
use super::*;
use crate::plugin_cmds::{PluginHost, PluginWorker};
use nodecraft_plugin_store::{FolderBackend, StoreBackend};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn open(root: &Path) -> Store<FolderBackend> {
    Store::open(FolderBackend::new(root.to_path_buf()).unwrap()).unwrap()
}

/// The provider package imported, granted and enabled through `store`; its grant id.
fn install(store: &mut Store<FolderBackend>) -> String {
    let trust = TrustStore::default();
    store.import(&package("1.0.0", &priced("40000")), &trust, false).unwrap();
    let grant = store.grant_rights(&id(), None, &trust, &rights(&[ORIGIN, "https://cdn.acme.example"])).unwrap();
    store.enable(&id(), &trust, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    grant.grant_id
}

fn store_check(root: &Path) -> GrantCheck {
    let root: PathBuf = root.to_path_buf();
    GrantCheck::folder(root)
}

fn job() -> ProviderJob {
    ProviderJob {
        model: "acme/v1".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "a red fox".into() }],
        idempotency_key: "nc-abc123".into(),
    }
}

fn keys() -> MemoryStore {
    let keys = MemoryStore::default();
    keys.set(&Account::new("plugin.io.example.acme:api.acme.example").unwrap(), &Secret::new("plugin-fixture-key-not-real").unwrap()).unwrap();
    keys
}

#[test]
fn a_revoke_in_another_window_stops_this_windows_broker_before_its_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = open(dir.path());
    let grant = install(&mut first);
    // This window published the provider from its own read of the store.
    let snapshot = PluginSnapshot::of(&first);
    let provider = Arc::clone(snapshot.provider(PROVIDER).unwrap());
    let http = Arc::new(Counting::default());
    let broker = broker(PROVIDER, &provider, &keys(), http.clone(), store_check(dir.path())).unwrap();
    let handle = nodecraft_providers::JobHandle { remote_id: "j-1".into(), data: json!({"model": "acme/v1"}) };

    // While the grant stands the broker sends (the fake service is offline).
    assert!(block_on(broker.submit(&job())).unwrap_err().message.contains("offline"));
    assert_eq!(*http.0.lock().unwrap(), 1);

    // Another window, with its own handle on the same store, revokes the grant.
    let mut second = open(dir.path());
    second.revoke(&grant).unwrap();
    assert!(!provider.is_revoked(), "this window hasn't read the store again");
    let error = block_on(broker.submit(&job())).unwrap_err();
    assert!(error.message.contains("withdrawn") && error.message.contains("nothing was sent"), "{}", error.message);
    assert!(block_on(broker.poll(&handle)).unwrap_err().message.contains("withdrawn"), "a job it sent before is not followed either");
    assert_eq!(*http.0.lock().unwrap(), 1, "nothing more was sent");

    // Granting the same digest again makes a new grant at a new epoch: the old stamp stays dead.
    let trust = TrustStore::default();
    second.grant_rights(&id(), None, &trust, &rights(&[ORIGIN, "https://cdn.acme.example"])).unwrap();
    assert!(block_on(broker.submit(&job())).unwrap_err().message.contains("withdrawn"));
    assert_eq!(*http.0.lock().unwrap(), 1);
}

/// Grants that can't be read (a file being replaced, damage, more than a grants file may hold)
/// or a store busy in another window's request are not a revoke. Nothing new is sent, a job
/// already sent is followed again later rather than given up (it may be billed), and the window's
/// own reread of the store publishes nothing that would withdraw the provider.
#[test]
fn grants_that_cannot_be_read_send_nothing_new_but_keep_a_sent_job() {
    let dir = tempfile::tempdir().unwrap();
    install(&mut open(dir.path()));
    let original = std::fs::read(dir.path().join("grants.json")).unwrap();
    // The window: a worker rereading the store every 20 ms, and a session following it as the
    // app's frame does.
    let root = dir.path().to_path_buf();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let worker = PluginWorker::spawn_rereading(move || Store::open(FolderBackend::new(root.clone())?), host, Duration::from_millis(20)).unwrap();
    assert!(worker.refresh());
    let deadline = Instant::now() + Duration::from_secs(10);
    while worker.plugins().1.provider(PROVIDER).is_none() {
        assert!(Instant::now() < deadline, "the provider was never published");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut s = session(PluginSnapshot::default());
    let mut seen = worker.changes();
    s.set_plugins(worker.plugins().1).unwrap();
    let provider = Arc::clone(s.plugins().provider(PROVIDER).unwrap());
    let follow = |s: &mut Session, seen: &mut u64| {
        // Several rereads, each followed as the app's frame follows them.
        let until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < until {
            if worker.changes() != *seen {
                *seen = worker.changes();
                s.set_plugins(worker.plugins().1).unwrap();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let handle = nodecraft_providers::JobHandle { remote_id: "j-1".into(), data: json!({"model": "acme/v1"}) };
    let oversized = vec![b' '; nodecraft_plugin_store::grants::MAX_GRANTS_BYTES + 2];
    let mut retouched = original.clone();
    retouched.push(b'\n');
    for (bytes, busy) in [(b"{ not grants".to_vec(), false), (oversized, false), (retouched, true)] {
        // Busy: another window holds the store's lock while the grants change under it.
        // The worker takes the lock for each reread, so this may wait out one of them.
        let lock = busy.then(|| {
            let backend = FolderBackend::new(dir.path().to_path_buf()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match backend.transaction() {
                    Ok(lock) => break lock,
                    Err(error) if error.kind == nodecraft_types::ErrorKind::Busy && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                    Err(error) => panic!("the store's lock was never free: {error:?}"),
                }
            }
        });
        std::fs::write(dir.path().join("grants.json"), &bytes).unwrap();
        follow(&mut s, &mut seen);
        let now = s.plugins().provider(PROVIDER).expect("the provider is still published");
        assert!(Arc::ptr_eq(now, &provider) && !provider.is_revoked(), "an unreadable or busy store withdraws nothing");
        let http = Arc::new(Counting::default());
        let broker = broker(PROVIDER, &provider, &keys(), http.clone(), store_check(dir.path())).unwrap();
        let poll = block_on(broker.poll(&handle)).unwrap_err();
        assert!(poll.kind.is_transient() && !poll.message.contains("withdrawn"), "a sent job is followed again later: {poll:?}");
        if !busy {
            let refused = block_on(broker.submit(&job())).unwrap_err();
            assert!(refused.message.contains("can't be read") && refused.message.contains("nothing was sent"), "{}", refused.message);
            assert!(refused.kind.is_transient(), "a submit refused this way may be tried again");
            assert_eq!(*http.0.lock().unwrap(), 0);
        }
        drop(lock);
        std::fs::write(dir.path().join("grants.json"), &original).unwrap();
    }
    // Once the store reads again, the same grant still stands.
    follow(&mut s, &mut seen);
    assert!(!provider.is_revoked());
}

#[test]
fn a_package_disabled_in_another_window_stops_this_windows_broker() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = open(dir.path());
    install(&mut first);
    let provider = Arc::clone(PluginSnapshot::of(&first).provider(PROVIDER).unwrap());
    let http = Arc::new(Counting::default());
    let broker = broker(PROVIDER, &provider, &keys(), http.clone(), store_check(dir.path())).unwrap();
    // A disable leaves the grant live; only the index changes.
    open(dir.path()).disable(&id()).unwrap();
    let error = block_on(broker.submit(&job())).unwrap_err();
    assert!(error.message.contains("withdrawn"), "{}", error.message);
    assert_eq!(*http.0.lock().unwrap(), 0);
}

#[test]
fn a_windows_worker_reads_the_store_again_and_withdraws_a_provider_revoked_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let grant = install(&mut open(dir.path()));
    let root = dir.path().to_path_buf();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let worker = PluginWorker::spawn_rereading(move || Store::open(FolderBackend::new(root.clone())?), host, Duration::from_millis(20)).unwrap();
    assert!(worker.refresh());
    let published = |worker: &PluginWorker, want: bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.plugins().1.provider(PROVIDER).is_some() != want {
            assert!(Instant::now() < deadline, "the worker never published the change");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    published(&worker, true);
    let changes = worker.changes();

    // Unchanged reads publish nothing.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(worker.changes(), changes);

    open(dir.path()).revoke(&grant).unwrap();
    published(&worker, false);
}

/// A store whose index another window rewrites (disabling the package) at the `flip`th index
/// read, wherever in the worker's timed read that falls.
struct Flipping {
    enabled: Vec<u8>,
    disabled: Vec<u8>,
    grants: Option<Vec<u8>>,
    objects: std::collections::BTreeMap<nodecraft_plugin::PackageDigest, Vec<u8>>,
    reads: std::sync::atomic::AtomicUsize,
    flip: usize,
}

impl nodecraft_plugin_store::StoreBackend for Flipping {
    fn read_index(&self) -> Result<Option<Vec<u8>>, nodecraft_types::Error> {
        let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(if n + 1 >= self.flip { self.disabled.clone() } else { self.enabled.clone() }))
    }
    fn write_index(&mut self, _: &[u8]) -> Result<(), nodecraft_types::Error> {
        Ok(())
    }
    fn read_object(&self, digest: &nodecraft_plugin::PackageDigest) -> Result<Vec<u8>, nodecraft_types::Error> {
        self.objects.get(digest).cloned().ok_or_else(|| nodecraft_types::Error::new(nodecraft_types::ErrorKind::NotFound, "missing"))
    }
    fn write_object(&mut self, _: &nodecraft_plugin::PackageDigest, _: &[u8]) -> Result<(), nodecraft_types::Error> {
        Ok(())
    }
    fn remove_object(&mut self, _: &nodecraft_plugin::PackageDigest) -> Result<(), nodecraft_types::Error> {
        Ok(())
    }
    fn list_objects(&self) -> Result<Vec<nodecraft_plugin::PackageDigest>, nodecraft_types::Error> {
        Ok(self.objects.keys().copied().collect())
    }
    fn read_grants(&self) -> Result<Option<Vec<u8>>, nodecraft_types::Error> {
        Ok(self.grants.clone())
    }
}

/// Whichever read the other window's write lands between, the worker's timed reads end up
/// publishing the disabled package: what it last saw is never newer than what it published.
#[test]
fn a_disable_landing_mid_read_is_always_published() {
    let mut store = granted(&priced("40000"), &[ORIGIN, "https://cdn.acme.example"]);
    let enabled = store.backend().index.clone().unwrap();
    store.disable(&id()).unwrap();
    let disabled = store.backend().index.clone().unwrap();
    let (grants, objects) = (store.backend().grants.clone(), store.backend().objects.clone());
    for flip in 1..=10 {
        let backend = Flipping {
            enabled: enabled.clone(),
            disabled: disabled.clone(),
            grants: grants.clone(),
            objects: objects.clone(),
            reads: Default::default(),
            flip,
        };
        let mut backend = Some(backend);
        let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
        let factory =
            move || backend.take().map(Store::open).unwrap_or_else(|| Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "opened twice")));
        let worker = PluginWorker::spawn_rereading(factory, host, Duration::from_millis(5)).unwrap();
        assert!(worker.refresh());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(worker.loaded() && worker.plugins().1.provider(PROVIDER).is_none()) {
            assert!(Instant::now() < deadline, "flip at read {flip}: the disable was never published");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
