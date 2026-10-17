//! The run executor (#231): drives a [`RunPlan`] to results. It hands the scheduler the jobs of
//! every step whose inputs are known, and when a step's jobs have all finished, it assembles the
//! step's outputs and materialises the steps waiting on it (resolving their inputs from this
//! run's outputs, within limits, #210). Each finished step becomes a [`StepResult`]: a generation
//! stamped with the input hash of what it was actually made from (#75), which the engine records
//! with [`record_changes`].
//!
//! Pure and synchronous, and it borrows nothing: the host passes the node kinds to each call,
//! feeds it the driver's events ([`Run::job_done`], [`Run::job_failed`]), adds the jobs it
//! returns and cancels the ones it gives up on. [`SourceRunner`] runs the jobs that need no
//! provider ([`JobSpec::Source`]); other job kinds go to the host's runner via [`Run::work`].
//!
//! Host contract: check [`Run::is_finished`] right after [`Run::start`] (a run with nothing to
//! schedule never sends an event); report jobs the scheduler refused as failed; give job ids no
//! other scheduled job uses.

use crate::planner::{InputSource, MAX_LIST_ITEMS, MAX_PLAN_INPUT_BYTES, MAX_STEP_INPUT_BYTES, RunPlan, Step, value_bytes};
use crate::scheduler::{self, JobId, Lane, Outcome, RetryPolicy, Start};
use crate::staleness::{InputHasher, MAX_QUERY_BYTES};
use nodecraft_graph::{Canvas, Change, Generation, MAX_HISTORY, MAX_NODE_BYTES, Node};
use nodecraft_nodes::{InputValue, JobSpec, PlanCtx, Registry};
use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, NodeId, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Most jobs one step may plan.
pub const MAX_STEP_JOBS: usize = MAX_LIST_ITEMS;
/// Most jobs one run may hand out.
pub const MAX_RUN_JOBS: usize = 100_000;
/// The lane jobs that need no provider run on.
pub const LOCAL_LANE: &str = "local";

/// Bounds on what materialising deferred steps may copy. The defaults are the planner's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunLimits {
    /// Items in one list input.
    pub list_items: usize,
    /// Bytes of input values one step is planned with.
    pub step_bytes: usize,
    /// Bytes of input values materialised across the whole run (the planner counts the inputs
    /// known up front separately).
    pub run_bytes: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self { list_items: MAX_LIST_ITEMS, step_bytes: MAX_STEP_INPUT_BYTES, run_bytes: MAX_PLAN_INPUT_BYTES }
    }
}

/// How a run is identified and paced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunOptions {
    /// Identifies the run (random per run): it goes into idempotency keys and generation ids.
    pub id: [u8; 16],
    /// The first scheduler job id to use; the host keeps ids unique in its scheduler.
    pub first_job: u64,
    pub limits: RunLimits,
    pub retry: RetryPolicy,
}

/// A finished step: its node and the generation to record on it.
#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    pub node: NodeId,
    pub generation: Generation,
}

/// A step that didn't produce a result, and why (shown on its card).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepFailure {
    pub node: NodeId,
    pub reason: String,
}

/// What changed after an event: jobs to add to the scheduler, jobs to cancel (unfinished jobs
/// of a step that failed), and steps that finished or failed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub jobs: Vec<scheduler::JobSpec>,
    pub cancel: Vec<JobId>,
    pub finished: Vec<StepResult>,
    pub failed: Vec<StepFailure>,
}

impl Progress {
    fn absorb(&mut self, other: Progress) {
        self.jobs.extend(other.jobs);
        self.cancel.extend(other.cancel);
        self.finished.extend(other.finished);
        self.failed.extend(other.failed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    /// Waiting for this many steps it comes after.
    Waiting(usize),
    /// Its jobs are out; this many haven't finished.
    Running(usize),
    Done,
    Failed,
}

#[derive(Clone, Debug)]
struct StepState {
    step: Step,
    status: Status,
    jobs: Vec<JobId>,
    /// Steps that come after this one.
    dependents: Vec<NodeId>,
}

#[derive(Clone, Debug)]
struct JobState {
    step: NodeId,
    work: JobSpec,
    done: bool,
    /// Its outputs, `(port, value)`, until the step assembles them.
    outputs: Vec<(String, Value)>,
}

/// One run of a plan. See the module docs.
pub struct Run {
    id: [u8; 16],
    canvas_id: CanvasId,
    /// The canvas as the run was planned: what deferred steps are planned against and input
    /// hashes are computed on (with this run's own outputs standing in for upstream results).
    canvas: Canvas,
    hasher: InputHasher,
    steps: BTreeMap<NodeId, StepState>,
    jobs: BTreeMap<JobId, JobState>,
    /// Outputs of finished steps, in `node.outputs` order.
    produced: BTreeMap<NodeId, Vec<Value>>,
    open_steps: usize,
    next_job: u64,
    run_bytes: usize,
    limits: RunLimits,
    retry: RetryPolicy,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// JSON bytes of `value`, counted without a copy (saturating at `usize::MAX`).
fn json_bytes(value: &impl serde::Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

/// Room kept below the node size limit for the JSON around the generations.
const NODE_SLACK: usize = 64 * 1024;

/// The changes that record `generation` on `node` and make it active: one undo step. Give it the
/// live node right before applying them. To make room, the oldest inactive generations are
/// dropped: below [`MAX_HISTORY`] entries, and until the node fits its size limit. `Err` when the
/// result can't be recorded even then.
pub fn record_changes(canvas: CanvasId, node: &Node, generation: Generation) -> Result<Vec<Change>, Error> {
    generation.validate().map_err(|e| invalid(format!("the result can't be recorded: {}", e.message)))?;
    let mut bytes = json_bytes(node).saturating_add(json_bytes(&generation)).saturating_add(NODE_SLACK);
    let mut len = node.history.len();
    let mut changes = Vec::new();
    for old in node.history.iter().filter(|g| Some(g.id) != node.active) {
        if len < MAX_HISTORY && bytes <= MAX_NODE_BYTES {
            break;
        }
        changes.push(Change::RemoveGeneration { canvas, id: node.id, generation: old.id });
        len = len.saturating_sub(1);
        bytes = bytes.saturating_sub(json_bytes(old));
    }
    if len >= MAX_HISTORY || bytes > MAX_NODE_BYTES {
        return Err(invalid("the result is too large to record on its node"));
    }
    let id = generation.id;
    changes.push(Change::AddGeneration { canvas, id: node.id, index: len, generation });
    changes.push(Change::SetActiveGeneration { canvas, id: node.id, generation: Some(id) });
    Ok(changes)
}

impl Run {
    /// Starts a run of `plan`, made on `canvas` (`canvas_id`). Returns the run and the first jobs. `Err` for a malformed plan: a node not
    /// on the canvas, planned twice, or a step that comes after one not planned before it.
    pub fn start(plan: &RunPlan, canvas_id: CanvasId, canvas: &Canvas, kinds: &Registry, options: RunOptions) -> Result<(Self, Progress), Error> {
        let mut steps: BTreeMap<NodeId, StepState> = BTreeMap::new();
        for step in &plan.steps {
            if canvas.nodes().get(&step.node).is_none() {
                return Err(invalid("a planned node is not on the canvas"));
            }
            if steps.contains_key(&step.node) {
                return Err(invalid("a node is planned twice"));
            }
            // Steps come in dependency order: everything a step comes after is planned before it.
            // That also rules out cycles.
            let mut waiting = 0usize;
            for before in &step.after {
                let earlier = steps.get_mut(before).ok_or_else(|| invalid("a step comes after one that isn't planned before it"))?;
                earlier.dependents.push(step.node);
                waiting = waiting.saturating_add(1);
            }
            steps.insert(step.node, StepState { step: step.clone(), status: Status::Waiting(waiting), jobs: Vec::new(), dependents: Vec::new() });
        }
        let hasher = InputHasher::new(canvas, kinds, MAX_QUERY_BYTES)?;
        let open_steps = steps.len();
        let ready: Vec<NodeId> = plan.steps.iter().map(|s| s.node).filter(|n| steps.get(n).is_some_and(|s| s.status == Status::Waiting(0))).collect();
        let mut run = Self {
            id: options.id,
            canvas_id,
            canvas: canvas.clone(),
            hasher,
            steps,
            jobs: BTreeMap::new(),
            produced: BTreeMap::new(),
            open_steps,
            next_job: options.first_job,
            run_bytes: 0,
            limits: options.limits,
            retry: options.retry,
        };
        let mut progress = Progress::default();
        for node in ready {
            progress.absorb(run.begin(kinds, node));
        }
        Ok((run, progress))
    }

    /// The work behind a scheduler job, for the host's runner.
    pub fn work(&self, job: JobId) -> Option<&JobSpec> {
        self.jobs.get(&job).map(|j| &j.work)
    }

    /// Every step finished or failed.
    pub fn is_finished(&self) -> bool {
        self.open_steps == 0
    }

    /// The canvas the run's results go on.
    pub fn canvas_id(&self) -> CanvasId {
        self.canvas_id
    }

    /// The idempotency key of a job of this run: `nodecraft:<run>:<job>` (ASCII, under 64).
    fn key(&self, job: JobId) -> String {
        format!("nodecraft:{}:{}", hex(&self.id), job.0)
    }

    /// The generation id of `node`'s result in this run.
    fn generation_id(&self, node: NodeId) -> GenerationId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nodecraft.run.generation");
        hasher.update(&self.id);
        hasher.update(node.as_bytes());
        let mut bytes = [0u8; 16];
        if let Some(head) = hasher.finalize().as_bytes().get(..16) {
            bytes.copy_from_slice(head);
        }
        GenerationId::from_bytes(bytes)
    }

    fn title(&self, node: NodeId) -> String {
        self.canvas.nodes().get(&node).map_or_else(|| node.to_string(), |n| if n.title.is_empty() { n.kind.clone() } else { n.title.clone() })
    }

    /// A job finished with `outputs` (`(output port, value)`, in order). Late or repeated reports
    /// change nothing.
    pub fn job_done(&mut self, kinds: &Registry, job: JobId, outputs: Vec<(String, Value)>) -> Progress {
        let Some(state) = self.jobs.get_mut(&job) else { return Progress::default() };
        if state.done {
            return Progress::default();
        }
        state.done = true;
        let node = state.step;
        let Some(StepState { status: Status::Running(left), .. }) = self.steps.get_mut(&node) else { return Progress::default() };
        // Keep outputs only while their step can still use them.
        if let Some(state) = self.jobs.get_mut(&job) {
            state.outputs = outputs;
        }
        *left = left.saturating_sub(1);
        if *left > 0 {
            return Progress::default();
        }
        self.finish(kinds, node)
    }

    /// A job failed for good or was cancelled: its step fails (its other jobs are to be
    /// cancelled), and so does everything after it.
    pub fn job_failed(&mut self, job: JobId, error: &str) -> Progress {
        let Some(state) = self.jobs.get_mut(&job) else { return Progress::default() };
        if state.done {
            return Progress::default();
        }
        state.done = true;
        let node = state.step;
        if !matches!(self.steps.get(&node).map(|s| s.status), Some(Status::Running(_))) {
            return Progress::default();
        }
        self.fail(node, error.to_owned())
    }

    /// Fails `node` and, transitively, every step after it.
    fn fail(&mut self, node: NodeId, reason: String) -> Progress {
        let mut progress = Progress::default();
        let mut queue = vec![(node, reason)];
        while let Some((node, reason)) = queue.pop() {
            let Some(state) = self.steps.get_mut(&node) else { continue };
            if matches!(state.status, Status::Done | Status::Failed) {
                continue;
            }
            state.status = Status::Failed;
            self.open_steps = self.open_steps.saturating_sub(1);
            let dependents = state.dependents.clone();
            for job in state.jobs.clone() {
                if let Some(job_state) = self.jobs.get_mut(&job) {
                    job_state.outputs = Vec::new();
                    if !job_state.done {
                        job_state.done = true;
                        progress.cancel.push(job);
                    }
                }
            }
            progress.failed.push(StepFailure { node, reason });
            let title = self.title(node);
            queue.extend(dependents.into_iter().map(|d| (d, format!("skipped: “{title}” didn't produce a result"))));
        }
        progress
    }

    /// Hands out a ready step's jobs: the planned ones, or ones planned now from this run's
    /// outputs. A step with no jobs finishes at once.
    fn begin(&mut self, kinds: &Registry, node: NodeId) -> Progress {
        let planned = match self.steps.get_mut(&node) {
            Some(state) => match state.step.jobs.take() {
                Some(jobs) => Ok(jobs),
                None => self.materialise(kinds, node),
            },
            None => return Progress::default(),
        };
        let jobs = match planned {
            Ok(jobs) if jobs.len() > MAX_STEP_JOBS => Err(format!("it would run more than {MAX_STEP_JOBS} jobs")),
            Ok(jobs) if self.jobs.len().saturating_add(jobs.len()) > MAX_RUN_JOBS => Err(format!("the run would have more than {MAX_RUN_JOBS} jobs")),
            other => other,
        };
        let jobs = match jobs {
            Ok(jobs) => jobs,
            Err(reason) => return self.fail(node, reason),
        };
        let mut progress = Progress::default();
        let mut ids = Vec::with_capacity(jobs.len());
        for work in jobs {
            let id = JobId(self.next_job);
            let Some(next) = self.next_job.checked_add(1) else { return self.fail(node, "the run ran out of job ids".into()) };
            self.next_job = next;
            progress.jobs.push(scheduler::JobSpec { id, deps: Vec::new(), lane: lane(&work), retry: self.retry, idempotency_key: self.key(id) });
            self.jobs.insert(id, JobState { step: node, work, done: false, outputs: Vec::new() });
            ids.push(id);
        }
        let left = ids.len();
        if let Some(state) = self.steps.get_mut(&node) {
            state.jobs = ids;
            state.status = Status::Running(left);
        }
        if left == 0 {
            progress.absorb(self.finish(kinds, node));
        }
        progress
    }

    /// Plans a deferred step from this run's outputs, within [`RunLimits`] (#210).
    fn materialise(&mut self, kinds: &Registry, node: NodeId) -> Result<Vec<JobSpec>, String> {
        let state = self.steps.get(&node).ok_or("the step is gone")?;
        let mut step_bytes = 0usize;
        let mut run_bytes = self.run_bytes;
        let mut inputs = Vec::with_capacity(state.step.inputs.len());
        for input in &state.step.inputs {
            let mut values: Vec<Value> = Vec::new();
            for source in &input.from {
                let (value, splice) = match source {
                    InputSource::Value(value) => (value, false),
                    InputSource::Step { node: up, port, list } => (self.step_output(*up, port)?, *list && input.port_type.many),
                };
                let items: Vec<&Value> = match value {
                    Value::List(items) if splice => items.iter().collect(),
                    other => vec![other],
                };
                if input.port_type.many && values.len().saturating_add(items.len()) > self.limits.list_items {
                    return Err(format!("its {} input would have more than {} items", input.port, self.limits.list_items));
                }
                for item in items {
                    let bytes = value_bytes(item);
                    step_bytes = step_bytes.saturating_add(bytes);
                    run_bytes = run_bytes.saturating_add(bytes);
                    if step_bytes > self.limits.step_bytes {
                        return Err(format!("its inputs are larger than {} bytes", self.limits.step_bytes));
                    }
                    if run_bytes > self.limits.run_bytes {
                        return Err(format!("the run's inputs are larger than {} bytes", self.limits.run_bytes));
                    }
                    values.push(item.clone());
                }
            }
            let value = if input.port_type.many {
                Value::List(values)
            } else {
                let mut values = values.into_iter();
                match (values.next(), values.next()) {
                    (Some(value), None) => value,
                    _ => return Err(format!("its {} input takes one value", input.port)),
                }
            };
            inputs.push(InputValue { port: input.port.clone(), port_type: input.port_type, value });
        }
        self.run_bytes = run_bytes;
        let node = self.canvas.nodes().get(&node).ok_or("the node is gone")?;
        let kind = kinds.get(&node.kind).map_err(|e| e.message)?;
        kind.plan(&PlanCtx { node, inputs: &inputs }).map_err(|e| e.message)
    }

    /// The value a finished step produced on `port`.
    fn step_output(&self, node: NodeId, port: &str) -> Result<&Value, String> {
        let outputs = self.produced.get(&node).ok_or_else(|| format!("{} hasn't produced a result", self.title(node)))?;
        let index = self
            .canvas
            .nodes()
            .get(&node)
            .and_then(|n| n.outputs.iter().position(|p| p.name == port))
            .ok_or_else(|| format!("{} has no {port} output", self.title(node)))?;
        outputs.get(index).ok_or_else(|| format!("{} has no {port} output", self.title(node)))
    }

    /// A step whose jobs are all done: its result, then the steps that were waiting on it.
    fn finish(&mut self, kinds: &Registry, node: NodeId) -> Progress {
        let result = match self.assemble(kinds, node) {
            Ok(result) => result,
            Err(reason) => return self.fail(node, reason),
        };
        let mut progress = Progress { finished: vec![result], ..Progress::default() };
        let dependents = self.steps.get_mut(&node).map(|state| {
            state.status = Status::Done;
            state.dependents.clone()
        });
        self.open_steps = self.open_steps.saturating_sub(1);
        for next in dependents.into_iter().flatten() {
            let ready = match self.steps.get_mut(&next) {
                Some(StepState { status: Status::Waiting(waiting), .. }) => {
                    *waiting = waiting.saturating_sub(1);
                    *waiting == 0
                }
                _ => false,
            };
            if ready {
                progress.absorb(self.begin(kinds, next));
            }
        }
        progress
    }

    /// Assembles a step's outputs and stamps its input hash.
    fn assemble(&mut self, kinds: &Registry, node: NodeId) -> Result<StepResult, String> {
        let target = self.canvas.nodes().get(&node).ok_or("the node is gone")?;
        let job_ids = self.steps.get(&node).map(|s| s.jobs.clone()).unwrap_or_default();
        let mut per_port: Vec<Vec<Value>> = vec![Vec::new(); target.outputs.len()];
        for job in job_ids {
            let outputs = self.jobs.get_mut(&job).map(|j| std::mem::take(&mut j.outputs)).unwrap_or_default();
            for (port, value) in outputs {
                let index = target.outputs.iter().position(|p| p.name == port).ok_or_else(|| format!("a job produced an unknown output {port}"))?;
                let slot = per_port.get_mut(index).ok_or("output index out of range")?;
                if slot.len() >= MAX_LIST_ITEMS {
                    return Err(format!("its {port} output has more than {MAX_LIST_ITEMS} items"));
                }
                slot.push(value);
            }
        }
        let mut outputs = Vec::with_capacity(per_port.len());
        for (port, values) in target.outputs.iter().zip(per_port) {
            if port.port_type.many {
                outputs.push(Value::List(values));
            } else {
                let mut values = values.into_iter();
                match (values.next(), values.next()) {
                    (Some(value), None) => outputs.push(value),
                    (None, _) => return Err(format!("it produced nothing on its {} output", port.name)),
                    _ => return Err(format!("it produced several values on its single {} output", port.name)),
                }
            }
        }
        // What it was made from: the planned canvas, with this run's upstream outputs. An
        // over-budget hash is recorded as unknown rather than failing a paid result.
        let input_hash = self.hasher.hash(&self.canvas, kinds, node, &self.produced).ok().flatten();
        let generation = Generation { id: self.generation_id(node), outputs, input_hash };
        generation.validate().map_err(|e| format!("its result can't be recorded: {}", e.message))?;
        self.produced.insert(node, generation.outputs.clone());
        Ok(StepResult { node, generation })
    }
}

/// The lane a job runs on: local work shares one lane.
fn lane(work: &JobSpec) -> Lane {
    match work {
        JobSpec::Source { .. } => Lane::new(LOCAL_LANE),
    }
}

/// Outputs of finished jobs, by job, until the host hands them to [`Run::job_done`].
pub type Outputs = Arc<Mutex<BTreeMap<JobId, Vec<(String, Value)>>>>;

/// Takes a finished job's outputs.
pub fn take_outputs(outputs: &Outputs, job: JobId) -> Vec<(String, Value)> {
    outputs.lock().unwrap_or_else(PoisonError::into_inner).remove(&job).unwrap_or_default()
}

/// Runs jobs that need no provider: a [`JobSpec::Source`] emits its value. The host registers
/// each job's work before scheduling it and reads the outputs back with [`take_outputs`].
#[derive(Default, Clone)]
pub struct SourceRunner {
    work: Arc<Mutex<BTreeMap<JobId, JobSpec>>>,
    pub outputs: Outputs,
}

impl SourceRunner {
    pub fn register(&self, job: JobId, work: JobSpec) {
        self.work.lock().unwrap_or_else(PoisonError::into_inner).insert(job, work);
    }
}

impl crate::driver::JobRunner for SourceRunner {
    fn run(&self, start: Start, cancel: crate::driver::CancelToken) -> crate::driver::TaskFuture<Outcome> {
        let work = self.work.lock().unwrap_or_else(PoisonError::into_inner).remove(&start.job);
        let outputs = Arc::clone(&self.outputs);
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Outcome::Cancelled;
            }
            match work {
                Some(JobSpec::Source { output, value, .. }) => {
                    outputs.lock().unwrap_or_else(PoisonError::into_inner).insert(start.job, vec![(output, value)]);
                    Outcome::Done
                }
                None => Outcome::Failed { error: format!("{} has no work registered", start.job), transient: false },
            }
        })
    }
}

#[cfg(test)]
mod tests;
