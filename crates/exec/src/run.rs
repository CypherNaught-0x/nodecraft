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
use nodecraft_graph::{Canvas, Change, Generation, Incomplete, JobInput, JobSettings, MAX_HISTORY, MAX_NODE_BYTES, Missing, MissingItem, Node, SentKey};
use nodecraft_nodes::{InputValue, JobSpec, PlanCtx, Registry};
use nodecraft_types::{BillableUnits, CanvasId, Error, ErrorKind, GenerationId, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Identifies the run (random per run): it goes into idempotency keys, generation ids and
    /// drawn random seeds, so a reused id repeats its seeds.
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
/// of a step that failed), steps that finished or failed, and jobs whose results were let go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub jobs: Vec<scheduler::JobSpec>,
    pub cancel: Vec<JobId>,
    pub finished: Vec<StepResult>,
    pub failed: Vec<StepFailure>,
    /// Jobs that finished with a result their step won't use (#611): the job finished after its
    /// step had failed, or another job of its fanned item failed. A host that paid for the
    /// result holds it rather than losing it.
    pub dropped: Vec<JobId>,
}

impl Progress {
    fn absorb(&mut self, other: Progress) {
        self.jobs.extend(other.jobs);
        self.cancel.extend(other.cancel);
        self.finished.extend(other.finished);
        self.failed.extend(other.failed);
        self.dropped.extend(other.dropped);
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
    /// For a fanned step (#480): the items that won't finish in this run, why, and the first
    /// error.
    missing: BTreeMap<usize, (Missing, String)>,
    /// For a fanned step inlined from a technique (#536): the items its result lacks, and why.
    /// The steps after it, inside the same technique, leave those items out.
    gaps: BTreeMap<usize, (Missing, String)>,
    /// The items of `missing` copied from an earlier step's gaps, whose reason already names it.
    inherited: BTreeSet<usize>,
    /// Steps that come after this one.
    dependents: Vec<NodeId>,
}

#[derive(Clone, Debug)]
struct JobState {
    step: NodeId,
    work: JobSpec,
    done: bool,
    /// It reported its result (a repeated report is no result let go).
    reported: bool,
    /// Its outputs, `(port, value)`, until the step assembles them.
    outputs: Vec<(String, Value)>,
    /// The units its provider said it billed (#437), recorded with its settings.
    billable_units: Option<BillableUnits>,
    /// The key it may have been billed under without finishing (#517), kept with its item when
    /// the item fails.
    billed_key: Option<String>,
    /// The key it is sent under, when it is not the run's own: a job of an item a partial result
    /// lacks, sent again under the key it may have been billed under (#517).
    key: Option<String>,
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
    /// Steps inlined from techniques (#94), with the technique node each runs for; their
    /// results stay inside the run, and a failure is reported once, as its technique node's.
    inner: BTreeMap<NodeId, NodeId>,
    /// Nodes whose failure has been reported.
    reported: BTreeSet<NodeId>,
    /// Why jobs were cancelled, passed out as it is so the host still knows a cancellation.
    cancel_reason: Option<String>,
    /// Outputs of finished steps, in `node.outputs` order.
    produced: BTreeMap<NodeId, Vec<Value>>,
    open_steps: usize,
    next_job: u64,
    /// The keys of earlier runs this run sends jobs under again (#517), each at most once.
    sent_again: BTreeSet<String>,
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
/// result can't be recorded even then. A result that fits only with less of its settings is
/// recorded with less of them ([`shed`]), rather than losing a paid result.
pub fn record_changes(canvas: CanvasId, node: &Node, mut generation: Generation) -> Result<Vec<Change>, Error> {
    loop {
        match fit(canvas, node, generation.clone()) {
            Err(_) if shed(&mut generation) => {}
            fitted => return fitted,
        }
    }
}

/// [`record_changes`] for a result kept by hand, such as a partial answer (#504): it joins the
/// node's history, but becomes the active result only when the node has none, so it never
/// replaces one.
pub fn keep_changes(canvas: CanvasId, node: &Node, generation: Generation) -> Result<Vec<Change>, Error> {
    let mut changes = record_changes(canvas, node, generation)?;
    if node.active.is_some() {
        changes.retain(|change| !matches!(change, Change::SetActiveGeneration { .. }));
    }
    Ok(changes)
}

/// Drops part of a result's settings so it can fit (#162): first its jobs' inputs, which can
/// be large and are often wired from elsewhere, so the parameters, drawn seeds among them,
/// survive; then the settings altogether. False once there is nothing left to drop.
fn shed(generation: &mut Generation) -> bool {
    if generation.settings.iter().any(|job| !job.inputs.is_empty()) {
        generation.settings.iter_mut().for_each(|job| job.inputs.clear());
        true
    } else if !generation.settings.is_empty() {
        generation.settings.clear();
        true
    } else {
        false
    }
}

fn fit(canvas: CanvasId, node: &Node, generation: Generation) -> Result<Vec<Change>, Error> {
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
        let inner = plan.inner;
        if inner.iter().any(|(id, owner)| !plan.steps.iter().any(|s| s.node == *id) || !canvas.nodes().contains_key(owner)) {
            return Err(invalid("an inlined step doesn't match the plan"));
        }
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
            steps.insert(
                step.node,
                StepState {
                    step,
                    status: Status::Waiting(waiting),
                    jobs: Vec::new(),
                    job_items: Vec::new(),
                    missing: BTreeMap::new(),
                    gaps: BTreeMap::new(),
                    inherited: BTreeSet::new(),
                    dependents: Vec::new(),
                },
            );
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
            inner,
            reported: BTreeSet::new(),
            cancel_reason: None,
            produced: BTreeMap::new(),
            open_steps,
            next_job: options.first_job,
            sent_again: BTreeSet::new(),
            run_bytes: 0,
            #[cfg(test)]
            copied_input_bytes: std::cell::Cell::new(0),
            #[cfg(test)]
            preflight_stack_peak: std::cell::Cell::new(0),
            limits: options.limits,
            retry: options.retry,
        };
        let progress = run.start_ready(kinds, ready);
        let progress = run.outward(progress);
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

    /// Whether `node`'s step has jobs out that haven't all finished.
    pub fn is_running(&self, node: NodeId) -> bool {
        matches!(self.steps.get(&node), Some(StepState { status: Status::Running(_), .. }))
    }

    /// Whether `node`'s step failed (a partly finished fanned step also counts once it reports why).
    pub fn has_failed(&self, node: NodeId) -> bool {
        matches!(self.steps.get(&node), Some(StepState { status: Status::Failed, .. }))
    }

    /// Whether `node`'s step is done or failed: it won't record anything more in this run.
    pub fn has_ended(&self, node: NodeId) -> bool {
        matches!(self.steps.get(&node), Some(StepState { status: Status::Done | Status::Failed, .. }))
    }

    /// The canvas the run's results go on.
    pub fn canvas_id(&self) -> CanvasId {
        self.canvas_id
    }

    /// The idempotency key of a job of this run: `nodecraft:<run>:<job>` (ASCII, under 64).
    fn key(&self, job: JobId) -> String {
        match self.jobs.get(&job).and_then(|state| state.key.clone()) {
            Some(key) => key,
            None => format!("nodecraft:{}:{}", hex(&self.id), job.0),
        }
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

    /// The node a step's results and failures belong to: its technique node when it is inlined,
    /// otherwise itself.
    pub fn owner(&self, node: NodeId) -> NodeId {
        self.inner.get(&node).copied().unwrap_or(node)
    }

    fn title(&self, node: NodeId) -> String {
        self.canvas.nodes().get(&node).map_or_else(|| node.to_string(), |n| if n.title.is_empty() { n.kind.clone() } else { n.title.clone() })
    }

    /// The units `job`'s provider said it billed (#437), for its step to record with the job's
    /// settings. Call before [`Run::job_done`]; a finished job's report changes nothing.
    pub fn job_billed(&mut self, job: JobId, units: BillableUnits) {
        if let Some(state) = self.jobs.get_mut(&job).filter(|state| !state.done) {
            state.billable_units = Some(units);
        }
    }

    /// `job` is about to fail although the provider may have billed it, under `key` (#517): a
    /// reply lost on submit, or a job it accepted whose outputs failed here. A fanned step keeps
    /// the key with the item, so running the item again sends the job under it rather than
    /// charging for a new one. Call before [`Run::job_failed`]; a key that isn't one of NodeCraft's
    /// own (`nodecraft:…`, short visible ASCII) is ignored.
    pub fn job_may_be_billed(&mut self, job: JobId, key: &str) {
        let valid = key.starts_with("nodecraft:") && key.len() <= nodecraft_graph::MAX_SENT_KEY && key.bytes().all(|b| b.is_ascii_graphic());
        // A job already sent again under a kept key and failed again isn't kept a third time:
        // its failure repeats (an output that can't be read here), so the next run sends it fresh.
        if let Some(state) = self.jobs.get_mut(&job).filter(|state| !state.done && state.key.is_none() && valid) {
            state.billed_key = Some(key.to_owned());
        }
    }

    /// Whether `outputs` fit `job`'s node by the rules its step is assembled with (#369): every
    /// port is one of the node's, a list output takes fewer than [`MAX_LIST_ITEMS`] values (the
    /// graph counts the list itself as an item too), and a
    /// single output at most one, or exactly one when the job fills its step (or its item) alone.
    /// A host checks a model job's converted result with this while it still holds the result,
    /// so a result its step would refuse is kept rather than lost.
    pub fn admit_outputs(&self, job: JobId, outputs: &[(String, Value)]) -> Result<(), String> {
        let state = self.jobs.get(&job).ok_or("the job isn't part of this run")?;
        let target = self.canvas.nodes().get(&state.step).ok_or("the node is gone")?;
        let step = self.steps.get(&state.step).ok_or("the job's step is unknown")?;
        let alone = match step.step.fanout {
            Some(_) => {
                let cell_of = |n: usize| step.job_items.get(n).copied();
                let cell = step.jobs.iter().position(|j| *j == job).and_then(cell_of).ok_or("the job's item is unknown")?;
                step.job_items.iter().filter(|c| **c == cell).count() == 1
            }
            None => step.jobs.len() == 1,
        };
        let mut counts = vec![0usize; target.outputs.len()];
        for (port, _) in outputs {
            let index = target.outputs.iter().position(|p| &p.name == port).ok_or_else(|| format!("a job produced an unknown output {port}"))?;
            let count = counts.get_mut(index).ok_or("output index out of range")?;
            *count = count.saturating_add(1);
        }
        for (port, count) in target.outputs.iter().zip(counts) {
            if port.port_type.many {
                if count >= MAX_LIST_ITEMS {
                    return Err(format!("its {} output has {MAX_LIST_ITEMS} or more items", port.name));
                }
            } else if count > 1 {
                return Err(format!("it produced several values on its single {} output", port.name));
            } else if count == 0 && alone {
                return Err(format!("it produced nothing on its {} output", port.name));
            }
        }
        Ok(())
    }

    /// A job finished with `outputs` (`(output port, value)`, in order). Late or repeated reports
    /// change nothing.
    pub fn job_done(&mut self, kinds: &Registry, job: JobId, outputs: Vec<(String, Value)>) -> Progress {
        let progress = self.job_finished(kinds, job, outputs);
        self.outward(progress)
    }

    /// What the run's own steps did, before [`Run::outward`].
    fn job_finished(&mut self, kinds: &Registry, job: JobId, outputs: Vec<(String, Value)>) -> Progress {
        let Some(state) = self.jobs.get_mut(&job) else { return Progress::default() };
        // A result its step can no longer use (its step failed, or cancelled the job) is let go
        // for the host to deal with; a repeated report changes nothing.
        let late = Progress { dropped: if outputs.is_empty() || state.reported { Vec::new() } else { vec![job] }, ..Progress::default() };
        state.reported = true;
        if state.done {
            return late;
        }
        state.done = true;
        let node = state.step;
        let Some(StepState { status: Status::Running(left), .. }) = self.steps.get_mut(&node) else { return late };
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

    /// Progress as the host sees it: results of steps inlined from techniques stay inside the
    /// run, and their failure is their technique node's, reported once, before the technique
    /// node's own ("skipped") failure would be.
    fn outward(&mut self, mut progress: Progress) -> Progress {
        if self.inner.is_empty() {
            return progress;
        }
        progress.finished.retain(|result| !self.inner.contains_key(&result.node));
        let failed = std::mem::take(&mut progress.failed);
        for failure in failed {
            let node = self.owner(failure.node);
            if !self.reported.insert(node) {
                continue;
            }
            let cancelled = self.cancel_reason.as_ref() == Some(&failure.reason);
            let reason = if node == failure.node || cancelled { failure.reason } else { format!("“{}”: {}", self.title(failure.node), failure.reason) };
            progress.failed.push(StepFailure { node, reason });
        }
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

    /// A job failed for good: its step fails (its other jobs are to be cancelled), and so does
    /// everything after it. In a fanned step only the job's item fails (#480): the other items
    /// carry on, and the step records the ones that finish as a partial result.
    pub fn job_failed(&mut self, kinds: &Registry, job: JobId, error: &str) -> Progress {
        self.job_ended(kinds, job, Missing::Failed, error)
    }

    /// A job was cancelled, `reason` saying why: like [`Run::job_failed`], but its item is
    /// recorded as cancelled rather than failed.
    pub fn job_cancelled(&mut self, kinds: &Registry, job: JobId, reason: &str) -> Progress {
        self.cancel_reason = Some(reason.to_owned());
        self.job_ended(kinds, job, Missing::Cancelled, reason)
    }

    fn job_ended(&mut self, kinds: &Registry, job: JobId, why: Missing, error: &str) -> Progress {
        let progress = self.job_stopped(kinds, job, why, error);
        self.outward(progress)
    }

    /// What the run's own steps did, before [`Run::outward`].
    fn job_stopped(&mut self, kinds: &Registry, job: JobId, why: Missing, error: &str) -> Progress {
        let Some(state) = self.jobs.get_mut(&job) else { return Progress::default() };
        if state.done {
            return Progress::default();
        }
        state.done = true;
        let node = state.step;
        let Some(step) = self.steps.get_mut(&node) else { return Progress::default() };
        if !matches!(step.status, Status::Running(_)) {
            return Progress::default();
        }
        let item = step.jobs.iter().position(|j| *j == job).and_then(|n| step.job_items.get(n).copied());
        let (Some(item), Some(_)) = (item, &step.step.fanout) else { return self.fail(node, error.to_owned()) };
        step.missing.entry(item).or_insert_with(|| (why, error.to_owned()));
        // The item's other jobs can't make it whole any more.
        let mut progress = Progress::default();
        let mut ended = 1usize;
        for (other, of) in step.jobs.iter().zip(&step.job_items) {
            if *of != item || *other == job {
                continue;
            }
            if let Some(state) = self.jobs.get_mut(other) {
                if !std::mem::take(&mut state.outputs).is_empty() {
                    progress.dropped.push(*other);
                }
                if !state.done {
                    state.done = true;
                    progress.cancel.push(*other);
                    ended = ended.saturating_add(1);
                }
            }
        }
        let Status::Running(left) = &mut step.status else { return progress };
        *left = left.saturating_sub(ended);
        if *left > 0 {
            return progress;
        }
        let (finished, ready) = self.finish(kinds, node);
        progress.absorb(finished);
        progress.absorb(self.start_ready(kinds, ready));
        progress
    }

    /// Fails `node` and, transitively, every step after it.
    fn fail(&mut self, node: NodeId, reason: String) -> Progress {
        self.fail_with(node, reason, "didn't produce a result")
    }

    /// [`Run::fail`], the steps after it told the node `skipped`.
    fn fail_with(&mut self, node: NodeId, reason: String, skipped: &str) -> Progress {
        let mut progress = Progress::default();
        let mut queue = vec![(node, reason)];
        let mut skipped = Some(skipped);
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
            let why = skipped.take().unwrap_or("didn't produce a result");
            queue.extend(dependents.into_iter().map(|d| (d, format!("skipped: “{title}” {why}"))));
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
                None => {
                    self.resume_deferred(kinds, node);
                    self.materialise(kinds, node)
                }
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
            if let Some((witness, usage)) = &mut self.admission {
                if !eager {
                    witness.normalize(node, &mut jobs, usage).map_err(|e| e.message)?;
                }
                for (n, job) in jobs.iter_mut().enumerate() {
                    seed::draw(witness, usage, &self.id, node, n, job).map_err(|e| e.message)?;
                }
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
        let reused = self.steps.get(&node).map(|s| reused_keys(&s.step, &job_items)).unwrap_or_default();
        for (n, (work, lane)) in jobs.into_iter().zip(lanes).enumerate() {
            let id = JobId(self.next_job);
            let Some(next) = self.next_job.checked_add(1) else { return (self.fail(node, "the run ran out of job ids".into()), Vec::new()) };
            self.next_job = next;
            // A key shared by two jobs would make them one charge: the second runs afresh.
            let key = reused.get(&n).filter(|key| self.sent_again.insert((*key).clone())).cloned();
            self.jobs.insert(id, JobState { step: node, work, done: false, reported: false, outputs: Vec::new(), billable_units: None, billed_key: None, key });
            progress.jobs.push(scheduler::JobSpec { id, deps: Vec::new(), lane, retry: self.retry, idempotency_key: self.key(id) });
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

    /// A deferred fanned step whose node shows a partial result made from the inputs this run
    /// has just made for it (#518) completes that result: only its missing items are planned,
    /// and the others are copied from it ([`crate::partial`]). Steps inlined from a technique,
    /// and technique nodes, keep what the planner decided.
    fn resume_deferred(&mut self, kinds: &Registry, node: NodeId) {
        if self.inner.contains_key(&node) || self.inner.values().any(|owner| *owner == node) {
            return;
        }
        let Some(fanout) = self.steps.get(&node).and_then(|s| s.step.fanout.as_ref()).filter(|f| f.resume.is_none()) else { return };
        if crate::partial::completable(&self.canvas, node, &fanout.items).is_none() {
            return;
        }
        let Ok(Some(now)) = self.hasher.hash(&self.canvas, kinds, node, &self.produced) else { return };
        let Some(state) = self.steps.get_mut(&node) else { return };
        let Some(fanout) = state.step.fanout.as_mut() else { return };
        fanout.resume = crate::partial::completing(&self.canvas, node, &fanout.items, now);
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
        let (skip, gapped) = self.left_out(node, &cells)?;
        let running = || cells.iter().zip(&skip).filter(|(_, skip)| !**skip).map(|(inputs, _)| inputs);
        let mut run_bytes = self.run_bytes;
        // Sources are immutable throughout this synchronous call. Validate the complete shape
        // and budget using borrowed slices before copying any accepted prefix of a failing step.
        for inputs in running() {
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
            for inputs in running() {
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
        for (planned, skip) in cells.iter().zip(&skip) {
            if *skip {
                counts.push(0);
                continue;
            }
            let mut inputs = Vec::with_capacity(planned.len());
            for input in *planned {
                let mut values = Vec::new();
                let mut origins = Vec::new();
                for (index, source) in input.from.iter().enumerate() {
                    let origin = input.origins.get(index).copied();
                    for item in self.input_items(source, input.port_type.many)? {
                        values.push(item.clone());
                        origins.extend(origin);
                        #[cfg(test)]
                        self.copied_input_bytes.set(self.copied_input_bytes.get().saturating_add(crate::planner::value_bytes(item)));
                    }
                }
                let value = if input.port_type.many { Value::List(values) } else { values.into_iter().next().ok_or("the preflight input is gone")? };
                if !input.port_type.many {
                    origins.truncate(1);
                }
                inputs.push(InputValue { port: input.port.clone(), port_type: input.port_type, value, origins });
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
        if let Some(state) = self.steps.get_mut(&node) {
            state.inherited.extend(gapped.keys().copied());
            state.missing.extend(gapped);
        }
        Ok((jobs, counts))
    }

    /// The cells of a deferred step that plan nothing (#536): those its resume leaves to its
    /// technique node's last result, and those reading an item an upstream step inside the
    /// technique lacks, which it lacks too (returned with why). `Err` when it reads such a
    /// step's result as a whole.
    #[allow(clippy::type_complexity)]
    fn left_out(&self, node: NodeId, cells: &[&[crate::planner::PlannedInput]]) -> Result<(Vec<bool>, BTreeMap<usize, (Missing, String)>), String> {
        let resume = self.steps.get(&node).and_then(|s| s.step.fanout.as_ref()).and_then(|f| f.resume.as_ref());
        let mut skip = Vec::with_capacity(cells.len());
        let mut gapped = BTreeMap::new();
        for (cell, inputs) in cells.iter().enumerate() {
            let mut left = resume.is_some_and(|r| !r.runs(cell));
            for source in inputs.iter().flat_map(|input| &input.from) {
                match source {
                    InputSource::Step { node: up, .. } if self.steps.get(up).is_some_and(|s| !s.gaps.is_empty()) => {
                        return Err(format!("skipped: “{}” didn't produce a result for every item", self.title(*up)));
                    }
                    InputSource::Item { node: up, index, .. } if !left => {
                        if let Some(gap) = self.steps.get(up).and_then(|s| s.gaps.get(index)) {
                            gapped.insert(cell, gap.clone());
                            left = true;
                        }
                    }
                    _ => {}
                }
            }
            skip.push(left);
        }
        Ok((skip, gapped))
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
            InputSource::Item { node, port, index, list } => match self.step_output(*node, port)? {
                Value::List(values) => (values.get(*index).ok_or_else(|| format!("{} has no result for one of its items", self.title(*node)))?, *list && many),
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
        let missing = self.steps.get_mut(&node).map(|s| std::mem::take(&mut s.missing)).unwrap_or_default();
        let items = self.steps.get(&node).and_then(|s| s.step.fanout.as_ref()).map_or(0, |f| f.items.len());
        if !missing.is_empty() && missing.len() >= items {
            // Nothing finished: the step fails as a whole.
            return (self.fail(node, first_reason(&missing)), Vec::new());
        }
        let result = match self.assemble(kinds, node, &missing) {
            Ok(result) => result,
            Err(reason) => return (self.fail(node, reason), Vec::new()),
        };
        let gaps = self.gaps(node, &missing);
        if !gaps.is_empty() && self.passes_items_on(node) {
            // Inside a technique the items it has carry on (#536): its technique node records
            // what they make as a partial result.
            self.produced.insert(node, result.generation.outputs.clone());
            if let Some(state) = self.steps.get_mut(&node) {
                state.gaps = gaps;
            }
            return self.done(node, Progress { finished: vec![result], ..Progress::default() });
        }
        if !gaps.is_empty() {
            // A partial result is kept, but what comes after it needs every item.
            let reason = if gaps.values().all(|(why, _)| *why == Missing::Cancelled) {
                first_reason(&gaps)
            } else {
                format!("{} of {items} items didn't finish: {}", gaps.len(), first_reason(&gaps))
            };
            let mut progress = self.fail_with(node, reason, "didn't produce a result for every item");
            progress.finished.insert(0, result);
            return (progress, Vec::new());
        }
        self.done(node, Progress { finished: vec![result], ..Progress::default() })
    }

    /// Marks `node` done: the steps that makes ready, with `progress`.
    fn done(&mut self, node: NodeId, progress: Progress) -> (Progress, Vec<NodeId>) {
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

    /// The items `node`'s result lacks: those `missing`, and those a step inlined from a
    /// technique left to its technique node's last result (#536). A step for an inlined node
    /// prefixes the reason with the node, as its technique node reports it.
    fn gaps(&self, node: NodeId, missing: &BTreeMap<usize, (Missing, String)>) -> BTreeMap<usize, (Missing, String)> {
        let inner = self.inner.contains_key(&node);
        let inherited = self.steps.get(&node).map(|s| &s.inherited);
        let named = |item: &usize| inner && !inherited.is_some_and(|i| i.contains(item));
        let mut gaps: BTreeMap<usize, (Missing, String)> = missing
            .iter()
            .map(|(item, (why, reason))| (*item, (*why, if named(item) { format!("“{}”: {reason}", self.title(node)) } else { reason.clone() })))
            .collect();
        let fanout = self.steps.get(&node).and_then(|s| s.step.fanout.as_ref());
        if let Some(fanout) = fanout
            && let Some(resume) = fanout.resume.as_ref().filter(|r| r.generation.is_none())
        {
            for item in (0..fanout.items.len()).filter(|item| !resume.runs(*item)) {
                gaps.entry(item).or_insert_with(|| (Missing::Cancelled, "its technique node keeps this item from its last result".into()));
            }
        }
        gaps
    }

    /// Whether the steps after `node` can leave out the items its result lacks (#536): it is
    /// inlined from a technique, and each of them is the same technique's, fanned out over the
    /// same items and planned from this run's outputs.
    fn passes_items_on(&self, node: NodeId) -> bool {
        let (Some(owner), Some(state)) = (self.inner.get(&node), self.steps.get(&node)) else { return false };
        let Some(fanout) = &state.step.fanout else { return false };
        state.dependents.iter().all(|next| {
            let ours = *next == *owner || self.inner.get(next) == Some(owner);
            ours && self.steps.get(next).is_some_and(|s| s.step.jobs.is_none() && s.step.fanout.as_ref().is_some_and(|f| f.items == fanout.items))
        })
    }

    /// Assembles a step's outputs and stamps its input hash. A fanned step (#92) assembles each
    /// item as a step of its own, and each output becomes a list with one value per item.
    /// Items in `missing` (#480) get an empty list on every output; a step completing a partial
    /// result copies the items it kept from it.
    fn assemble(&mut self, kinds: &Registry, node: NodeId, missing: &BTreeMap<usize, (Missing, String)>) -> Result<StepResult, String> {
        let target = self.canvas.nodes().get(&node).ok_or("the node is gone")?;
        let (job_ids, job_items, items, resume) = match self.steps.get(&node) {
            Some(state) => {
                let fanout = state.step.fanout.as_ref();
                (state.jobs.clone(), state.job_items.clone(), fanout.map(|f| f.items.clone()), fanout.and_then(|f| f.resume.clone()))
            }
            None => (Vec::new(), Vec::new(), None, None),
        };
        let kept = match resume.as_ref().and_then(|r| r.generation) {
            Some(generation) => Some(target.history.iter().find(|g| g.id == generation).ok_or("the partial result it completes is gone")?),
            None => None,
        };
        let cells = items.as_ref().map_or(1, Vec::len);
        let billed = self.billed_keys(&job_ids, &job_items);
        let mut per_cell: Vec<Vec<Vec<Value>>> = vec![vec![Vec::new(); target.outputs.len()]; cells];
        let mut settings: Vec<Vec<JobSettings>> = vec![Vec::new(); cells];
        for (n, job) in job_ids.into_iter().enumerate() {
            let cell = if items.is_some() { *job_items.get(n).ok_or("a job's item is unknown")? } else { 0 };
            let per_port = per_cell.get_mut(cell).ok_or("a job's item is out of range")?;
            if let Some(JobState { work: JobSpec::Generate { provider, instance, model, params, inputs, .. }, billable_units, .. }) = self.jobs.get(&job) {
                let item = if items.is_some() { Some(u32::try_from(cell).map_err(|_| "a job's item is out of range")?) } else { None };
                let inputs = inputs.iter().map(|input| JobInput { role: input.role.clone(), value: input.value.clone() }).collect();
                let cell_settings = settings.get_mut(cell).ok_or("a job's item is out of range")?;
                cell_settings.push(JobSettings {
                    provider: provider.clone(),
                    instance: *instance,
                    model: model.clone(),
                    params: params.clone(),
                    inputs,
                    item,
                    billable_units: *billable_units,
                });
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
        for (cell, per_port) in per_cell.into_iter().enumerate() {
            if missing.contains_key(&cell) {
                settings.get_mut(cell).map(Vec::clear);
                assembled.push(vec![Value::List(Vec::new()); target.outputs.len()]);
                continue;
            }
            if resume.as_ref().is_some_and(|r| r.generation.is_none() && !r.runs(cell)) {
                // Inside a technique (#536): its technique node keeps this item.
                settings.get_mut(cell).map(Vec::clear);
                assembled.push(vec![Value::List(Vec::new()); target.outputs.len()]);
                continue;
            }
            if let (Some(resume), Some(kept)) = (&resume, kept)
                && !resume.runs(cell)
            {
                let outputs = kept.outputs.iter().map(|output| match output {
                    Value::List(values) => values.get(cell).cloned().ok_or("the partial result it completes lacks an item"),
                    _ => Err("the partial result it completes isn't per item"),
                });
                assembled.push(outputs.collect::<Result<Vec<_>, _>>()?);
                if let Some(cell_settings) = settings.get_mut(cell) {
                    let of_cell = |job: &&JobSettings| job.item.and_then(|item| usize::try_from(item).ok()) == Some(cell);
                    *cell_settings = kept.settings.iter().filter(of_cell).cloned().collect();
                }
                continue;
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
        let incomplete = if missing.is_empty() {
            None
        } else {
            let mut items = Vec::with_capacity(missing.len());
            for (item, (why, _)) in missing {
                let mut missing = MissingItem::new(u32::try_from(*item).map_err(|_| "a job's item is out of range")?, *why);
                if *why == Missing::Failed {
                    missing.keys = billed.get(item).cloned().unwrap_or_default();
                }
                items.push(missing);
            }
            Some(Incomplete { missing: items })
        };
        let settings = settings.into_iter().flatten().collect();
        let generation = Generation { id: self.generation_id(node), outputs, input_hash, items: items.unwrap_or_default(), settings, incomplete };
        let generation = recordable(generation)?;
        if generation.incomplete.is_none() {
            self.produced.insert(node, generation.outputs.clone());
        }
        Ok(StepResult { node, generation })
    }
}

impl Run {
    /// Per item, the keys its jobs may have been billed under (#517), by position among the
    /// item's jobs.
    fn billed_keys(&self, jobs: &[JobId], job_items: &[usize]) -> BTreeMap<usize, Vec<SentKey>> {
        let mut keys: BTreeMap<usize, Vec<SentKey>> = BTreeMap::new();
        let mut position: BTreeMap<usize, u32> = BTreeMap::new();
        for (job, item) in jobs.iter().zip(job_items) {
            let at = position.entry(*item).or_insert(0);
            if let Some(key) = self.jobs.get(job).and_then(|state| state.billed_key.clone()) {
                keys.entry(*item).or_default().push(SentKey { job: *at, key });
            }
            *at = at.saturating_add(1);
        }
        keys
    }
}

/// For a step completing a partial result (#517): by index into its planned jobs, the key each
/// job that may have been billed last time is sent under again.
fn reused_keys(step: &Step, job_items: &[usize]) -> BTreeMap<usize, String> {
    let Some(resume) = step.fanout.as_ref().and_then(|f| f.resume.as_ref()) else { return BTreeMap::new() };
    let mut reused = BTreeMap::new();
    let mut position: BTreeMap<usize, u32> = BTreeMap::new();
    for (n, item) in job_items.iter().enumerate() {
        let at = position.entry(*item).or_insert(0);
        let missing = u32::try_from(*item).ok().and_then(|item| resume.missing.iter().find(|m| m.item == item));
        if let Some(sent) = missing.and_then(|m| m.keys.iter().find(|k| k.job == *at)) {
            reused.insert(n, sent.key.clone());
        }
        *at = at.saturating_add(1);
    }
    reused
}

/// A result as it can be recorded. Settings too large to keep alongside it are dropped, like
/// an over-budget input hash, rather than failing a paid result.
fn recordable(mut generation: Generation) -> Result<Generation, String> {
    while generation.validate().is_err() && shed(&mut generation) {}
    generation.validate().map_err(|e| format!("its result can't be recorded: {}", e.message))?;
    Ok(generation)
}

/// Why a step's items didn't finish: the first failure's error, else the first cancel's reason.
fn first_reason(missing: &BTreeMap<usize, (Missing, String)>) -> String {
    let failed = missing.values().find(|(why, _)| *why == Missing::Failed);
    failed.or_else(|| missing.values().next()).map_or_else(String::new, |(_, reason)| reason.clone())
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

    /// [`ActionExecutor::execute`] for work long enough to stop early: `cancelled` turns true
    /// once the run is cancelled. Quick actions needn't look.
    fn execute_cancellable(
        &self,
        action: &str,
        params: &nodecraft_types::ParamValues,
        inputs: &[Value],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        let _ = cancelled;
        self.execute(action, params, inputs)
    }

    /// Whether `action` can take minutes (a video render), so it gets a thread of its own rather
    /// than holding one of the runtime's. Natively only: the browser runs every action in place.
    fn long_running(&self, action: &str) -> bool {
        let _ = action;
        false
    }
}

/// Runs one action job on `actions`: a long one on its own thread, natively.
async fn execute(
    actions: Arc<dyn ActionExecutor>,
    action: String,
    params: nodecraft_types::ParamValues,
    inputs: Vec<Value>,
    cancel: crate::driver::CancelToken,
) -> Result<Value, String> {
    #[cfg(not(target_arch = "wasm32"))]
    if actions.long_running(&action) {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new().name("nodecraft-action".into()).spawn(move || {
            let _ = sender.send(actions.execute_cancellable(&action, &params, &inputs, &|| cancel.is_cancelled()));
        });
        return match spawned {
            // A thread that ends without an answer panicked; its job fails rather than the run.
            Ok(_) => receiver.await.unwrap_or_else(|_| Err("the action stopped unexpectedly".into())),
            Err(_) => Err("the action's thread couldn't start".into()),
        };
    }
    actions.execute_cancellable(&action, &params, &inputs, &|| cancel.is_cancelled())
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
                Some(JobSpec::Action { action, params, inputs, output, .. }) => match actions {
                    Some(actions) => match execute(actions, action, params, inputs, cancel.clone()).await {
                        Ok(value) => {
                            outputs.lock().unwrap_or_else(PoisonError::into_inner).insert(start.job, vec![(output, value)]);
                            Outcome::Done
                        }
                        Err(error) => Outcome::Failed { error, transient: false },
                    },
                    None => Outcome::Failed { error: "this host can't run actions".into(), transient: false },
                },
                None => Outcome::Failed { error: format!("{} has no work registered", start.job), transient: false },
            }
        })
    }
}

mod resume;
mod seed;
pub use resume::{RecipePort, ResumeRecipe};
#[cfg(test)]
mod tests;

#[cfg(test)]
mod partial_tests;

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
