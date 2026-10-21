//! The worker thread: loads the project, lists its techniques, then runs queued requests one at a
//! time, each in a fresh session made from the loaded snapshot.
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, MutexGuard};

use nodecraft_engine::Session;
use nodecraft_engine::commands::Services;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_format::{MemoryStorage, Snapshot, VerifiedAsset};
use nodecraft_graph::{Canvas, Change};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value as Output};
use serde_json::{Value, json};

use super::Policy;
use crate::quote::Maker;
use crate::run::{Consent, Runner};

/// Runs remembered at most; the oldest finished one is forgotten first.
pub const MAX_RUNS: usize = 256;
/// Bytes of run results kept at most; the oldest finished run's files are let go first.
pub const MAX_KEPT_BYTES: u64 = 1024 * 1024 * 1024;
/// Items of one list output kept at most.
const MAX_ITEMS: usize = 64;

/// The served project: its snapshot, and how to build provider adapters for a run.
pub struct Loaded {
    pub snapshot: Snapshot,
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
}

impl Run {
    pub fn new(technique: String, version: u64, key: Option<(String, u64)>) -> Self {
        Self { status: Status::Queued, technique, version, key, quote: None, steps: Vec::new(), outputs: Vec::new(), expired: false, error: None }
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

    /// The run made with `key`, if it is still remembered.
    pub fn keyed(&self, key: &str) -> Option<(&String, &Run)> {
        self.runs.iter().find(|(_, run)| run.key.as_ref().is_some_and(|(k, _)| k == key))
    }

    /// Forgets a run that never reached the worker.
    pub fn drop_run(&mut self, id: &str) {
        self.runs.remove(id);
        self.order.retain(|other| other != id);
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

    fn set_status(&mut self, id: &str, status: Status) {
        if let Some(run) = self.runs.get_mut(id) {
            run.status = status;
        }
    }
}

/// Starts the worker: it loads the project with `load`, and once it has listed the techniques it
/// takes jobs from `queue`. Returns the techniques, or why the project can't be served.
pub fn spawn(load: impl FnOnce() -> Result<Loaded, Error> + Send + 'static, queue: Receiver<Job>, runs: Arc<Runs>) -> Result<Vec<Entry>, Error> {
    let (ready, listed) = channel();
    std::thread::Builder::new()
        .name("nodecraft-serve-runs".into())
        .spawn(move || {
            let prepared = load().and_then(Worker::new);
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
                runs.lock().set_status(&job.id, Status::Running);
                // A bug in one run fails that run, not the server: it is reported, never left running.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run(&job)))
                    .unwrap_or_else(|_| Err((Error::new(ErrorKind::Internal, "the run stopped on an internal error; the server keeps serving"), None)));
                runs.lock().finish(&job.id, |run| match outcome {
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
            }
        })
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the run thread: {e}")))?;
    listed.recv().map_err(|_| Error::new(ErrorKind::Internal, "the run thread stopped while loading the project"))?
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
}

impl Worker {
    /// Adds the scratch canvas to the snapshot and lists what it serves.
    fn new(mut loaded: Loaded) -> Result<(Self, Vec<Entry>), Error> {
        let mut new_id = crate::random_bytes();
        let canvas = CanvasId::from_bytes(new_id());
        Change::AddCanvas { canvas: Canvas::new(canvas, "Served runs") }.apply(&mut loaded.snapshot.project)?;
        let mut worker = Self { loaded, canvas, new_id: Box::new(new_id) };
        let entries = worker.entries()?;
        Ok((worker, entries))
    }

    fn session(&self, services: Services) -> Result<Session, Error> {
        Ok(Session::with_storage(self.loaded.snapshot.clone(), Box::new(MemoryStorage::default()))?.with_services(services))
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
        let mut services = Services::default();
        let mut runner = Runner::new(&mut services, job.id.clone()).map_err(|e| (e, None))?;
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
        let steps = runner.run(session, canvas, &[id], &quote, Consent { spend: true }).map_err(|e| (e, Some(quote.clone())))?;
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
