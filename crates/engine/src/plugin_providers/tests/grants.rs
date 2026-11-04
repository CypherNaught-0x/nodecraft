//! A grant revoked in another window (#174): two NodeCraft windows share one plugin store on
//! disk, each with its own handle. A revoke in one stops the other's broker before its next paid
//! request, without that window reading the store again, and the other window's worker
//! withdraws the provider at its next read.
#![cfg(not(target_arch = "wasm32"))]
use super::*;
use crate::plugin_cmds::{PluginHost, PluginWorker};
use nodecraft_plugin_store::FolderBackend;
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
    GrantCheck::store(move || FolderBackend::new(root.clone()))
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

#[test]
fn grants_that_cannot_be_read_hold_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = open(dir.path());
    install(&mut first);
    let provider = Arc::clone(PluginSnapshot::of(&first).provider(PROVIDER).unwrap());
    std::fs::write(dir.path().join("grants.json"), b"{ not grants").unwrap();
    let http = Arc::new(Counting::default());
    let broker = broker(PROVIDER, &provider, &keys(), http.clone(), store_check(dir.path())).unwrap();
    assert!(block_on(broker.submit(&job())).unwrap_err().message.contains("withdrawn"));
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
