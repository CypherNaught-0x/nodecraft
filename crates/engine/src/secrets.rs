//! Provider API keys in the operating system's keychain.
//!
//! Keys live only in the keychain (macOS Keychain, Windows Credential Manager, the Secret Service
//! on Linux), under the service [`SERVICE`] with the provider's [`Account`] name. They never go
//! into project files, settings, logs or error messages. The value types and the store interface
//! live in `providers::secrets`, so adapters receive a [`Secret`]; this module adds the OS
//! keychain and the `keys` host service behind the `keys.*` commands.
//!
//! The browser build has no keychain; it will keep keys per origin with a notice (M10). Until then
//! hosts use a [`MemoryStore`] there (keys last for the session).

use std::sync::{Arc, Mutex, PoisonError};

pub use nodecraft_providers::secrets::{Account, MAX_SECRET_BYTES, MemoryStore, Secret, SecretError, SecretStore};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::Services;

/// The keychain service name: the app id.
pub const SERVICE: &str = "io.github.cyphernaught0x.nodecraft";
/// The host-service capability the `keys.*` commands use.
pub const KEY_SERVICE: &str = "keys.store";

#[cfg(not(target_arch = "wasm32"))]
pub use keychain::KeyringStore;

#[cfg(not(target_arch = "wasm32"))]
mod keychain {
    use super::*;

    /// The operating system keychain, through the `keyring` crate.
    #[derive(Debug, Default)]
    pub struct KeyringStore;

    pub(super) fn map(e: keyring::Error) -> SecretError {
        use keyring::Error as E;
        match e {
            E::NoDefaultStore | E::NotSupportedByStore(_) => {
                // Keep the reason the keychain couldn't be opened; it is decided once per run.
                let reason = match keyring::Entry::store_status() {
                    Err(e) => e.to_string().chars().take(200).collect::<String>(),
                    Ok(()) => "not supported".into(),
                };
                let hint = if cfg!(target_os = "linux") {
                    "install or unlock a keyring (GNOME Keyring, KWallet), then restart NodeCraft"
                } else {
                    "restart NodeCraft"
                };
                SecretError::Unavailable(format!("{reason}; {hint}"))
            }
            E::NoStorageAccess(_) => SecretError::Denied("unlock the keychain and allow NodeCraft access".into()),
            // These carry the stored bytes: never echo them.
            E::BadEncoding(_) | E::BadDataFormat(..) => SecretError::Failed("the stored key can't be read; set it again".into()),
            E::Ambiguous(_) => SecretError::Failed("more than one key is stored under this name; remove and set it again".into()),
            E::TooLong(_, limit) => SecretError::Invalid(format!("the key is too long for this system's keychain (at most {limit})")),
            E::Invalid(..) => SecretError::Invalid("the keychain refused the key".into()),
            // Platform messages describe the failure, never the secret.
            other => SecretError::Failed(other.to_string().chars().take(300).collect()),
        }
    }

    fn entry(account: &Account) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, account.as_str()).map_err(map)
    }

    impl SecretStore for KeyringStore {
        fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
            match entry(account)?.get_password() {
                // A value written by another tool or an older version that can't be sent: say so,
                // rather than pretending no key is set (which would surface later as a 401).
                Ok(v) => Secret::new(&v).map(Some).map_err(|_| SecretError::Failed("the stored key can't be used; set it again".into())),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(map(e)),
            }
        }
        fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError> {
            entry(account)?.set_password(secret.expose()).map_err(map)
        }
        fn delete(&self, account: &Account) -> Result<bool, SecretError> {
            match entry(account)?.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(e) => Err(map(e)),
            }
        }
    }
}

fn to_error(e: SecretError) -> Error {
    let kind = match e {
        SecretError::Invalid(_) => ErrorKind::InvalidInput,
        SecretError::Unavailable(_) => ErrorKind::Unsupported,
        // "Unlock your keychain" is something the user can fix, unlike an internal failure.
        SecretError::Denied(_) => ErrorKind::Authentication,
        _ => ErrorKind::Internal,
    };
    Error::new(kind, e.to_string())
}

/// A request to the `keys` host service (built by the `keys.*` commands).
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum KeyRequest {
    Set { account: String, key: String },
    Remove { account: String },
    Status { account: String },
}

/// A key request, checked: what to do to which account.
enum KeyOp {
    Set(Secret),
    Remove,
    Status,
}

/// Parses and checks a request without touching the keychain, so bad input fails at once.
fn parse(request: &Value) -> Result<(Account, KeyOp), Error> {
    let request: KeyRequest = serde_json::from_value(request.clone()).map_err(|_| Error::new(ErrorKind::InvalidInput, "malformed key request"))?;
    Ok(match request {
        KeyRequest::Set { account, key } => (Account::new(&account).map_err(to_error)?, KeyOp::Set(Secret::new(&key).map_err(to_error)?)),
        KeyRequest::Remove { account } => (Account::new(&account).map_err(to_error)?, KeyOp::Remove),
        KeyRequest::Status { account } => (Account::new(&account).map_err(to_error)?, KeyOp::Status),
    })
}

/// Runs a checked request against `store` (this is where a keychain prompt can block).
fn execute(store: &dyn SecretStore, account: &Account, op: &KeyOp) -> Result<Value, Error> {
    match op {
        KeyOp::Set(secret) => {
            store.set(account, secret).map_err(to_error)?;
            Ok(json!({ "stored": true, "masked": secret.masked() }))
        }
        KeyOp::Remove => Ok(json!({ "removed": store.delete(account).map_err(to_error)? })),
        KeyOp::Status => {
            let found = store.get(account).map_err(to_error)?;
            Ok(json!({ "stored": found.is_some(), "masked": found.map(|s| s.masked()) }))
        }
    }
}

/// Registers the `keys` host service on `services`, answering synchronously from `store`. For
/// headless hosts (CLI, MCP) where a keychain prompt may block the call; the app uses
/// [`register_key_worker`] so the window never freezes.
pub fn register_key_service(services: &mut Services, store: Arc<dyn SecretStore>) -> Result<(), Error> {
    let store = Mutex::new(store);
    services.register(KEY_SERVICE, move |request: &Value| {
        let (account, op) = parse(request)?;
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        execute(store.as_ref(), &account, &op)
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub use worker::{KeyOutcome, KeyResults, KeyWorker, register_key_worker};

#[cfg(not(target_arch = "wasm32"))]
mod worker {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

    /// Requests waiting for the keychain; more are refused as busy.
    const QUEUE: usize = 64;

    struct Job {
        id: u64,
        account: Account,
        op: KeyOp,
    }

    /// The result of a request handed to the [`KeyWorker`].
    #[derive(Debug, Clone, PartialEq)]
    pub struct KeyOutcome {
        /// The `pending` id the service returned.
        pub id: u64,
        pub account: String,
        /// Same shape as the synchronous service's result; errors never contain the key.
        pub result: Result<Value, Error>,
    }

    /// Runs keychain calls on their own thread, so an unlock or access prompt never blocks the
    /// thread that dispatches commands (the UI). The submitting side; results go to the one
    /// [`KeyResults`] returned with it.
    pub struct KeyWorker {
        jobs: SyncSender<Job>,
        next: AtomicU64,
    }

    /// Where a [`KeyWorker`]'s results arrive. There is exactly one, held by the host, which routes
    /// each outcome by its `pending` id to whoever asked (the keys panel, the in-app agent). Both
    /// queues are bounded: a host that stops reading makes new requests fail as busy.
    pub struct KeyResults(Receiver<KeyOutcome>);

    impl KeyResults {
        /// At most one result-queue's worth of outcomes may be returned by one drain.
        pub const MAX_BATCH: usize = QUEUE;

        /// Up to [`Self::MAX_BATCH`] available results, in request order (never blocks).
        /// Concurrently arriving results can join this batch, but cannot extend it past the
        /// bound. Remaining outcomes stay queued for later calls; none are skipped or dropped.
        pub fn drain(&self) -> Vec<KeyOutcome> {
            let outcomes = self.0.try_iter().take(Self::MAX_BATCH);
            #[cfg(test)]
            let outcomes = outcomes.inspect(|outcome| tests::after_receive(outcome.id));
            outcomes.collect()
        }
    }

    impl KeyWorker {
        /// Starts the worker thread over `store`. Once both halves are dropped, queued requests
        /// are discarded; a keychain call already running (for example waiting on a prompt)
        /// still finishes first, and its result is dropped.
        pub fn spawn(store: Arc<dyn SecretStore>) -> Result<(Arc<Self>, KeyResults), Error> {
            let (jobs, inbox) = sync_channel::<Job>(QUEUE);
            let (outbox, done) = sync_channel::<KeyOutcome>(QUEUE);
            std::thread::Builder::new()
                .name("nodecraft-keychain".into())
                .spawn(move || {
                    for job in inbox {
                        // A keychain backend that panics fails this request, not the worker.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(store.as_ref(), &job.account, &job.op)))
                            .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "the keychain call failed unexpectedly")));
                        if outbox.send(KeyOutcome { id: job.id, account: job.account.as_str().to_owned(), result }).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the keychain worker: {e}")))?;
            Ok((Arc::new(Self { jobs, next: AtomicU64::new(1) }), KeyResults(done)))
        }

        fn submit(&self, account: Account, op: KeyOp) -> Result<u64, Error> {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            match self.jobs.try_send(Job { id, account, op }) {
                Ok(()) => Ok(id),
                Err(TrySendError::Full(_)) => Err(Error::new(ErrorKind::Busy, "the keychain is still answering earlier requests; try again")),
                Err(TrySendError::Disconnected(_)) => Err(Error::new(ErrorKind::Internal, "the keychain worker has stopped")),
            }
        }
    }

    /// Registers the `keys` host service so that it checks the request, queues it on `worker`
    /// and returns `{"pending": id}` at once; the result arrives through [`KeyResults::drain`]
    /// with the same id. So in the app, `keys.*` commands answer `{"pending": id}`; headless hosts
    /// (CLI, MCP) use [`register_key_service`] and answer directly.
    pub fn register_key_worker(services: &mut Services, worker: Arc<KeyWorker>) -> Result<(), Error> {
        services.register(KEY_SERVICE, move |request: &Value| {
            let (account, op) = parse(request)?;
            let id = worker.submit(account, op)?;
            Ok(json!({ "pending": id }))
        })
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            cell::RefCell,
            sync::atomic::AtomicUsize,
            sync::mpsc::{Sender, channel},
            time::Duration,
        };

        // A thread-local scheduling probe observes the real drain iterator. It has no runtime
        // counterpart and never receives secrets. The fake store controls all worker progress.
        type ReceiveProbe = Box<dyn Fn(u64)>;
        thread_local! {
            static AFTER_RECEIVE: RefCell<Option<ReceiveProbe>> = RefCell::new(None);
        }
        pub(super) fn after_receive(id: u64) {
            AFTER_RECEIVE.with(|probe| {
                if let Some(probe) = probe.borrow().as_ref() {
                    probe(id);
                }
            });
        }
        struct ClearProbe;
        impl Drop for ClearProbe {
            fn drop(&mut self) {
                AFTER_RECEIVE.with(|probe| *probe.borrow_mut() = None);
            }
        }
        struct GatedStore {
            entered: Sender<usize>,
            permit: Mutex<Receiver<()>>,
            calls: AtomicUsize,
        }
        impl SecretStore for GatedStore {
            fn get(&self, _: &Account) -> Result<Option<Secret>, SecretError> {
                let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
                self.entered.send(call).map_err(|_| SecretError::Failed("fixture closed".into()))?;
                self.permit.lock().unwrap().recv_timeout(Duration::from_secs(10)).map_err(|_| SecretError::Failed("fixture did not release call".into()))?;
                Ok(None)
            }
            fn set(&self, _: &Account, _: &Secret) -> Result<(), SecretError> {
                panic!("unexpected write in status-only fixture")
            }
            fn delete(&self, _: &Account) -> Result<bool, SecretError> {
                panic!("unexpected delete in status-only fixture")
            }
        }
        #[test]
        fn drain_stays_bounded_during_synchronized_refill_and_delivers_every_result_once() {
            let (entered, calls) = channel();
            let (permit, permits) = channel();
            let (worker, results) = KeyWorker::spawn(Arc::new(GatedStore { entered, permit: Mutex::new(permits), calls: AtomicUsize::new(0) })).unwrap();
            let account = Account::new("openai").unwrap();
            assert_eq!(worker.submit(account.clone(), KeyOp::Status).unwrap(), 1);
            assert_eq!(calls.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
            // The first call is parked, so exactly QUEUE further requests fit the inbox.
            for expected in 2..=QUEUE as u64 + 1 {
                assert_eq!(worker.submit(account.clone(), KeyOp::Status).unwrap(), expected);
            }
            for call in 1..=QUEUE {
                if call != 1 {
                    assert_eq!(calls.recv_timeout(Duration::from_secs(10)).unwrap(), call);
                }
                permit.send(()).unwrap();
            }
            // Entering call65 proves64 outcomes were sent into the bounded result queue.
            assert_eq!(calls.recv_timeout(Duration::from_secs(10)).unwrap(), QUEUE + 1);
            assert_eq!(worker.submit(account, KeyOp::Status).unwrap(), QUEUE as u64 + 2);
            let refill = permit.clone();
            AFTER_RECEIVE.with(|probe| {
                *probe.borrow_mut() = Some(Box::new(move |id| {
                    if id == 1 {
                        refill.send(()).unwrap();
                        // Call66 cannot start before the worker sends result65. Thus result65
                        // is queued before the draining iterator resumes after its first receive.
                        assert_eq!(calls.recv_timeout(Duration::from_secs(10)).unwrap(), QUEUE + 2);
                    }
                }))
            });
            let reset = ClearProbe;
            let mut delivered = results.drain();
            drop(reset);
            assert_eq!(delivered.len(), KeyResults::MAX_BATCH, "refill must not extend one drain beyond its fixed batch");
            assert_eq!(delivered.iter().map(|outcome| outcome.id).collect::<Vec<_>>(), (1..=QUEUE as u64).collect::<Vec<_>>());
            // The result admitted during that drain is retained for the next call.
            let next = results.drain();
            assert_eq!(next.iter().map(|outcome| outcome.id).collect::<Vec<_>>(), [QUEUE as u64 + 1]);
            delivered.extend(next);
            assert!(results.drain().is_empty(), "the last fake call is still parked: empty drain doesn't wait");
            drop(worker); // Closing submission must still deliver the last running call.
            permit.send(()).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while delivered.len() < QUEUE + 2 {
                let batch = results.drain();
                assert!(batch.len() <= KeyResults::MAX_BATCH);
                delivered.extend(batch);
                assert!(std::time::Instant::now() < deadline, "the worker didn't finish its final result");
                std::thread::yield_now();
            }
            assert_eq!(
                delivered.iter().map(|outcome| outcome.id).collect::<Vec<_>>(),
                (1..=QUEUE as u64 + 2).collect::<Vec<_>>(),
                "ordered exactly-once delivery across batches"
            );
            assert!(delivered.iter().all(|outcome| outcome.account == "openai" && outcome.result == Ok(json!({"stored":false,"masked":null}))));
            // Waiting for disconnect is test-only; drain remains nonblocking on a closed queue.
            assert!(matches!(results.0.recv_timeout(Duration::from_secs(10)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected)));
            assert!(results.drain().is_empty());
        }
    }
}

/// Checks a chat endpoint's key with a request that costs nothing (listing models). Some servers
/// list models without any key (Ollama, LM Studio, OpenRouter), so success there only says the
/// server is reachable; a rejected key is reported as `ErrorKind::Auth`.
pub async fn test_llm_key(client: &nodecraft_providers::llm::LlmClient) -> Result<usize, nodecraft_providers::ProviderError> {
    client.list_models().await.map(|models| models.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_service_round_trip_without_echoing_keys() {
        let mut services = Services::default();
        register_key_service(&mut services, Arc::new(MemoryStore::default())).unwrap();
        let set = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-proj-abcdefghij1234"})).unwrap();
        assert_eq!(set, json!({"stored": true, "masked": "…1234"}));
        assert!(!set.to_string().contains("abcdefghij"));
        let status = services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})).unwrap();
        assert_eq!(status, json!({"stored": true, "masked": "…1234"}));
        let bad = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-secret with space"})).unwrap_err();
        assert!(!bad.to_string().contains("sk-secret"), "{bad}");
        assert_eq!(services.invoke(KEY_SERVICE, &json!({"op": "remove", "account": "openai"})).unwrap(), json!({"removed": true}));
        assert_eq!(services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})).unwrap(), json!({"stored": false, "masked": null}));
        assert!(services.invoke(KEY_SERVICE, &json!({"op": "drop_all"})).is_err());
    }

    /// A keychain that takes a while to answer (an unlock prompt).
    #[cfg(not(target_arch = "wasm32"))]
    struct Slow(MemoryStore, std::time::Duration);

    #[cfg(not(target_arch = "wasm32"))]
    impl SecretStore for Slow {
        fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
            std::thread::sleep(self.1);
            self.0.get(account)
        }
        fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError> {
            std::thread::sleep(self.1);
            self.0.set(account, secret)
        }
        fn delete(&self, account: &Account) -> Result<bool, SecretError> {
            std::thread::sleep(self.1);
            self.0.delete(account)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn wait(results: &KeyResults, id: u64) -> KeyOutcome {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut arrived = results.drain();
            assert!(arrived.iter().all(|o| o.id <= id), "results come in request order");
            if let Some(done) = arrived.iter().position(|o| o.id == id).map(|i| arrived.swap_remove(i)) {
                return done;
            }
            assert!(std::time::Instant::now() < deadline, "no result for {id}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_worker_answers_later_without_blocking_the_caller() {
        let (worker, results) = KeyWorker::spawn(Arc::new(Slow(MemoryStore::default(), std::time::Duration::from_millis(300)))).unwrap();
        let mut services = Services::default();
        register_key_worker(&mut services, Arc::clone(&worker)).unwrap();
        let pending = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-proj-abcdefghij1234"})).unwrap();
        assert!(results.drain().is_empty(), "the call returned before the keychain answered");
        let id = pending["pending"].as_u64().unwrap();
        let done = wait(&results, id);
        assert_eq!(done.result, Ok(json!({"stored": true, "masked": "…1234"})));
        assert_eq!(done.account, "openai");
        let status = services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})).unwrap()["pending"].as_u64().unwrap();
        assert_eq!(wait(&results, status).result, Ok(json!({"stored": true, "masked": "…1234"})));
        // Bad input fails at once, without queueing, and never echoes the key.
        let bad = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-secret with space"})).unwrap_err();
        assert!(!bad.to_string().contains("sk-secret"));
        assert!(services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "Not Valid", "key": "x"})).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_full_queue_is_busy_and_a_panicking_keychain_fails_one_request() {
        struct Panics;
        impl SecretStore for Panics {
            fn get(&self, _: &Account) -> Result<Option<Secret>, SecretError> {
                panic!("backend bug")
            }
            fn set(&self, _: &Account, _: &Secret) -> Result<(), SecretError> {
                std::thread::sleep(std::time::Duration::from_millis(200));
                Ok(())
            }
            fn delete(&self, _: &Account) -> Result<bool, SecretError> {
                Ok(true)
            }
        }
        let (worker, results) = KeyWorker::spawn(Arc::new(Panics)).unwrap();
        let mut services = Services::default();
        register_key_worker(&mut services, Arc::clone(&worker)).unwrap();
        let first = services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})).unwrap()["pending"].as_u64().unwrap();
        assert_eq!(wait(&results, first).result.unwrap_err().kind, ErrorKind::Internal);
        // The worker survived the panic.
        let next = services.invoke(KEY_SERVICE, &json!({"op": "remove", "account": "openai"})).unwrap()["pending"].as_u64().unwrap();
        assert_eq!(wait(&results, next).result, Ok(json!({"removed": true})));
        let mut busy = false;
        for _ in 0..200 {
            if let Err(e) = services.invoke(KEY_SERVICE, &json!({"op": "set", "account": "openai", "key": "sk-1234567890"})) {
                assert_eq!(e.kind, ErrorKind::Busy);
                busy = true;
                break;
            }
        }
        assert!(busy, "a slow keychain eventually refuses more work instead of queueing without bound");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unread_results_make_requests_busy_and_dropping_the_worker_stops_it() {
        let (worker, results) = KeyWorker::spawn(Arc::new(MemoryStore::default())).unwrap();
        let mut services = Services::default();
        register_key_worker(&mut services, Arc::clone(&worker)).unwrap();
        // Nobody reads the results: both bounded queues fill, then requests are refused.
        let mut refused = false;
        for _ in 0..1000 {
            match services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => {
                    assert_eq!(e.kind, ErrorKind::Busy);
                    refused = true;
                    break;
                }
            }
        }
        assert!(refused, "results can't pile up without bound");
        assert!(!results.drain().is_empty());
        // Without its results reader the thread stops, and new requests say so.
        drop(results);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match services.invoke(KEY_SERVICE, &json!({"op": "status", "account": "openai"})) {
                Err(e) if e.kind == ErrorKind::Internal => break,
                _ => {
                    assert!(std::time::Instant::now() < deadline, "the worker thread never stopped");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        drop(services);
        drop(worker);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn keychain_errors_never_echo_stored_bytes() {
        use keyring::Error as E;
        let secret = b"sk-live-secret-bytes".to_vec();
        let platform = || -> Box<dyn std::error::Error + Send + Sync> { Box::new(std::io::Error::other("platform said no")) };
        for e in [
            E::BadEncoding(secret.clone()),
            E::BadDataFormat(secret.clone(), platform()),
            E::TooLong("password".into(), 2560),
            E::Invalid("p".into(), "r".into()),
            E::PlatformFailure(platform()),
        ] {
            let msg = keychain::map(e).to_string();
            assert!(!msg.contains("sk-live"), "{msg}");
        }
        assert!(matches!(keychain::map(E::NoStorageAccess(platform())), SecretError::Denied(_)));
        assert!(keychain::map(E::TooLong("password".into(), 2560)).to_string().contains("2560"));
    }

    /// Opt-in: touches the real keychain (`NODECRAFT_LIVE_KEYCHAIN=1`); never in CI.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn live_keychain_round_trip() {
        if std::env::var("NODECRAFT_LIVE_KEYCHAIN").ok().as_deref() != Some("1") {
            return;
        }
        let store = KeyringStore;
        let acc = Account::new("nodecraft-live-test").unwrap();
        store.set(&acc, &Secret::new("test-value-123456").unwrap()).unwrap();
        assert_eq!(store.get(&acc).unwrap().map(|s| s.expose().to_owned()).as_deref(), Some("test-value-123456"));
        assert!(store.delete(&acc).unwrap());
        assert!(store.get(&acc).unwrap().is_none());
    }
}
