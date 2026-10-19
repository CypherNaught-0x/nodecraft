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

use crate::planner::{InputSource, MAX_LIST_ITEMS, MAX_PLAN_INPUT_BYTES, MAX_STEP_INPUT_BYTES, RunPlan, Step};
use crate::scheduler::{self, JobId, Lane, Outcome, RetryPolicy, Start};
use crate::staleness::{InputHasher, MAX_QUERY_BYTES};
use nodecraft_graph::{Canvas, Change, Generation, JobSettings, MAX_HISTORY, MAX_NODE_BYTES, Node};
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
    /// known up front separately). Complete preflight precedes copying; admitted copies stay
    /// charged even if the kind subsequently rejects planning.
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
    /// For a fanned step (#92): the item each of `jobs` is for.
    job_items: Vec<usize>,
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
    pub(crate) admission: Option<(crate::admission::Witness, crate::admission::Usage)>,
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
    #[cfg(test)]
    copied_input_bytes: std::cell::Cell<usize>,
    #[cfg(test)]
    preflight_stack_peak: std::cell::Cell<usize>,
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
    /// Starts a run of `plan`, made on `canvas` (`canvas_id`). Returns the run and the first
    /// jobs. `Err` for a malformed plan (a node not on the canvas, planned twice, or a step that
    /// comes after one not planned before it), or a canvas too large to hash (as `stale` refuses
    /// it). Input hashes share one byte budget for the run; once it is spent, later results are
    /// recorded without a hash (unknown, never stale).
    pub fn start(plan: &RunPlan, canvas_id: CanvasId, canvas: &Canvas, kinds: &Registry, options: RunOptions) -> Result<(Self, Progress), Error> {
        Self::start_owned(plan.clone(), canvas_id, canvas.clone(), kinds, options, None)
    }

    /// Consume the immutable catalog-admitted work; never clone its snapshot or re-plan it.
    pub fn start_admitted(admitted: crate::admission::AdmittedRunPlan, kinds: &Registry, mut options: RunOptions) -> Result<(Self, Progress), Error> {
        let crate::admission::AdmittedRunPlan { plan, canvas_id, canvas, witness, mut usage } = admitted;
        options.limits.list_items = options.limits.list_items.min(usage.limits.list_items);
        options.limits.step_bytes = options.limits.step_bytes.min(usage.limits.step_bytes);
        options.limits.run_bytes = options.limits.run_bytes.min(usage.limits.run_bytes);
        if usage.bytes > options.limits.run_bytes || usage.steps.values().any(|n| *n > options.limits.step_bytes) {
            return Err(crate::admission::cap());
        }
        usage.limits = options.limits;
        Self::start_owned(plan, canvas_id, canvas, kinds, options, Some((witness, usage)))
    }

    fn start_owned(
        plan: RunPlan,
        canvas_id: CanvasId,
        canvas: Canvas,
        kinds: &Registry,
        options: RunOptions,
        admission: Option<(crate::admission::Witness, crate::admission::Usage)>,
    ) -> Result<(Self, Progress), Error> {
        let mut steps: BTreeMap<NodeId, StepState> = BTreeMap::new();
        let ready: Vec<NodeId> = plan.steps.iter().filter(|s| s.after.is_empty()).map(|s| s.node).collect();
        for step in plan.steps {
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
            steps.insert(step.node, StepState { step, status: Status::Waiting(waiting), jobs: Vec::new(), job_items: Vec::new(), dependents: Vec::new() });
        }
        let hasher = InputHasher::new(&canvas, kinds, MAX_QUERY_BYTES)?;
        let open_steps = steps.len();

        let mut run = Self {
            id: options.id,
            canvas_id,
            canvas,
            admission,
            hasher,
            steps,
            jobs: BTreeMap::new(),
            produced: BTreeMap::new(),
            open_steps,
            next_job: options.first_job,
            run_bytes: 0,
            #[cfg(test)]
            copied_input_bytes: std::cell::Cell::new(0),
            #[cfg(test)]
            preflight_stack_peak: std::cell::Cell::new(0),
            limits: options.limits,
            retry: options.retry,
        };
        let progress = run.start_ready(kinds, ready);
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
        let (mut progress, ready) = self.finish(kinds, node);
        progress.absorb(self.start_ready(kinds, ready));
        progress
    }

    /// Starts `ready` steps, and the steps they make ready in turn (a step with no jobs finishes
    /// at once), with a work list rather than recursion, so long chains can't exhaust the stack.
    fn start_ready(&mut self, kinds: &Registry, ready: Vec<NodeId>) -> Progress {
        let mut progress = Progress::default();
        let mut queue: std::collections::VecDeque<NodeId> = ready.into();
        while let Some(node) = queue.pop_front() {
            let (step, more) = self.begin(kinds, node);
            progress.absorb(step);
            queue.extend(more);
        }
        progress
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
    /// outputs. A step with no jobs finishes at once; the steps that makes ready are returned.
    fn begin(&mut self, kinds: &Registry, node: NodeId) -> (Progress, Vec<NodeId>) {
        let eager = self.steps.get(&node).is_some_and(|s| s.step.jobs.is_some());
        let planned = match self.steps.get_mut(&node) {
            Some(state) => match state.step.jobs.take() {
                Some(jobs) => Ok((jobs, state.step.fanout.as_ref().map(|f| f.jobs.clone()).unwrap_or_default())),
                None => self.materialise(kinds, node),
            },
            None => return (Progress::default(), Vec::new()),
        };
        let (planned, counts) = match planned {
            Ok((jobs, counts)) => (Ok(jobs), counts),
            Err(reason) => (Err(reason), Vec::new()),
        };
        let job_items = match item_of_jobs(&counts, planned.as_ref().map_or(0, Vec::len)) {
            Ok(items) => items,
            Err(reason) => return (self.fail(node, reason), Vec::new()),
        };
        let jobs = match planned {
            Ok(jobs) if jobs.len() > MAX_STEP_JOBS => Err(format!("it would run more than {MAX_STEP_JOBS} jobs")),
            Ok(jobs) if self.jobs.len().saturating_add(jobs.len()) > MAX_RUN_JOBS => Err(format!("the run would have more than {MAX_RUN_JOBS} jobs")),
            other => other,
        };
        let jobs = jobs.and_then(|mut jobs| {
            let instance = self.canvas.nodes().get(&node).ok_or_else(|| "the step node is missing".to_owned())?;
            crate::generation_contract::validate_outputs(instance, &jobs).map_err(str::to_owned)?;
            if jobs.iter().any(|job| matches!(job, JobSpec::Generate { .. })) && self.admission.is_none() {
                return Err("generation requires catalog admission".into());
            }
            if let Some((witness, _)) = &self.admission {
                crate::generation_contract::validate_targets(instance, &jobs, &witness.targets).map_err(|reason| reason.message().to_owned())?;
            }
            if !eager && let Some((witness, usage)) = &mut self.admission {
                witness.normalize(node, &mut jobs, usage).map_err(|e| e.message)?;
            }
            Ok(jobs)
        });
        let jobs = match jobs {
            Ok(jobs) => jobs,
            Err(reason) => return (self.fail(node, reason), Vec::new()),
        };
        let Some(instance) = self.canvas.nodes().get(&node) else {
            return (self.fail(node, "the step node is missing".into()), Vec::new());
        };
        if let Err(reason) = crate::generation_contract::validate_outputs(instance, &jobs) {
            return (self.fail(node, reason.into()), Vec::new());
        }
        let lanes = match jobs.iter().map(lane).collect::<Result<Vec<_>, _>>() {
            Ok(lanes) => lanes,
            Err(error) => return (self.fail(node, error.message), Vec::new()),
        };
        let mut progress = Progress::default();
        let mut ids = Vec::with_capacity(jobs.len());
        for (work, lane) in jobs.into_iter().zip(lanes) {
            let id = JobId(self.next_job);
            let Some(next) = self.next_job.checked_add(1) else { return (self.fail(node, "the run ran out of job ids".into()), Vec::new()) };
            self.next_job = next;
            progress.jobs.push(scheduler::JobSpec { id, deps: Vec::new(), lane, retry: self.retry, idempotency_key: self.key(id) });
            self.jobs.insert(id, JobState { step: node, work, done: false, outputs: Vec::new() });
            ids.push(id);
        }
        let left = ids.len();
        if let Some(state) = self.steps.get_mut(&node) {
            state.jobs = ids;
            state.job_items = job_items;
            state.status = Status::Running(left);
        }
        if left == 0 {
            let (finished, ready) = self.finish(kinds, node);
            progress.absorb(finished);
            return (progress, ready);
        }
        (progress, Vec::new())
    }

    /// Plans a deferred step from this run's outputs, within [`RunLimits`] (#210): its jobs and,
    /// for a fanned step (#92), how many of them each item has. Each item is planned like a step
    /// of its own; all of them count towards the run's bytes.
    fn materialise(&mut self, kinds: &Registry, node: NodeId) -> Result<(Vec<JobSpec>, Vec<usize>), String> {
        let state = self.steps.get(&node).ok_or("the step is gone")?;
        let cells: Vec<&[crate::planner::PlannedInput]> = match &state.step.fanout {
            Some(fanout) => fanout.cells.iter().map(Vec::as_slice).collect(),
            None => vec![state.step.inputs.as_slice()],
        };
        let mut run_bytes = self.run_bytes;
        // Sources are immutable throughout this synchronous call. Validate the complete shape
        // and budget using borrowed slices before copying any accepted prefix of a failing step.
        for inputs in &cells {
            let mut step_bytes = 0usize;
            for input in *inputs {
                let mut count = 0usize;
                for source in &input.from {
                    let items = self.input_items(source, input.port_type.many)?;
                    count = count.checked_add(items.len()).ok_or("too many input values")?;
                    if input.port_type.many && count > self.limits.list_items {
                        return Err(format!("its {} input would have more than {} items", input.port, self.limits.list_items));
                    }
                    for item in items {
                        self.admit_input(item, &mut step_bytes, &mut run_bytes)?;
                    }
                }
                if !input.port_type.many && count != 1 {
                    return Err(format!("its {} input takes one value", input.port));
                }
            }
        }
        if self.admission.is_some() {
            use crate::admission::measure::{add, mul, values};
            let mut charge = 0usize;
            for inputs in &cells {
                charge = add(charge, mul(inputs.len(), std::mem::size_of::<InputValue>()).map_err(|e| e.message)?).map_err(|e| e.message)?;
                for input in *inputs {
                    charge = add(charge, input.port.len()).map_err(|e| e.message)?;
                    for source in &input.from {
                        for item in self.input_items(source, input.port_type.many)? {
                            charge = add(
                                charge,
                                add(if input.port_type.many { std::mem::size_of::<Value>() } else { 0 }, values(item).map_err(|e| e.message)?)
                                    .map_err(|e| e.message)?,
                            )
                            .map_err(|e| e.message)?;
                        }
                    }
                }
            }
            if let Some((_, usage)) = &mut self.admission {
                usage.reserve(node, charge).map_err(|e| e.message)?;
            }
        }
        // This copy pass is admitted in full. Keep its charge even if kind planning rejects it.
        self.run_bytes = run_bytes;
        let instance = self.canvas.nodes().get(&node).ok_or("the node is gone")?;
        let kind = kinds.get(&instance.kind).map_err(|e| e.message)?;
        let fanned = state.step.fanout.is_some();
        let mut jobs = Vec::new();
        let mut counts = Vec::with_capacity(if fanned { cells.len() } else { 0 });
        for planned in &cells {
            let mut inputs = Vec::with_capacity(planned.len());
            for input in *planned {
                let mut values = Vec::new();
                for source in &input.from {
                    for item in self.input_items(source, input.port_type.many)? {
                        values.push(item.clone());
                        #[cfg(test)]
                        self.copied_input_bytes.set(self.copied_input_bytes.get().saturating_add(crate::planner::value_bytes(item)));
                    }
                }
                let value = if input.port_type.many { Value::List(values) } else { values.into_iter().next().ok_or("the preflight input is gone")? };
                inputs.push(InputValue { port: input.port.clone(), port_type: input.port_type, value });
            }
            let planned = kind
                .plan(&PlanCtx {
                    targets: self.admission.as_ref().map_or(&nodecraft_types::EMPTY_TARGET_SELECTION, |(witness, _)| &witness.targets),
                    node: instance,
                    inputs: &inputs,
                })
                .map_err(|e| e.message)?;
            if fanned {
                counts.push(planned.len());
            }
            jobs.extend(planned);
            if jobs.len() > MAX_STEP_JOBS {
                return Err(format!("it would run more than {MAX_STEP_JOBS} jobs"));
            }
        }
        Ok((jobs, counts))
    }

    /// The planner's logical value size, but with scratch space bounded by depth, not list
    /// width. Check graph limits before the later recursive clone and stop on byte overflow.
    fn admit_input(&self, root: &Value, step_bytes: &mut usize, run_bytes: &mut usize) -> Result<(), String> {
        let mut stack = vec![std::slice::from_ref(root).iter()];
        let mut seen = 0usize;
        #[cfg(test)]
        self.preflight_stack_peak.set(self.preflight_stack_peak.get().max(stack.len()));
        while let Some(items) = stack.last_mut() {
            let Some(value) = items.next() else {
                stack.pop();
                continue;
            };
            seen = seen.saturating_add(1);
            if seen > nodecraft_graph::MAX_VALUE_ITEMS {
                return Err("input value exceeds the document item limit".into());
            }
            let payload = match value {
                Value::Text(text) => text.len(),
                Value::Media(asset) => asset.mime.as_str().len().checked_add(64).ok_or("input byte count overflowed")?,
                _ => 0,
            };
            let bytes = std::mem::size_of::<Value>().checked_add(payload).ok_or("input byte count overflowed")?;
            *step_bytes = step_bytes.checked_add(bytes).ok_or("its input byte count overflowed")?;
            *run_bytes = run_bytes.checked_add(bytes).ok_or("the run's input byte count overflowed")?;
            if *step_bytes > self.limits.step_bytes {
                return Err(format!("its inputs are larger than {} bytes", self.limits.step_bytes));
            }
            if *run_bytes > self.limits.run_bytes {
                return Err(format!("the run's inputs are larger than {} bytes", self.limits.run_bytes));
            }
            if let Value::List(children) = value
                && !children.is_empty()
            {
                if stack.len() > nodecraft_graph::MAX_VALUE_DEPTH {
                    return Err("input value exceeds the document depth limit".into());
                }
                stack.push(children.iter());
                #[cfg(test)]
                self.preflight_stack_peak.set(self.preflight_stack_peak.get().max(stack.len()));
            }
        }
        Ok(())
    }

    /// Splice a list output without allocating a temporary collection of references.
    fn input_items<'a>(&'a self, source: &'a InputSource, many: bool) -> Result<&'a [Value], String> {
        let (value, splice) = match source {
            InputSource::Value(value) => (value, false),
            InputSource::Step { node, port, list } => (self.step_output(*node, port)?, *list && many),
            InputSource::Item { node, port, index } => match self.step_output(*node, port)? {
                Value::List(values) => (values.get(*index).ok_or_else(|| format!("{} has no result for one of its items", self.title(*node)))?, false),
                _ => return Err(format!("{} didn't produce a result per item", self.title(*node))),
            },
        };
        Ok(match value {
            Value::List(items) if splice => items.as_slice(),
            other => std::slice::from_ref(other),
        })
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

    /// A step whose jobs are all done: its result, and the steps it makes ready (for the caller
    /// to start).
    fn finish(&mut self, kinds: &Registry, node: NodeId) -> (Progress, Vec<NodeId>) {
        let result = match self.assemble(kinds, node) {
            Ok(result) => result,
            Err(reason) => return (self.fail(node, reason), Vec::new()),
        };
        let progress = Progress { finished: vec![result], ..Progress::default() };
        let mut ready = Vec::new();
        let dependents = self.steps.get_mut(&node).map(|state| {
            state.status = Status::Done;
            state.dependents.clone()
        });
        self.open_steps = self.open_steps.saturating_sub(1);
        for next in dependents.into_iter().flatten() {
            let now_ready = match self.steps.get_mut(&next) {
                Some(StepState { status: Status::Waiting(waiting), .. }) => {
                    *waiting = waiting.saturating_sub(1);
                    *waiting == 0
                }
                _ => false,
            };
            if now_ready {
                ready.push(next);
            }
        }
        (progress, ready)
    }

    /// Assembles a step's outputs and stamps its input hash. A fanned step (#92) assembles each
    /// item as a step of its own, and each output becomes a list with one value per item.
    fn assemble(&mut self, kinds: &Registry, node: NodeId) -> Result<StepResult, String> {
        let target = self.canvas.nodes().get(&node).ok_or("the node is gone")?;
        let (job_ids, job_items, items) = match self.steps.get(&node) {
            Some(state) => (state.jobs.clone(), state.job_items.clone(), state.step.fanout.as_ref().map(|f| f.items.clone())),
            None => (Vec::new(), Vec::new(), None),
        };
        let cells = items.as_ref().map_or(1, Vec::len);
        let mut per_cell: Vec<Vec<Vec<Value>>> = vec![vec![Vec::new(); target.outputs.len()]; cells];
        let mut settings = Vec::new();
        for (n, job) in job_ids.into_iter().enumerate() {
            let cell = if items.is_some() { *job_items.get(n).ok_or("a job's item is unknown")? } else { 0 };
            let per_port = per_cell.get_mut(cell).ok_or("a job's item is out of range")?;
            if let Some(JobState { work: JobSpec::Generate { provider, instance, model, params, .. }, .. }) = self.jobs.get(&job) {
                let item = if items.is_some() { Some(u32::try_from(cell).map_err(|_| "a job's item is out of range")?) } else { None };
                settings.push(JobSettings { provider: provider.clone(), instance: *instance, model: model.clone(), params: params.clone(), item });
            }
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
        let mut assembled = Vec::with_capacity(cells);
        for per_port in per_cell {
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
            assembled.push(outputs);
        }
        let outputs = match &items {
            None => assembled.into_iter().next().unwrap_or_default(),
            // Transpose: per port, one value per item.
            Some(_) => {
                let mut ports: Vec<Vec<Value>> = vec![Vec::with_capacity(cells); target.outputs.len()];
                for outputs in assembled {
                    for (port, value) in ports.iter_mut().zip(outputs) {
                        port.push(value);
                    }
                }
                ports.into_iter().map(Value::List).collect()
            }
        };
        // What it was made from: the planned canvas, with this run's upstream outputs. An
        // over-budget hash is recorded as unknown rather than failing a paid result.
        let input_hash = self.hasher.hash(&self.canvas, kinds, node, &self.produced).ok().flatten();
        let generation = Generation { id: self.generation_id(node), outputs, input_hash, items: items.unwrap_or_default(), settings };
        let generation = recordable(generation)?;
        self.produced.insert(node, generation.outputs.clone());
        Ok(StepResult { node, generation })
    }
}

/// A result as it can be recorded. Settings too large to keep alongside it are dropped, like
/// an over-budget input hash, rather than failing a paid result.
fn recordable(mut generation: Generation) -> Result<Generation, String> {
    if !generation.settings.is_empty() && generation.validate().is_err() {
        generation.settings.clear();
    }
    generation.validate().map_err(|e| format!("its result can't be recorded: {}", e.message))?;
    Ok(generation)
}

/// The item each job is for, from how many jobs each item has; empty when not fanned.
fn item_of_jobs(counts: &[usize], jobs: usize) -> Result<Vec<usize>, String> {
    if counts.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::with_capacity(jobs);
    for (item, count) in counts.iter().enumerate() {
        if items.len().saturating_add(*count) > jobs {
            return Err("its items' jobs don't add up".into());
        }
        items.extend(std::iter::repeat_n(item, *count));
    }
    if items.len() != jobs {
        return Err("its items' jobs don't add up".into());
    }
    Ok(items)
}

/// The lane a job runs on: local work shares one lane.
fn lane(work: &JobSpec) -> Result<Lane, Error> {
    match work {
        JobSpec::Source { .. } | JobSpec::Action { .. } => Ok(Lane::new(LOCAL_LANE)),
        // Every provider identity uses the canonical namespace, including defaults.
        JobSpec::Generate { provider, instance, .. } => Ok(Lane::new(nodecraft_types::ProviderTarget::new(provider, *instance)?.lane_name()?)),
    }
}

/// Outputs of finished jobs, by job, until the host hands them to [`Run::job_done`].
pub type Outputs = Arc<Mutex<BTreeMap<JobId, Vec<(String, Value)>>>>;

/// Takes a finished job's outputs.
pub fn take_outputs(outputs: &Outputs, job: JobId) -> Vec<(String, Value)> {
    outputs.lock().unwrap_or_else(PoisonError::into_inner).remove(&job).unwrap_or_default()
}

/// Runs local actions (#410) for [`SourceRunner`]: the host implements it with access to the
/// project's assets. An error is a message for the person; it never panics.
pub trait ActionExecutor: crate::driver::MaybeSendSync {
    fn execute(&self, action: &str, params: &nodecraft_types::ParamValues, inputs: &[Value]) -> Result<Value, String>;
}

/// Runs jobs that need no provider: a [`JobSpec::Source`] emits its value, and a
/// [`JobSpec::Action`] runs on the host's [`ActionExecutor`]. The host registers each job's work
/// before scheduling it and reads the outputs back with [`take_outputs`].
#[derive(Default, Clone)]
pub struct SourceRunner {
    work: Arc<Mutex<BTreeMap<JobId, JobSpec>>>,
    pub outputs: Outputs,
    actions: Option<Arc<dyn ActionExecutor>>,
}

impl SourceRunner {
    pub fn register(&self, job: JobId, work: JobSpec) {
        self.work.lock().unwrap_or_else(PoisonError::into_inner).insert(job, work);
    }

    /// Runs action jobs on `actions`; without one they fail with a clear message.
    pub fn with_actions(mut self, actions: Arc<dyn ActionExecutor>) -> Self {
        self.actions = Some(actions);
        self
    }
}

impl crate::driver::JobRunner for SourceRunner {
    fn run(&self, start: Start, cancel: crate::driver::CancelToken) -> crate::driver::TaskFuture<Outcome> {
        let work = self.work.lock().unwrap_or_else(PoisonError::into_inner).remove(&start.job);
        let outputs = Arc::clone(&self.outputs);
        let actions = self.actions.clone();
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Outcome::Cancelled;
            }
            match work {
                Some(JobSpec::Source { output, value, .. }) => {
                    outputs.lock().unwrap_or_else(PoisonError::into_inner).insert(start.job, vec![(output, value)]);
                    Outcome::Done
                }
                Some(JobSpec::Generate { .. }) => Outcome::Failed { error: format!("{} needs a provider runner", start.job), transient: false },
                Some(JobSpec::Action { action, params, inputs, output, .. }) => match actions.as_ref().map(|a| a.execute(&action, &params, &inputs)) {
                    Some(Ok(value)) => {
                        outputs.lock().unwrap_or_else(PoisonError::into_inner).insert(start.job, vec![(output, value)]);
                        Outcome::Done
                    }
                    Some(Err(error)) => Outcome::Failed { error, transient: false },
                    None => Outcome::Failed { error: "this host can't run actions".into(), transient: false },
                },
                None => Outcome::Failed { error: format!("{} has no work registered", start.job), transient: false },
            }
        })
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod output_contract_tests;

#[cfg(test)]
mod endpoint_lane_before {
    use super::*;
    #[test]
    fn provider_named_local_does_not_share_the_source_lane() {
        let node = NodeId::from_bytes([7; 16]);
        let source = JobSpec::Source { node, output: "out".into(), value: Value::Text("fixture".into()) };
        let provider = JobSpec::Generate {
            instance: None,
            node,
            outputs: vec![],
            provider: "local".into(),
            model: "fixture".into(),
            params: Default::default(),
            inputs: vec![],
        };
        assert_ne!(lane(&source), lane(&provider), "a provider named local must not consume deterministic-source concurrency");
    }
}

#[cfg(test)]
mod endpoint_concurrency_tests {
    use super::*;
    #[test]
    fn typed_endpoint_lanes_keep_local_separate_and_respect_lane_and_global_caps() {
        let a = nodecraft_types::ProviderTarget::named("local", nodecraft_types::ProviderInstanceId::from_bytes([0; 16])).unwrap();
        let b = nodecraft_types::ProviderTarget::named("local", nodecraft_types::ProviderInstanceId::from_bytes([1; 16])).unwrap();
        let default = nodecraft_types::ProviderTarget::default_instance("local").unwrap();
        let mut limits = scheduler::Limits { global: 3, default_per_lane: 1, ..Default::default() };
        limits.set_target_limit(&a, 1).unwrap();
        limits.set_target_limit(&b, 1).unwrap();
        let mut scheduler = scheduler::Scheduler::new(limits);
        let lanes = [
            Lane::for_target(&a).unwrap(),
            Lane::for_target(&a).unwrap(),
            Lane::for_target(&b).unwrap(),
            Lane::new(LOCAL_LANE),
            Lane::for_target(&default).unwrap(),
        ];
        scheduler
            .add(
                lanes
                    .into_iter()
                    .enumerate()
                    .map(|(n, lane)| scheduler::JobSpec {
                        id: JobId(n as u64),
                        deps: vec![],
                        lane,
                        retry: RetryPolicy::NEVER,
                        idempotency_key: format!("key{n}"),
                    })
                    .collect(),
            )
            .unwrap();
        let started = scheduler.start_ready(0);
        assert_eq!(started.len(), 3);
        assert_eq!(started.iter().filter(|s| s.lane == Lane::for_target(&a).unwrap()).count(), 1);
        assert!(started.iter().any(|s| s.lane == Lane::for_target(&b).unwrap()));
        assert!(started.iter().any(|s| s.lane.0 == LOCAL_LANE));
        assert!(scheduler.start_ready(0).is_empty());
        scheduler.report(JobId(0), Outcome::Done, 0).unwrap();
        let next = scheduler.start_ready(0);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].job, JobId(1));
        assert!(nodecraft_types::ProviderTarget::default_instance(&"x".repeat(513)).is_err());
        let maximum = nodecraft_types::ProviderTarget::named(&"x".repeat(512), nodecraft_types::ProviderInstanceId::from_bytes([9; 16])).unwrap();
        assert!(maximum.lane_name().unwrap().len() <= 1061);
    }
}
