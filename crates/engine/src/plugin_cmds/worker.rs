//! Native asynchronous package service. Terminal results stay until acknowledged or displaced
//! by a later admission at capacity; unfinished operations are never displaced.
use super::*;
use std::sync::{
    Arc, Mutex, MutexGuard, TryLockError,
    atomic::{AtomicU64, Ordering},
    mpsc::{SyncSender, TrySendError, sync_channel},
};

/// Total retained operations, including unread completions, per registered host.
pub const MAX_PENDING: usize = 16;
/// Encoded terminal envelope, in addition to the existing command JSON shape/text bounds.
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

struct Job {
    id: String,
    request: Value,
}
enum Status {
    Queued,
    Running,
    Done(Arc<Value>),
}
struct Slot {
    id: String,
    sequence: u64,
    status: Status,
}
struct State {
    slots: Vec<Slot>,
    stopped: bool,
}

fn stopped() -> Error {
    Error::new(ErrorKind::Internal, "the plugin worker stopped; inspect the store before retrying an operation")
}
fn capacity() -> Error {
    Error::new(ErrorKind::Busy, "the plugin work queue is full of unfinished requests; try again after one finishes")
}
fn updating() -> Error {
    Error::new(ErrorKind::Busy, "the plugin worker is updating; try again")
}
fn missing() -> Error {
    Error::new(ErrorKind::NotFound, "that plugin request isn't retained by this host")
}

fn envelope(id: &str, result: Result<Value, Error>) -> Value {
    let (status, field, value) = match result {
        Ok(value) => ("completed", "result", value),
        Err(error) => ("failed", "error", json!({"kind":error.kind,"message":error.message})),
    };
    Value::Object(serde_json::Map::from_iter([
        ("pending".into(), Value::String(id.to_owned())),
        ("status".into(), Value::String(status.into())),
        (field.into(), value),
    ]))
}

/// Counts the real escaped JSON without allocating a serialized copy.
struct Count(usize);
impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let total = self.0.checked_add(bytes.len()).filter(|n| *n <= MAX_RESULT_BYTES).ok_or_else(|| std::io::Error::other("plugin result limit"))?;
        self.0 = total;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn terminal(id: &str, result: Result<Value, Error>) -> Arc<Value> {
    let value = envelope(id, result);
    if crate::commands::validate_json(&value).is_err() || serde_json::to_writer(Count(0), &value).is_err() {
        return Arc::new(envelope(
            id,
            Err(Error::new(ErrorKind::Internal, "the plugin result exceeded host limits; the operation may already have committed")),
        ));
    }
    Arc::new(value)
}
impl State {
    fn stop(&mut self) {
        self.stopped = true;
        for slot in &mut self.slots {
            let detail = match slot.status {
                Status::Queued => "the plugin worker stopped before this request started",
                Status::Running => "the plugin operation failed unexpectedly; the store may already have changed",
                Status::Done(_) => continue,
            };
            slot.status = Status::Done(terminal(&slot.id, Err(Error::new(ErrorKind::Internal, detail))));
        }
    }
}
fn recover<'a>(shared: &'a Mutex<State>, error: std::sync::PoisonError<MutexGuard<'a, State>>) -> MutexGuard<'a, State> {
    // Critical sections only move valid Slot enums; no backend or user callback runs inside.
    // Keep completed owners and conservatively fail anything whose publication was interrupted.
    let mut state = error.into_inner();
    state.stop();
    shared.clear_poison();
    state
}
fn lock(shared: &Mutex<State>) -> MutexGuard<'_, State> {
    match shared.lock() {
        Ok(state) => state,
        Err(error) => recover(shared, error),
    }
}
fn try_lock(shared: &Mutex<State>) -> Result<MutexGuard<'_, State>, Error> {
    match shared.try_lock() {
        Ok(state) => Ok(state),
        Err(TryLockError::WouldBlock) => Err(updating()),
        Err(TryLockError::Poisoned(error)) => Ok(recover(shared, error)),
    }
}

/// Submitting/querying never performs package I/O. The service owns one worker and a finite
/// retained ledger. Dropping the host abandons its results, without joining an OS I/O call.
pub struct PluginWorker {
    jobs: SyncSender<Job>,
    state: Arc<Mutex<State>>,
    prefix: String,
    next: AtomicU64,
}
impl Drop for PluginWorker {
    fn drop(&mut self) {
        // The last host owner closes admission. This mutex protects only bounded state moves,
        // never a backend callback or OS I/O. Do not join an operation already in progress.
        lock(&self.state).stopped = true;
    }
}
impl PluginWorker {
    /// `factory`, including native directory/index initialization, runs on the first request.
    /// A normal initialization error can be retried by a later explicit request. A backend panic
    /// stops this worker: no accepted operation is automatically replayed.
    pub fn spawn<B, F>(mut factory: F, host: PluginHost) -> Result<Arc<Self>, Error>
    where
        B: StoreBackend + Send + 'static,
        F: FnMut() -> Result<Store<B>, Error> + Send + 'static,
    {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| Error::new(ErrorKind::Internal, "plugin request identity entropy is unavailable"))?;
        let prefix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let mut slots = Vec::new();
        slots.try_reserve_exact(MAX_PENDING).map_err(|_| Error::new(ErrorKind::Internal, "plugin result slots can't be reserved"))?;
        let state = Arc::new(Mutex::new(State { slots, stopped: false }));
        let (jobs, inbox) = sync_channel::<Job>(MAX_PENDING);
        let shared = Arc::clone(&state);
        std::thread::Builder::new()
            .name("nodecraft-plugins".into())
            .spawn(move || {
                let mut store = None;
                for job in inbox {
                    {
                        let mut state = lock(&shared);
                        if state.stopped {
                            break;
                        }
                        let Some(slot) = state.slots.iter_mut().find(|slot| slot.id == job.id) else {
                            state.stop();
                            break;
                        };
                        slot.status = Status::Running;
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if store.is_none() {
                            store = Some(factory()?);
                        }
                        let store = store.as_mut().ok_or_else(stopped)?;
                        handle(store, &host, &job.request)
                    }));
                    match result {
                        Ok(result) => {
                            let outcome = terminal(&job.id, result);
                            let mut state = lock(&shared);
                            if state.stopped {
                                break;
                            }
                            if let Some(slot) = state.slots.iter_mut().find(|slot| slot.id == job.id) {
                                slot.status = Status::Done(outcome);
                            } else {
                                state.stop();
                                break;
                            }
                        }
                        Err(_) => {
                            lock(&shared).stop();
                            break;
                        }
                    }
                }
            })
            .map_err(|_| Error::new(ErrorKind::Unsupported, "the plugin worker can't be started"))?;
        Ok(Arc::new(Self { jobs, state, prefix, next: AtomicU64::new(1) }))
    }

    /// Accepts one bounded, owned request, or refuses without accepting a package operation.
    pub fn submit(&self, request: &Value) -> Result<String, Error> {
        let request = bounded_request(request)?;
        let mut state = try_lock(&self.state)?;
        if state.stopped {
            return Err(stopped());
        }
        // Sequence order survives forget's swap_remove; queued/running slots are never victims.
        let replacement = if state.slots.len() >= MAX_PENDING {
            Some(state.slots.iter_mut().filter(|slot| matches!(slot.status, Status::Done(_))).min_by_key(|slot| slot.sequence).ok_or_else(capacity)?)
        } else {
            None
        };
        let number = self
            .next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::new(ErrorKind::Busy, "plugin request IDs are exhausted; restart the host"))?;
        let id = format!("{}-{number}", self.prefix);
        let slot = Slot { id: id.clone(), sequence: number, status: Status::Queued };
        // The worker must acquire this same mutex before starting the received job. Publish
        // its slot before unlocking, and keep an old terminal result intact if enqueue fails.
        match self.jobs.try_send(Job { id: id.clone(), request }) {
            Ok(()) => {
                if let Some(previous) = replacement {
                    *previous = slot;
                } else {
                    state.slots.push(slot);
                }
                Ok(id)
            }
            Err(error) => match error {
                TrySendError::Full(_) => Err(capacity()),
                TrySendError::Disconnected(_) => {
                    state.stop();
                    Err(stopped())
                }
            },
        }
    }
    /// Non-consuming. A terminal envelope stays identical until acknowledgement or eviction
    /// as the oldest terminal at capacity. An evicted ID returns NotFound.
    pub fn result(&self, id: &str) -> Result<Value, Error> {
        check_id(id)?;
        let state = try_lock(&self.state)?;
        let slot = state.slots.iter().find(|slot| slot.id == id).ok_or_else(missing)?;
        Ok(match &slot.status {
            Status::Queued => json!({"pending":id,"status":"queued"}),
            Status::Running => json!({"pending":id,"status":"running"}),
            Status::Done(result) => (**result).clone(),
        })
    }
    /// Explicit acknowledgement: never cancels an operation or discards an unfinished result.
    pub fn forget(&self, id: &str) -> Result<(), Error> {
        check_id(id)?;
        let mut state = try_lock(&self.state)?;
        let index = state.slots.iter().position(|slot| slot.id == id).ok_or_else(missing)?;
        if !state.slots.get(index).is_some_and(|slot| matches!(slot.status, Status::Done(_))) {
            return Err(Error::new(ErrorKind::Conflict, "that plugin request hasn't finished"));
        }
        state.slots.swap_remove(index);
        Ok(())
    }
}

fn check_id(id: &str) -> Result<(), Error> {
    if id.is_empty() || id.len() > MAX_PENDING_ID { Err(Error::new(ErrorKind::InvalidInput, "invalid plugin pending ID")) } else { Ok(()) }
}
fn bounded_request(request: &Value) -> Result<Value, Error> {
    let invalid = || Error::new(ErrorKind::InvalidInput, "invalid plugin worker request");
    let object = request.as_object().ok_or_else(invalid)?;
    let op = object.get("op").and_then(Value::as_str).ok_or_else(invalid)?;
    let fields: &[&str] = match op {
        "list" | "prune" => &["op"],
        "import" | "update" => {
            let location = object.get("location").and_then(Value::as_str).ok_or_else(invalid)?;
            if location.is_empty() || location.len() > 4096 || object.get("dry_run").is_some_and(|v| !v.is_boolean()) {
                return Err(invalid());
            }
            &["op", "location", "dry_run"]
        }
        "revoke" => {
            let grant = object.get("grant_id").and_then(Value::as_str).ok_or_else(invalid)?;
            if !super::valid_grant_id(grant) {
                return Err(invalid());
            }
            &["op", "grant_id"]
        }
        "enable" | "disable" | "uninstall" | "rollback" | "grant" => {
            let package = object.get("package_id").and_then(Value::as_str).ok_or_else(invalid)?;
            PackageId::parse(package).map_err(|_| invalid())?;
            if op == "rollback" {
                PackageDigest::parse(object.get("digest").and_then(Value::as_str).ok_or_else(invalid)?).map_err(|_| invalid())?;
                &["op", "package_id", "digest"]
            } else {
                &["op", "package_id"]
            }
        }
        _ => return Err(invalid()),
    };
    if object.len() > fields.len() || object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(invalid());
    }
    // Flat, known string/bool fields; no copied array, nested object, or unbounded string.
    Ok(request.clone())
}

/// Registers both admission and an observable retained-result route. A polling client never
/// steals another client's queue entry; only the holder of the opaque ID can acknowledge it.
pub fn register_plugin_worker(services: &mut Services, worker: Arc<PluginWorker>) -> Result<(), Error> {
    if services.contains(PLUGIN_SERVICE) || services.contains(super::RESULT_SERVICE) {
        return Err(Error::new(ErrorKind::Conflict, "plugin host services are already registered"));
    }
    let submit = Arc::clone(&worker);
    services.register(PLUGIN_SERVICE, move |request| Ok(json!({"pending":submit.submit(request)?})))?;
    services.register(super::RESULT_SERVICE, move |request| {
        let id = request.get("pending").and_then(Value::as_str).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing plugin pending ID"))?;
        match request.get("op").and_then(Value::as_str) {
            Some("result") => worker.result(id),
            Some("forget") => {
                worker.forget(id)?;
                Ok(json!({"forgotten":true}))
            }
            _ => Err(Error::new(ErrorKind::InvalidInput, "unknown plugin result request")),
        }
    })
}

#[cfg(test)]
mod tests;
