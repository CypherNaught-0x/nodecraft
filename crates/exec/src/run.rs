//! The run executor (#231): drives a [`RunPlan`] to results. It hands the scheduler the jobs of
//! every step whose inputs are known, and when a step's jobs have all finished, it assembles the
//! step's outputs and materialises the steps waiting on it (resolving their inputs from this
//! run's outputs, within the planner's limits, #210). Each finished step becomes a [`StepResult`]:
//! a generation stamped with the input hash of what it was actually made from (#75), which the
//! engine records with [`record_changes`].
//!
//! Pure and synchronous: the host feeds it the driver's events ([`Run::job_done`],
//! [`Run::job_failed`]) and adds the jobs it returns. [`SourceRunner`] runs the jobs that need no
//! provider ([`JobSpec::Source`]); other job kinds go to the host's runner via [`Run::work`].

use crate::planner::{InputSource, MAX_LIST_ITEMS, MAX_PLAN_INPUT_BYTES, MAX_STEP_INPUT_BYTES, RunPlan, Step, value_bytes};
use crate::scheduler::{self, JobId, Lane, Outcome, RetryPolicy, Start};
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Project};
use nodecraft_nodes::{InputValue, JobSpec, PlanCtx, Registry};
use nodecraft_types::{CanvasId, Error, ErrorKind, GenerationId, NodeId, ProjectId, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Most jobs one step may plan.
pub const MAX_STEP_JOBS: usize = MAX_LIST_ITEMS;
/// Most jobs one run may hand out.
pub const MAX_RUN_JOBS: usize = 100_000;
/// Most generations a node keeps (the graph's limit); recording a new one past it drops the
/// oldest inactive one.
pub const MAX_HISTORY: usize = 1024;

/// The lane jobs that need no provider run on.
pub const LOCAL_LANE: &str = "local";

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

/// What changed after an event: jobs to add to the scheduler, steps that finished or failed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub jobs: Vec<scheduler::JobSpec>,
    pub finished: Vec<StepResult>,
    pub failed: Vec<StepFailure>,
}

impl Progress {
    fn absorb(&mut self, other: Progress) {
        self.jobs.extend(other.jobs);
        self.finished.extend(other.finished);
        self.failed.extend(other.failed);
    }
}

#[derive(Clone, Debug)]
enum Status {
    /// Waiting for the steps it comes after.
    Waiting,
    /// Its jobs are out; how many haven't finished yet.
    Running {
        left: usize,
    },
    /// Finished, with its outputs in `node.outputs` order.
    Done(Vec<Value>),
    Failed,
}

#[derive(Clone, Debug)]
struct StepState {
    step: Step,
    status: Status,
    jobs: Vec<JobId>,
}

#[derive(Clone, Debug)]
struct JobState {
    step: NodeId,
    work: JobSpec,
    /// Its outputs, `(port, value)`, once done.
    outputs: Option<Vec<(String, Value)>>,
}

/// One run of a plan. See the module docs.
pub struct Run<'a> {
    id: [u8; 16],
    kinds: &'a Registry,
    canvas: CanvasId,
    /// The canvas as the run was planned, with this run's results applied as they finish: what
    /// input hashes are computed against.
    scratch: Project,
    steps: BTreeMap<NodeId, StepState>,
    order: Vec<NodeId>,
    jobs: BTreeMap<JobId, JobState>,
    next_job: u64,
    plan_bytes: usize,
    retry: RetryPolicy,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The changes that record `generation` on `node` and make it active: one undo step. A node at
/// [`MAX_HISTORY`] first drops its oldest inactive generation.
pub fn record_changes(canvas: CanvasId, node: &Node, generation: Generation) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut len = node.history.len();
    if len >= MAX_HISTORY
        && let Some(oldest) = node.history.iter().find(|g| Some(g.id) != node.active)
    {
        changes.push(Change::RemoveGeneration { canvas, id: node.id, generation: oldest.id });
        len = len.saturating_sub(1);
    }
    let id = generation.id;
    changes.push(Change::AddGeneration { canvas, id: node.id, index: len, generation });
    changes.push(Change::SetActiveGeneration { canvas, id: node.id, generation: Some(id) });
    changes
}

impl<'a> Run<'a> {
    /// Starts a run of `plan`, made on `canvas` (`canvas_id`), with node kinds `kinds`. `id`
    /// identifies the run (random per run): it goes into idempotency keys and generation ids.
    /// Jobs get scheduler ids from `first_job` on. Returns the run and the jobs to schedule.
    pub fn start(
        plan: &RunPlan,
        canvas_id: CanvasId,
        canvas: &Canvas,
        kinds: &'a Registry,
        id: [u8; 16],
        first_job: u64,
        retry: RetryPolicy,
    ) -> Result<(Self, Progress), Error> {
        let mut scratch = Project::new(ProjectId::from_bytes(id), "run")?;
        Change::AddCanvas { canvas: canvas.clone() }.apply(&mut scratch)?;
        let mut steps = BTreeMap::new();
        let mut order = Vec::with_capacity(plan.steps.len());
        for step in &plan.steps {
            if canvas.nodes().get(&step.node).is_none() {
                return Err(invalid("a planned node is not on the canvas"));
            }
            if steps.insert(step.node, StepState { step: step.clone(), status: Status::Waiting, jobs: Vec::new() }).is_some() {
                return Err(invalid("a node is planned twice"));
            }
            order.push(step.node);
        }
        let mut run = Self { id, kinds, canvas: canvas_id, scratch, steps, order, jobs: BTreeMap::new(), next_job: first_job, plan_bytes: 0, retry };
        let progress = run.advance();
        Ok((run, progress))
    }

    /// The work behind a scheduler job, for the host's runner.
    pub fn work(&self, job: JobId) -> Option<&JobSpec> {
        self.jobs.get(&job).map(|j| &j.work)
    }

    /// Every step finished or failed.
    pub fn is_finished(&self) -> bool {
        self.steps.values().all(|s| matches!(s.status, Status::Done(_) | Status::Failed))
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

    /// A job finished with `outputs` (`(output port, value)`, in order).
    pub fn job_done(&mut self, job: JobId, outputs: Vec<(String, Value)>) -> Progress {
        let Some(state) = self.jobs.get_mut(&job) else { return Progress::default() };
        if state.outputs.is_some() {
            return Progress::default();
        }
        state.outputs = Some(outputs);
        let node = state.step;
        let mut progress = Progress::default();
        let left = match self.steps.get_mut(&node) {
            Some(StepState { status: Status::Running { left }, .. }) => {
                *left = left.saturating_sub(1);
                *left
            }
            _ => return progress,
        };
        if left == 0 {
            progress.absorb(self.finish(node));
            progress.absorb(self.advance());
        }
        progress
    }

    /// A job failed (for good) or was cancelled: its step fails, and so does everything after it.
    pub fn job_failed(&mut self, job: JobId, error: &str) -> Progress {
        let Some(node) = self.jobs.get(&job).map(|j| j.step) else { return Progress::default() };
        let mut progress = Progress::default();
        if matches!(self.steps.get(&node).map(|s| &s.status), Some(Status::Running { .. })) {
            progress.failed.push(self.fail(node, error.to_owned()));
            progress.absorb(self.advance());
        }
        progress
    }

    fn fail(&mut self, node: NodeId, reason: String) -> StepFailure {
        if let Some(state) = self.steps.get_mut(&node) {
            state.status = Status::Failed;
        }
        StepFailure { node, reason }
    }

    /// Starts every waiting step whose predecessors are done, and fails those whose predecessors
    /// failed, until nothing changes.
    fn advance(&mut self) -> Progress {
        let mut progress = Progress::default();
        loop {
            let mut changed = false;
            for node in self.order.clone() {
                let Some(state) = self.steps.get(&node) else { continue };
                if !matches!(state.status, Status::Waiting) {
                    continue;
                }
                let mut blocked_by = None;
                let mut ready = true;
                for before in &state.step.after {
                    match self.steps.get(before).map(|s| &s.status) {
                        Some(Status::Done(_)) => {}
                        Some(Status::Failed) => blocked_by = Some(*before),
                        // A predecessor outside the plan can't finish: the planner never does this.
                        None => blocked_by = Some(*before),
                        Some(_) => ready = false,
                    }
                }
                if let Some(before) = blocked_by {
                    progress.failed.push(self.fail(node, format!("skipped: {before} didn't produce a result")));
                    changed = true;
                } else if ready {
                    progress.absorb(self.begin(node));
                    changed = true;
                }
            }
            if !changed {
                return progress;
            }
        }
    }

    /// Hands out a ready step's jobs: the planned ones, or ones planned now from this run's
    /// outputs. A step with no jobs finishes at once.
    fn begin(&mut self, node: NodeId) -> Progress {
        let mut progress = Progress::default();
        let planned = match self.steps.get(&node).map(|s| s.step.jobs.clone()) {
            Some(Some(jobs)) => Ok(jobs),
            Some(None) => self.materialise(node),
            None => return progress,
        };
        let jobs = match planned {
            Ok(jobs) if jobs.len() > MAX_STEP_JOBS => Err(format!("it would run more than {MAX_STEP_JOBS} jobs")),
            Ok(jobs) if self.jobs.len().saturating_add(jobs.len()) > MAX_RUN_JOBS => Err(format!("the run would have more than {MAX_RUN_JOBS} jobs")),
            other => other,
        };
        let jobs = match jobs {
            Ok(jobs) => jobs,
            Err(reason) => {
                progress.failed.push(self.fail(node, reason));
                return progress;
            }
        };
        let mut ids = Vec::with_capacity(jobs.len());
        for work in jobs {
            let id = JobId(self.next_job);
            self.next_job = self.next_job.saturating_add(1);
            progress.jobs.push(scheduler::JobSpec { id, deps: Vec::new(), lane: lane(&work), retry: self.retry, idempotency_key: self.key(id) });
            self.jobs.insert(id, JobState { step: node, work, outputs: None });
            ids.push(id);
        }
        let left = ids.len();
        if let Some(state) = self.steps.get_mut(&node) {
            state.jobs = ids;
            state.status = Status::Running { left };
        }
        if left == 0 {
            progress.absorb(self.finish(node));
        }
        progress
    }

    /// Plans a deferred step from this run's outputs, within the planner's limits (#210).
    fn materialise(&mut self, node: NodeId) -> Result<Vec<JobSpec>, String> {
        let state = self.steps.get(&node).ok_or("the step is gone")?;
        let mut step_bytes = 0usize;
        let mut plan_bytes = self.plan_bytes;
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
                if input.port_type.many && values.len().saturating_add(items.len()) > MAX_LIST_ITEMS {
                    return Err(format!("its {} input would have more than {MAX_LIST_ITEMS} items", input.port));
                }
                for item in items {
                    let bytes = value_bytes(item);
                    step_bytes = step_bytes.saturating_add(bytes);
                    plan_bytes = plan_bytes.saturating_add(bytes);
                    if step_bytes > MAX_STEP_INPUT_BYTES {
                        return Err(format!("its inputs are larger than {} MiB", MAX_STEP_INPUT_BYTES / (1024 * 1024)));
                    }
                    if plan_bytes > MAX_PLAN_INPUT_BYTES {
                        return Err(format!("the run's inputs are larger than {} MiB", MAX_PLAN_INPUT_BYTES / (1024 * 1024)));
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
        self.plan_bytes = plan_bytes;
        let canvas = self.scratch.canvases().get(&self.canvas).ok_or("the run's canvas is gone")?;
        let node = canvas.nodes().get(&node).ok_or("the node is gone")?;
        let kind = self.kinds.get(&node.kind).map_err(|e| e.message)?;
        kind.plan(&PlanCtx { node, inputs: &inputs }).map_err(|e| e.message)
    }

    /// The value a finished step produced on `port`.
    fn step_output(&self, node: NodeId, port: &str) -> Result<&Value, String> {
        let Some(Status::Done(outputs)) = self.steps.get(&node).map(|s| &s.status) else { return Err(format!("{node} hasn't produced a result")) };
        let canvas = self.scratch.canvases().get(&self.canvas).ok_or("the run's canvas is gone")?;
        let index =
            canvas.nodes().get(&node).and_then(|n| n.outputs.iter().position(|p| p.name == port)).ok_or_else(|| format!("{node} has no {port} output"))?;
        outputs.get(index).ok_or_else(|| format!("{node} has no {port} output"))
    }

    /// Assembles a step whose jobs are all done into its result, records it on the run's canvas
    /// and stamps its input hash.
    fn finish(&mut self, node: NodeId) -> Progress {
        let mut progress = Progress::default();
        match self.assemble(node) {
            Ok(result) => progress.finished.push(result),
            Err(reason) => progress.failed.push(self.fail(node, reason)),
        }
        progress
    }

    fn assemble(&mut self, node: NodeId) -> Result<StepResult, String> {
        let state = self.steps.get(&node).ok_or("the step is gone")?;
        let canvas = self.scratch.canvases().get(&self.canvas).ok_or("the run's canvas is gone")?;
        let target = canvas.nodes().get(&node).ok_or("the node is gone")?;
        let mut per_port: Vec<Vec<Value>> = vec![Vec::new(); target.outputs.len()];
        for job in &state.jobs {
            for (port, value) in self.jobs.get(job).and_then(|j| j.outputs.as_ref()).into_iter().flatten() {
                let index = target.outputs.iter().position(|p| &p.name == port).ok_or_else(|| format!("a job produced an unknown output {port}"))?;
                let slot = per_port.get_mut(index).ok_or("output index out of range")?;
                if slot.len() >= MAX_LIST_ITEMS {
                    return Err(format!("its {port} output has more than {MAX_LIST_ITEMS} items"));
                }
                slot.push(value.clone());
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
        // The hash of what it was made from: the run's canvas, with this run's upstream results.
        let input_hash = crate::staleness::input_hash(canvas, self.kinds, node).ok();
        let generation = Generation { id: self.generation_id(node), outputs: outputs.clone(), input_hash };
        // Recording it on the run's canvas checks it fits the document (the engine records the
        // same changes) and lets downstream steps hash against it.
        let changes = record_changes(self.canvas, target, generation.clone());
        Changeset::new(changes).apply(&mut self.scratch).map_err(|e| format!("its result can't be recorded: {}", e.message))?;
        if let Some(state) = self.steps.get_mut(&node) {
            state.status = Status::Done(outputs);
        }
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
