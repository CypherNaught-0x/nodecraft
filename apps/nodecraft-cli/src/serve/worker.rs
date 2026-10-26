//! The worker thread: loads the project, lists its techniques, then runs queued requests one at a
//! time, each in a fresh session made from the loaded snapshot.
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nodecraft_engine::Session;
use nodecraft_engine::commands::Services;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_format::{MemoryStorage, Snapshot, VerifiedAsset};
use nodecraft_graph::{Canvas, Change};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value as Output};
use serde_json::{Value, json};

use super::Policy;
use tokio::sync::mpsc::Sender;

use super::webhook::{self, Delivery};
use crate::quote::Maker;
use crate::run::{Consent, Runner, Spend};

/// Runs remembered at most; the oldest finished one is forgotten first.
pub const MAX_RUNS: usize = 256;
/// Bytes of run results kept at most; the oldest finished run's files are let go first.
pub const MAX_KEPT_BYTES: u64 = 1024 * 1024 * 1024;
/// Bytes of uploads waiting in the queue at most, on top of the kept results.
pub const MAX_QUEUED_BYTES: u64 = 256 * 1024 * 1024;
/// How long an `Idempotency-Key` is remembered, even after its run is forgotten.
pub const KEY_TTL: Duration = Duration::from_secs(24 * 3600);
/// Keys remembered at most; past it new keyed runs are refused until old keys expire.
pub const MAX_KEYS: usize = 65_536;
/// Items of one list output kept at most.
const MAX_ITEMS: usize = 64;

/// The served project: its snapshot, and how to build provider adapters for a run.
pub struct Loaded {
    pub snapshot: Snapshot,
    /// Canonical source location for stable identity across read-only requests.
    pub origin: Option<nodecraft_format::StorageLocation>,
    pub maker: Box<dyn FnMut() -> Result<Box<Maker<'static>>, Error>>,
}

/// One input of a technique, as `GET /techniques` lists it.
#[derive(Clone, Debug)]
pub struct InputSpec {
    pub port: String,
    /// Whether a request can fill it: a text, number or media input, not one only a wire feeds.
    pub fillable: bool,
}

/// One published technique version the server runs.
#[derive(Clone, Debug)]
pub struct Entry {
    pub technique: String,
    pub version: u64,
    /// The canvas holding that version.
    pub canvas: CanvasId,
    pub inputs: Vec<InputSpec>,
    /// Parameter names a request may set.
    pub params: Vec<String>,
    /// Each output port with the name the form shows for it.
    pub outputs: Vec<(String, String)>,
    /// How `GET /techniques` shows it.
    pub listed: Value,
}

/// A value for one input: typed engine JSON, or a file to import.
#[derive(Debug)]
pub enum Given {
    Value(Value),
    Media { name: String, bytes: Vec<u8> },
}

/// A run to do.
#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub canvas: CanvasId,
    pub inputs: Vec<(String, Vec<Given>)>,
    pub params: Vec<(String, Value)>,
    /// Output names by port, from the technique's form.
    pub names: Vec<(String, String)>,
    pub policy: Policy,
    /// Where to tell the client the run ended.
    pub webhook: Option<url::Url>,
}

impl Job {
    /// The bytes it holds: its files, and its values as JSON.
    pub fn bytes(&self) -> u64 {
        let size = |given: &Given| {
            let len = match given {
                Given::Media { bytes, .. } => bytes.len(),
                Given::Value(value) => serde_json::to_vec(value).map_or(0, |json| json.len()),
            };
            u64::try_from(len).unwrap_or(u64::MAX)
        };
        self.inputs.iter().flat_map(|(_, given)| given).map(size).fold(0, u64::saturating_add)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
    fn is_done(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

/// One result of a run.
pub struct Produced {
    pub name: String,
    pub port: String,
    pub item: Option<usize>,
    pub body: Body,
}

pub enum Body {
    Text(String),
    Media {
        mime: String,
        asset: VerifiedAsset,
    },
    /// A number, a flag or anything else that has no file.
    Value(Value),
    /// A file let go to keep the server's memory bounded; it keeps its place in the list.
    Expired {
        mime: String,
    },
}

/// How a run's webhook delivery stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookStatus {
    Pending,
    Delivered,
    Failed,
}

impl HookStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }
}

/// A run's webhook: how its delivery stands, after how many attempts, and the last failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub status: HookStatus,
    pub attempts: usize,
    pub error: Option<String>,
}

/// A run, as `GET /runs/{id}` reports it.
pub struct Run {
    pub status: Status,
    pub technique: String,
    pub version: u64,
    /// The `Idempotency-Key` it was made with, and its request's fingerprint.
    pub key: Option<(String, u64)>,
    pub quote: Option<Value>,
    pub steps: Vec<Value>,
    pub outputs: Vec<Produced>,
    /// Its files were let go to keep the server's memory bounded.
    pub expired: bool,
    pub error: Option<Error>,
    /// Its webhook, if the request named one.
    pub webhook: Option<Hook>,
}

impl Run {
    pub fn new(technique: String, version: u64, key: Option<(String, u64)>) -> Self {
        Self {
            status: Status::Queued,
            technique,
            version,
            key,
            quote: None,
            steps: Vec::new(),
            outputs: Vec::new(),
            expired: false,
            error: None,
            webhook: None,
        }
    }

    fn kept_bytes(&self) -> u64 {
        self.outputs.iter().map(|o| if let Body::Media { asset, .. } = &o.body { asset.byte_len() } else { 0 }).fold(0, u64::saturating_add)
    }
}

/// Every run the server remembers, oldest first.
#[derive(Default)]
pub struct Runs(Mutex<Book>);

#[derive(Default)]
pub struct Book {
    pub order: VecDeque<String>,
    pub runs: HashMap<String, Run>,
    kept: u64,
    /// Each `Idempotency-Key` seen, kept for [`KEY_TTL`] even once its run is forgotten.
    keys: HashMap<String, Remembered>,
    /// Bytes of uploads in jobs the worker hasn't taken yet.
    queued: u64,
}

/// What a key was used for.
struct Remembered {
    fingerprint: u64,
    run: String,
    at: Instant,
}

/// A key's run, as far as the book knows.
pub enum Keyed<'a> {
    /// The run, still remembered.
    Run(&'a String, &'a Run),
    /// The key was used for a run since forgotten.
    Forgotten { fingerprint: u64 },
}

impl Runs {
    /// The book; a poisoned lock is taken over, since every change to it is one assignment.
    pub fn lock(&self) -> MutexGuard<'_, Book> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Book {
    /// Adds a run, forgetting the oldest finished one when full. `None` if every run is unfinished.
    pub fn add(&mut self, id: String, run: Run) -> Option<()> {
        if self.runs.len() >= MAX_RUNS {
            let oldest = self.order.iter().position(|id| self.runs.get(id).is_some_and(|run| run.status.is_done()))?;
            if let Some(gone) = self.order.remove(oldest).and_then(|id| self.runs.remove(&id)) {
                self.kept = self.kept.saturating_sub(gone.kept_bytes());
            }
        }
        self.order.push_back(id.clone());
        self.runs.insert(id, run);
        Some(())
    }

    /// What `key` was used for: its run while that is remembered, and after that until
    /// [`KEY_TTL`] has passed since `now`.
    pub fn keyed(&self, key: &str, now: Instant) -> Option<Keyed<'_>> {
        let seen = self.keys.get(key)?;
        match self.runs.get_key_value(&seen.run) {
            Some((id, run)) => Some(Keyed::Run(id, run)),
            None if self.lives(seen, now) => Some(Keyed::Forgotten { fingerprint: seen.fingerprint }),
            None => None,
        }
    }

    /// Whether a key still protects: its run is remembered, or it is younger than [`KEY_TTL`].
    fn lives(&self, seen: &Remembered, now: Instant) -> bool {
        self.runs.contains_key(&seen.run) || now.saturating_duration_since(seen.at) < KEY_TTL
    }

    /// Remembers that `key` made run `id`. `false` if [`MAX_KEYS`] unexpired keys are held.
    pub fn remember(&mut self, key: String, fingerprint: u64, id: String, now: Instant) -> bool {
        if self.keys.len() >= MAX_KEYS {
            let runs = &self.runs;
            self.keys.retain(|_, seen| runs.contains_key(&seen.run) || now.saturating_duration_since(seen.at) < KEY_TTL);
            if self.keys.len() >= MAX_KEYS {
                return false;
            }
        }
        self.keys.insert(key, Remembered { fingerprint, run: id, at: now });
        true
    }

    /// Counts `bytes` of uploads as queued. `false`, counting nothing, if they don't fit.
    pub fn queue_bytes(&mut self, bytes: u64) -> bool {
        match self.queued.checked_add(bytes) {
            Some(total) if total <= MAX_QUEUED_BYTES => {
                self.queued = total;
                true
            }
            _ => false,
        }
    }

    /// Stops counting `bytes` of uploads as queued.
    pub fn unqueue_bytes(&mut self, bytes: u64) {
        self.queued = self.queued.saturating_sub(bytes);
    }

    /// Forgets a run that never reached the worker, and the key it was made with.
    pub fn drop_run(&mut self, id: &str) {
        self.runs.remove(id);
        self.order.retain(|other| other != id);
        self.keys.retain(|_, seen| seen.run != id);
    }

    /// Records a finished run, then lets the oldest files go until the kept bytes fit.
    fn finish(&mut self, id: &str, done: impl FnOnce(&mut Run)) {
        let Some(run) = self.runs.get_mut(id) else { return };
        done(run);
        self.kept = self.kept.saturating_add(run.kept_bytes());
        for old in &self.order {
            if self.kept <= MAX_KEPT_BYTES {
                break;
            }
            let Some(run) = self.runs.get_mut(old).filter(|run| run.status.is_done() && !run.expired) else { continue };
            self.kept = self.kept.saturating_sub(run.kept_bytes());
            for output in &mut run.outputs {
                if let Body::Media { mime, .. } = &output.body {
                    output.body = Body::Expired { mime: mime.clone() };
                }
            }
            run.expired = true;
        }
    }

    /// Records how a run's webhook delivery went.
    pub fn hook(&mut self, id: &str, status: HookStatus, attempts: usize, error: Option<String>) {
        if let Some(run) = self.runs.get_mut(id) {
            run.webhook = Some(Hook { status, attempts, error });
        }
    }

    fn set_status(&mut self, id: &str, status: Status) {
        if let Some(run) = self.runs.get_mut(id) {
            run.status = status;
        }
    }
}

/// Starts the worker: it loads the project with `load`, and once it has listed the techniques it
/// takes jobs from `queue`. Returns the techniques, or why the project can't be served.
///
/// A finished run whose request named a webhook is offered to `hooks`, the delivery thread.
pub fn spawn(
    load: impl FnOnce() -> Result<Loaded, Error> + Send + 'static,
    queue: Receiver<Job>,
    runs: Arc<Runs>,
    hooks: Option<Sender<Delivery>>,
    spend: Spend,
) -> Result<Vec<Entry>, Error> {
    let (ready, listed) = channel();
    std::thread::Builder::new()
        .name("nodecraft-serve-runs".into())
        .spawn(move || {
            let prepared = load().and_then(|loaded| Worker::new(loaded, spend));
            let mut worker = match prepared {
                Ok((worker, entries)) => {
                    let _ = ready.send(Ok(entries));
                    worker
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            // Ends when the server is gone and the queue with it.
            while let Ok(job) = queue.recv() {
                {
                    let mut book = runs.lock();
                    book.unqueue_bytes(job.bytes());
                    book.set_status(&job.id, Status::Running);
                }
                // A bug in one run fails that run, not the server: it is reported, never left running.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run(&job)))
                    .unwrap_or_else(|_| Err((Error::new(ErrorKind::Internal, "the run stopped on an internal error; the server keeps serving"), None)));
                let mut book = runs.lock();
                book.finish(&job.id, |run| match outcome {
                    Ok(done) => {
                        run.status = if done.failed { Status::Failed } else { Status::Succeeded };
                        run.quote = Some(done.quote);
                        run.steps = done.steps;
                        run.outputs = done.outputs;
                        if done.failed && run.error.is_none() {
                            run.error = Some(Error::new(ErrorKind::Internal, "a step of the run didn't finish; see its steps"));
                        }
                    }
                    Err((error, quote)) => {
                        run.status = Status::Failed;
                        run.quote = quote;
                        run.error = Some(error);
                    }
                });
                if let Some(url) = job.webhook {
                    notify(&mut book, &job.id, url, hooks.as_ref());
                }
            }
        })
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the run thread: {e}")))?;
    listed.recv().map_err(|_| Error::new(ErrorKind::Internal, "the run thread stopped while loading the project"))?
}

/// Offers a finished run's webhook to the delivery thread, as the run then stands.
fn notify(book: &mut Book, id: &str, url: url::Url, hooks: Option<&Sender<Delivery>>) {
    let Some(run) = book.runs.get(id) else { return };
    let event = format!("evt_{id}");
    let mut described = super::api::describe(id, run);
    // The delivery's own state is no news to its receiver.
    if let Some(described) = described.as_object_mut() {
        described.remove("webhook");
    }
    let body = webhook::payload(&event, described);
    let offered = match hooks {
        Some(hooks) => webhook::offer(hooks, Delivery { run: id.to_owned(), url, event, body }),
        None => Err("this server sends no webhooks"),
    };
    if let Err(why) = offered {
        book.hook(id, HookStatus::Failed, 0, Some(why.into()));
    }
}

/// What a run did.
struct Done {
    quote: Value,
    steps: Vec<Value>,
    outputs: Vec<Produced>,
    failed: bool,
}

struct Worker {
    loaded: Loaded,
    /// The scratch canvas each run places its technique on.
    canvas: CanvasId,
    new_id: Box<dyn FnMut() -> [u8; 16]>,
    /// What every run has spent, held to the server's overall budget.
    spend: Spend,
}

impl Worker {
    /// Adds the scratch canvas to the snapshot and lists what it serves.
    fn new(mut loaded: Loaded, spend: Spend) -> Result<(Self, Vec<Entry>), Error> {
        let mut new_id = crate::random_bytes();
        let canvas = CanvasId::from_bytes(new_id());
        Change::AddCanvas { canvas: Canvas::new(canvas, "Served runs") }.apply(&mut loaded.snapshot.project)?;
        let mut worker = Self { loaded, canvas, new_id: Box::new(new_id), spend };
        let entries = worker.entries()?;
        Ok((worker, entries))
    }

    fn session(&self, services: Services) -> Result<Session, Error> {
        let storage = Box::new(MemoryStorage::default());
        let session = match self.loaded.origin.clone() {
            Some(origin) => Session::with_storage_origin(self.loaded.snapshot.clone(), storage, origin)?,
            None => Session::with_storage(self.loaded.snapshot.clone(), storage)?,
        };
        Ok(session.with_services(services))
    }

    fn node_id(&mut self) -> NodeId {
        NodeId::from_bytes((self.new_id)())
    }

    /// Places `version` on the scratch canvas.
    fn place(&mut self, session: &mut Session, version: CanvasId) -> Result<NodeId, Error> {
        let id = self.node_id();
        session.dispatch("technique.place", &json!({"canvas": self.canvas, "version": version, "id": id, "pos": {"x": 0.0, "y": 0.0}}))?;
        Ok(id)
    }

    /// Every published version, with the form a request fills.
    fn entries(&mut self) -> Result<Vec<Entry>, Error> {
        let mut session = self.session(Services::default())?;
        let listed = session.dispatch("technique.list", &json!({}))?;
        let mut entries = Vec::new();
        for version in listed["techniques"].as_array().into_iter().flatten() {
            let Some(canvas) = version["canvas"].as_str().and_then(|c| c.parse::<CanvasId>().ok()) else { continue };
            let id = self.place(&mut session, canvas)?;
            let form = session.dispatch("app.form", &json!({"canvas": self.canvas, "id": id}))?;
            let inputs: Vec<InputSpec> = form["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|input| InputSpec { port: input["port"].as_str().unwrap_or_default().to_owned(), fillable: input["fillable"] == true })
                .collect();
            let params: Vec<String> = form["params"].as_array().into_iter().flatten().filter_map(|p| p["spec"]["name"].as_str().map(str::to_owned)).collect();
            let shown_inputs: Vec<Value> = form["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| json!({"port": i["port"], "name": i["name"], "port_type": i["port_type"], "fillable": i["fillable"], "example": i["example"]}))
                .collect();
            let outputs: Vec<(String, String)> = form["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| Some((o["port"].as_str()?.to_owned(), o["name"].as_str()?.to_owned())))
                .collect();
            let shown_params: Vec<Value> = form["params"].as_array().into_iter().flatten().map(|p| json!({"spec": p["spec"], "default": p["value"]})).collect();
            let listed = json!({
                "technique": version["technique"], "version": version["version"], "name": version["name"], "description": form["description"],
                "category": version["category"], "tags": version["tags"], "inputs": shown_inputs, "params": shown_params, "outputs": form["outputs"],
                "ready": form["ready"], "blocked": form["blocked"],
            });
            entries.push(Entry {
                technique: version["technique"].as_str().unwrap_or_default().to_owned(),
                version: version["version"].as_u64().unwrap_or_default(),
                canvas,
                inputs,
                params,
                outputs,
                listed,
            });
        }
        Ok(entries)
    }

    /// Runs `job` to its end. A failure that came after the quote carries it.
    fn run(&mut self, job: &Job) -> Result<Done, (Error, Option<Value>)> {
        // Counted from when the worker takes the job, not from when it was queued.
        let deadline = job.policy.run_timeout.and_then(|limit| Instant::now().checked_add(limit));
        let mut services = Services::default();
        let mut runner = Runner::with_spend(&mut services, job.id.clone(), &self.spend).map_err(|e| (e, None))?;
        let mut session = self.session(services).map_err(|e| (e, None))?;
        let session = &mut session;
        let id = self.place(session, job.canvas).map_err(|e| (e, None))?;
        self.fill(session, id, job).map_err(|e| (e, None))?;
        let canvas = self.canvas;
        let mut maker = (self.loaded.maker)().map_err(|e| (e, None))?;
        crate::quote::prepare(session, canvas, &[id], &mut *maker).map_err(|e| (e, None))?;
        runner.install(session);
        let quote = crate::quote::quote(session, canvas, &[id]).map_err(|e| (e, None))?;
        if let Err(error) = within(&quote, &job.policy) {
            return Err((error, Some(quote)));
        }
        let steps = runner.run_within(session, canvas, &[id], &quote, Consent { spend: true }, deadline).map_err(|e| (e, Some(quote.clone())))?;
        let failed = steps.iter().any(|step| step["status"] != "recorded");
        let outputs = if failed { Vec::new() } else { outputs(session, canvas, id, &job.names).map_err(|e| (e, Some(quote.clone())))? };
        Ok(Done { quote, steps, outputs, failed })
    }

    /// Fills the placed technique's inputs and parameters from the request.
    fn fill(&mut self, session: &mut Session, id: NodeId, job: &Job) -> Result<(), Error> {
        for (port, given) in &job.inputs {
            let mut values = Vec::with_capacity(given.len());
            for value in given {
                values.push(match value {
                    Given::Value(value) => value.clone(),
                    Given::Media { name, bytes } => {
                        let asset = nodecraft_engine::import::import_media(session, &nodecraft_engine::import::Incoming { name, bytes })?;
                        json!({"type": "media", "value": asset})
                    }
                });
            }
            let seed = self.node_id();
            session.dispatch("app.input_set", &json!({"canvas": self.canvas, "id": id, "port": port, "seed": seed, "values": values}))?;
        }
        for (name, value) in &job.params {
            session.dispatch("node.param_set", &json!({"canvas": self.canvas, "id": id, "name": name, "value": value}))?;
        }
        Ok(())
    }
}

/// Whether `quote` is within `policy`: every charged currency within its limit, and a price
/// that isn't fully known only when allowed. The quote's own checks (budgets, blocks) are
/// `run.start`'s.
pub fn within(quote: &Value, policy: &Policy) -> Result<(), Error> {
    let refused = |message: String| Error::new(ErrorKind::Budget, message);
    for (code, total) in quote["totals"].as_object().into_iter().flatten() {
        let micros = total["micros"].as_u64().ok_or_else(|| Error::new(ErrorKind::Internal, "the quote's total is unreadable"))?;
        if micros == 0 {
            continue;
        }
        match policy.max_cost.get(code) {
            Some(limit) if micros <= *limit => {}
            Some(limit) => return Err(refused(format!("the run would cost {} {code}, over the limit of {} {code}", amount(micros), amount(*limit)))),
            None => {
                return Err(refused(format!(
                    "the run would cost {} {code}, and the server may not spend {code}; start it with `--max-cost {code}=<amount>`",
                    amount(micros)
                )));
            }
        }
    }
    let unpriced = quote["steps"].as_array().into_iter().flatten().any(|step| step["unpriced"].is_string());
    if (quote["complete"] != true || unpriced) && !policy.allow_unpriced {
        return Err(refused("the run's price isn't fully known; start the server with `--allow-unpriced` to run it anyway".into()));
    }
    Ok(())
}

/// Micros as a decimal amount, without trailing zeros.
pub fn amount(micros: u64) -> String {
    let fraction = format!("{:06}", micros % 1_000_000);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() { format!("{}", micros / 1_000_000) } else { format!("{}.{fraction}", micros / 1_000_000) }
}

/// The placed technique's results, each media file held so it can be downloaded.
fn outputs(session: &Session, canvas: CanvasId, id: NodeId, names: &[(String, String)]) -> Result<Vec<Produced>, Error> {
    let missing = || Error::new(ErrorKind::Internal, "the run finished without a result");
    let node = session.document().canvases().get(&canvas).and_then(|c| c.nodes().get(&id)).ok_or_else(missing)?;
    let generation = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)).ok_or_else(missing)?;
    let mut produced = Vec::new();
    for (port, value) in node.outputs.iter().zip(&generation.outputs) {
        let name = names.iter().find(|(p, _)| *p == port.name).map_or_else(|| port.name.clone(), |(_, n)| n.clone());
        let items: Vec<(Option<usize>, &Output)> = match value {
            Output::List(items) => items.iter().take(MAX_ITEMS).enumerate().map(|(n, v)| (Some(n), v)).collect(),
            single => vec![(None, single)],
        };
        for (item, value) in items {
            let body = match value {
                Output::Text(text) => Body::Text(text.clone()),
                Output::Media(asset) => Body::Media {
                    mime: asset.mime.essence().to_owned(),
                    asset: session.assets().open_verified(asset, &TransferCancel::default(), &StreamMetrics::default())?,
                },
                other => Body::Value(serde_json::to_value(other).unwrap_or(Value::Null)),
            };
            produced.push(Produced { name: name.clone(), port: port.name.clone(), item, body });
        }
    }
    Ok(produced)
}

#[cfg(test)]
mod book_tests {
    use super::*;

    #[test]
    fn legacy_serve_sessions_keep_the_same_source_budget_identity() {
        let origin = nodecraft_format::StorageLocation::from_command("public-legacy.ncraft".into()).unwrap();
        let snapshot = Snapshot {
            project: nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "Public fixture").unwrap(),
            assets: Default::default(),
            epoch: 0,
        };
        let mut identities = vec![];
        for _ in 0..2 {
            let loaded = Loaded {
                snapshot: snapshot.clone(),
                origin: Some(origin.clone()),
                maker: Box::new(|| Ok(Box::new(|_| Err(nodecraft_engine::provider_setup::Readiness::NotSetUp("offline".into()))))),
            };
            let (worker, _) = Worker::new(loaded, Spend::default()).unwrap();
            for _ in 0..2 {
                identities.push(worker.session(Services::default()).unwrap().document().id());
            }
        }
        assert!(identities.windows(2).all(|pair| pair[0] == pair[1]), "scratch canvases and repeated requests cannot reset project spending");
        assert_ne!(identities[0], snapshot.project.id());
    }

    fn done(book: &mut Book, id: &str) {
        book.finish(id, |run| run.status = Status::Succeeded);
    }

    #[test]
    fn a_key_outlives_its_run_until_it_expires() {
        let now = Instant::now();
        let mut book = Book::default();
        book.add("first".into(), Run::new("t".into(), 1, Some(("k".into(), 7)))).unwrap();
        assert!(book.remember("k".into(), 7, "first".into(), now));
        done(&mut book, "first");
        assert!(matches!(book.keyed("k", now), Some(Keyed::Run(id, _)) if id == "first"));
        // While its run is remembered a key never expires, however old.
        assert!(matches!(book.keyed("k", now + KEY_TTL * 2), Some(Keyed::Run(..))));
        // Enough newer runs push the first one out of the book; its key stays.
        for n in 0..MAX_RUNS {
            book.add(format!("r{n}"), Run::new("t".into(), 1, None)).unwrap();
            done(&mut book, &format!("r{n}"));
        }
        assert!(!book.runs.contains_key("first"));
        assert!(matches!(book.keyed("k", now), Some(Keyed::Forgotten { fingerprint: 7 })));
        assert!(book.keyed("k", now + KEY_TTL).is_none(), "an expired key is forgotten");
    }

    #[test]
    fn a_full_key_table_refuses_new_keys_until_old_ones_expire() {
        let now = Instant::now();
        let mut book = Book::default();
        for n in 0..MAX_KEYS {
            assert!(book.remember(format!("k{n}"), 0, "run".into(), now));
        }
        assert!(!book.remember("one more".into(), 0, "run".into(), now));
        assert!(book.remember("one more".into(), 0, "run".into(), now + KEY_TTL));
        assert_eq!(book.keys.len(), 1, "the expired keys were let go");
    }

    #[test]
    fn a_dropped_run_takes_its_key_with_it() {
        let now = Instant::now();
        let mut book = Book::default();
        book.add("run".into(), Run::new("t".into(), 1, Some(("k".into(), 1)))).unwrap();
        assert!(book.remember("k".into(), 1, "run".into(), now));
        book.drop_run("run");
        assert!(book.keyed("k", now).is_none());
    }

    #[test]
    fn queued_upload_bytes_are_budgeted() {
        let mut book = Book::default();
        assert!(book.queue_bytes(MAX_QUEUED_BYTES - 10));
        assert!(!book.queue_bytes(11), "past the budget nothing is counted");
        assert!(!book.queue_bytes(u64::MAX));
        assert!(book.queue_bytes(10));
        book.unqueue_bytes(MAX_QUEUED_BYTES);
        assert!(book.queue_bytes(MAX_QUEUED_BYTES));
        book.unqueue_bytes(u64::MAX);
        assert_eq!(book.queued, 0);
        let job = Job {
            id: "j".into(),
            canvas: CanvasId::from_bytes([1; 16]),
            inputs: vec![("a".into(), vec![Given::Media { name: "x".into(), bytes: vec![0; 5] }, Given::Value(json!(1))])],
            params: Vec::new(),
            names: Vec::new(),
            policy: Policy::default(),
            webhook: None,
        };
        assert_eq!(job.bytes(), 6, "five bytes of file and `1`");
    }
}
