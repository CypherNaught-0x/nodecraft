//! The job scheduler: a deterministic state machine with no I/O and no clock of its own.
//!
//! It decides which jobs start (dependencies done, per-lane and global concurrency free, retry
//! backoff elapsed), records how they end, retries transient failures with backoff under a stable
//! idempotency key (so a retried provider call never charges twice), skips jobs whose inputs
//! failed, and cancels cooperatively. [`crate::driver`] runs it on an async runtime; tests drive
//! it directly with a fake clock.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

/// Cap on jobs held by one scheduler (a batch of batches must not exhaust memory).
pub const MAX_JOBS: usize = 100_000;
/// Cap on dependencies of one job.
pub const MAX_DEPS: usize = 4_096;

/// A job's id, unique within a scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "job {}", self.0)
    }
}

/// What a job runs on, for concurrency limits: one lane per provider or server (`fal`,
/// `comfy:http://gpu-box:8188`), `local` for deterministic actions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lane(pub String);

impl Lane {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// How often and how patiently a job is retried after transient failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in total, the first included (1 = never retry). 0 is treated as 1.
    pub max_attempts: u32,
    /// Delay before the first retry; doubles per attempt.
    pub base_delay_ms: u64,
    /// Longest delay between attempts.
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 3, base_delay_ms: 1_000, max_delay_ms: 60_000 }
    }
}

impl RetryPolicy {
    pub const NEVER: Self = Self { max_attempts: 1, base_delay_ms: 0, max_delay_ms: 0 };

    /// Delay before attempt `next_attempt` (2 for the first retry), with deterministic jitter of
    /// up to −25 % so retries of a batch don't hit a provider in lockstep.
    pub fn delay_ms(&self, next_attempt: u32, jitter_seed: u64) -> u64 {
        let doublings = next_attempt.saturating_sub(2).min(62);
        let delay = self.base_delay_ms.saturating_mul(1u64 << doublings).min(self.max_delay_ms);
        let quarter = delay / 4;
        let jitter = if quarter == 0 { 0 } else { splitmix(jitter_seed ^ u64::from(next_attempt)) % (quarter + 1) };
        delay - jitter
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A job to schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    pub id: JobId,
    /// Jobs whose outputs this one needs.
    pub deps: Vec<JobId>,
    pub lane: Lane,
    pub retry: RetryPolicy,
    /// Sent to the provider with every attempt so a retry never charges twice. The planner makes
    /// it unique per run and job (`nodecraft:<run uuid>:<job>`); keep it ASCII and under 64
    /// characters, which every provider accepts.
    pub idempotency_key: String,
}

/// Concurrency limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Jobs running at once across all lanes.
    pub global: usize,
    /// Jobs running at once on a lane without its own entry in `per_lane`.
    pub default_per_lane: usize,
    pub per_lane: HashMap<Lane, usize>,
}

impl Default for Limits {
    fn default() -> Self {
        Self { global: 16, default_per_lane: 4, per_lane: HashMap::new() }
    }
}

impl Limits {
    fn lane(&self, lane: &Lane) -> usize {
        self.per_lane.get(lane).copied().unwrap_or(self.default_per_lane).max(1)
    }
}

/// Where a job is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for dependencies.
    Waiting,
    /// Dependencies done; waiting for a free slot.
    Ready,
    /// Failed transiently; retried once `until_ms` has passed.
    Backoff {
        until_ms: u64,
        attempt: u32,
    },
    Running {
        attempt: u32,
        cancel_requested: bool,
    },
    Done,
    Failed {
        error: String,
    },
    Cancelled,
    /// Not run because a dependency failed, was cancelled or was skipped.
    Skipped {
        because: JobId,
    },
}

impl JobState {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Failed { .. } | Self::Cancelled | Self::Skipped { .. })
    }
}

/// An instruction to start a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub job: JobId,
    pub lane: Lane,
    /// 1 for the first attempt.
    pub attempt: u32,
    /// The same for every attempt of this job in this run: providers pass it on so a retry
    /// after a lost reply doesn't create (and charge for) a second generation.
    pub idempotency_key: String,
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// `transient` errors (timeouts, rate limits, busy servers, network) are retried.
    Failed {
        error: String,
        transient: bool,
    },
    Cancelled,
}

/// Which jobs to cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelScope {
    All,
    /// These jobs and everything downstream of them.
    Jobs(Vec<JobId>),
}

/// Something that happened, for the queue panel and status cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Queued { job: JobId },
    Started { job: JobId, attempt: u32 },
    Retrying { job: JobId, next_attempt: u32, delay_ms: u64, error: String },
    Done { job: JobId },
    Failed { job: JobId, error: String },
    Cancelled { job: JobId },
    Skipped { job: JobId, because: JobId },
}

/// Errors from misuse of the scheduler API (never from jobs; those are [`Outcome`]s).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchedulerError {
    DuplicateJob(JobId),
    UnknownDependency {
        job: JobId,
        dep: JobId,
    },
    Cycle(JobId),
    TooManyJobs {
        limit: usize,
    },
    TooManyDeps {
        job: JobId,
        limit: usize,
    },
    UnknownJob(JobId),
    NotRunning(JobId),
    /// Only failed, cancelled and skipped jobs can be requeued.
    NotFinished(JobId),
    /// A requeued job's input failed too; requeue it in the same call.
    InputNotRequeued {
        job: JobId,
        dep: JobId,
    },
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateJob(j) => write!(f, "{j} was added twice"),
            Self::UnknownDependency { job, dep } => write!(f, "{job} depends on {dep}, which isn't scheduled"),
            Self::Cycle(j) => write!(f, "{j} depends on itself through a cycle"),
            Self::TooManyJobs { limit } => write!(f, "more than {limit} jobs"),
            Self::TooManyDeps { job, limit } => write!(f, "{job} has more than {limit} dependencies"),
            Self::UnknownJob(j) => write!(f, "{j} isn't scheduled"),
            Self::NotRunning(j) => write!(f, "{j} isn't running"),
            Self::NotFinished(j) => write!(f, "{j} hasn't failed, so it can't be retried"),
            Self::InputNotRequeued { job, dep } => write!(f, "{job} needs {dep}, which failed too; retry both"),
        }
    }
}

impl std::error::Error for SchedulerError {}

#[derive(Debug)]
struct Job {
    spec: JobSpec,
    state: JobState,
    /// Order of addition; ready jobs start in this order.
    seq: u64,
    /// Attempts started so far.
    attempts: u32,
    pending_deps: usize,
    dependents: Vec<JobId>,
}

/// Counts of jobs by state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub waiting: usize,
    pub ready: usize,
    pub backoff: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub skipped: usize,
}

impl Summary {
    pub fn unfinished(&self) -> usize {
        self.waiting + self.ready + self.backoff + self.running
    }
}

/// See the module docs.
#[derive(Debug)]
pub struct Scheduler {
    limits: Limits,
    jobs: HashMap<JobId, Job>,
    /// Ready jobs per lane, by addition order. Lanes are few (one per provider or server), so
    /// starting a job looks at each lane's head only: no scan over all jobs.
    ready: HashMap<Lane, BTreeMap<u64, JobId>>,
    /// Jobs in backoff by the time they become ready.
    backoff: BTreeSet<(u64, u64, JobId)>,
    running_total: usize,
    running_per_lane: HashMap<Lane, usize>,
    unfinished: usize,
    paused: bool,
    next_seq: u64,
    events: VecDeque<Event>,
    #[cfg(test)]
    dependency_inspections: std::cell::Cell<usize>,
}

/// Longest backoff between attempts, whatever the policy says.
pub const MAX_RETRY_DELAY_MS: u64 = 60 * 60 * 1000;

impl Scheduler {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits: Limits { global: limits.global.max(1), ..limits },
            jobs: HashMap::new(),
            ready: HashMap::new(),
            backoff: BTreeSet::new(),
            running_total: 0,
            running_per_lane: HashMap::new(),
            unfinished: 0,
            paused: false,
            next_seq: 0,
            events: VecDeque::new(),
            #[cfg(test)]
            dependency_inspections: std::cell::Cell::new(0),
        }
    }

    /// Adds jobs. Dependencies may name jobs added earlier (finished or not) or in the same call.
    /// All or nothing: on error nothing is added.
    pub fn add(&mut self, specs: Vec<JobSpec>) -> Result<(), SchedulerError> {
        if self.jobs.len().saturating_add(specs.len()) > MAX_JOBS {
            return Err(SchedulerError::TooManyJobs { limit: MAX_JOBS });
        }
        let mut new_ids = HashSet::new();
        for s in &specs {
            if self.jobs.contains_key(&s.id) || !new_ids.insert(s.id) {
                return Err(SchedulerError::DuplicateJob(s.id));
            }
            if s.deps.len() > MAX_DEPS {
                return Err(SchedulerError::TooManyDeps { job: s.id, limit: MAX_DEPS });
            }
        }
        for s in &specs {
            if let Some(dep) = s.deps.iter().find(|d| !self.jobs.contains_key(d) && !new_ids.contains(d)) {
                return Err(SchedulerError::UnknownDependency { job: s.id, dep: *dep });
            }
        }
        let topo = topological(&specs, &new_ids)?;

        // Valid. Insert in the given order (that order decides who starts first) …
        for spec in specs {
            let id = spec.id;
            let seq = self.next_seq;
            self.next_seq = self.next_seq.saturating_add(1);
            let mut deps = spec.deps.clone();
            deps.sort_unstable();
            deps.dedup();
            let mut pending = 0;
            for d in &deps {
                if new_ids.contains(d) {
                    pending += 1;
                } else if let Some(dep) = self.jobs.get_mut(d)
                    && !dep.state.is_finished()
                {
                    pending += 1;
                    dep.dependents.push(id);
                }
            }
            self.jobs.insert(id, Job { spec, state: JobState::Waiting, seq, attempts: 0, pending_deps: pending, dependents: Vec::new() });
            self.unfinished += 1;
            self.events.push_back(Event::Queued { job: id });
        }
        // … wire dependencies between new jobs …
        for id in &topo {
            let mut deps: Vec<JobId> = self.jobs.get(id).map(|j| j.spec.deps.iter().copied().filter(|d| new_ids.contains(d)).collect()).unwrap_or_default();
            deps.sort_unstable();
            deps.dedup();
            for d in deps {
                if let Some(dep) = self.jobs.get_mut(&d) {
                    dep.dependents.push(*id);
                }
            }
        }
        // … then, in dependency order, skip jobs whose input already failed and release the rest.
        for id in topo {
            self.settle(id);
        }
        Ok(())
    }

    /// For a waiting job: skip it if an input finished unsuccessfully, make it ready if all are done.
    fn settle(&mut self, id: JobId) {
        let Some(job) = self.jobs.get(&id) else { return };
        if job.state != JobState::Waiting {
            return;
        }
        let pending = job.pending_deps;
        let blocked = job
            .spec
            .deps
            .iter()
            .find(|d| {
                #[cfg(test)]
                self.dependency_inspections.set(self.dependency_inspections.get().saturating_add(1));
                self.jobs.get(d).is_some_and(|dj| dj.state.is_finished() && dj.state != JobState::Done)
            })
            .copied();
        if let Some(b) = blocked {
            self.finish(id, JobState::Skipped { because: b });
        } else if pending == 0 {
            self.set_state(id, JobState::Ready);
        }
    }

    /// Stops (`true`) or resumes starting jobs; running ones carry on. The budget check pauses the
    /// queue when spending reaches a cap.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Starts every job that can start at `now_ms`, oldest first, within the limits. A full lane
    /// doesn't hold up jobs on other lanes.
    pub fn start_ready(&mut self, now_ms: u64) -> Vec<Start> {
        while self.backoff.first().is_some_and(|(until, _, _)| *until <= now_ms) {
            // Popping first guarantees progress even if the job were missing.
            let Some((_, _, id)) = self.backoff.pop_first() else { break };
            self.set_state(id, JobState::Ready);
        }
        let mut started = Vec::new();
        if self.paused {
            return started;
        }
        while self.running_total < self.limits.global {
            // The oldest head among lanes with a free slot.
            let pick = self
                .ready
                .iter()
                .filter(|(lane, q)| !q.is_empty() && self.running_per_lane.get(*lane).copied().unwrap_or(0) < self.limits.lane(lane))
                .filter_map(|(_, q)| q.first_key_value().map(|(seq, id)| (*seq, *id)))
                .min();
            let Some((_, id)) = pick else { break };
            let Some(job) = self.jobs.get_mut(&id) else { break };
            job.attempts = job.attempts.saturating_add(1);
            let attempt = job.attempts;
            let lane = job.spec.lane.clone();
            let idempotency_key = job.spec.idempotency_key.clone();
            self.set_state(id, JobState::Running { attempt, cancel_requested: false });
            self.events.push_back(Event::Started { job: id, attempt });
            started.push(Start { job: id, lane, attempt, idempotency_key });
        }
        started
    }

    /// Records how a running job's attempt ended, at `now_ms`.
    pub fn report(&mut self, id: JobId, outcome: Outcome, now_ms: u64) -> Result<(), SchedulerError> {
        let job = self.jobs.get(&id).ok_or(SchedulerError::UnknownJob(id))?;
        let JobState::Running { attempt, cancel_requested } = job.state else {
            return Err(SchedulerError::NotRunning(id));
        };
        let retry = job.spec.retry;
        match outcome {
            Outcome::Done => {
                self.finish(id, JobState::Done);
                let dependents = self.jobs.get(&id).map(|j| j.dependents.clone()).unwrap_or_default();
                for d in dependents {
                    if let Some(dj) = self.jobs.get_mut(&d)
                        && dj.state == JobState::Waiting
                    {
                        dj.pending_deps = dj.pending_deps.saturating_sub(1);
                        // Successful inputs cannot introduce a blocked dependency. Check the
                        // full list only when the final normalized prerequisite completes.
                        if dj.pending_deps == 0 {
                            self.settle(d);
                        }
                    }
                }
            }
            Outcome::Cancelled => self.finish(id, JobState::Cancelled),
            Outcome::Failed { .. } if cancel_requested => self.finish(id, JobState::Cancelled),
            Outcome::Failed { error, transient } => {
                if transient && attempt < retry.max_attempts.max(1) {
                    let next_attempt = attempt.saturating_add(1);
                    let seed = self.jobs.get(&id).map_or(0, |j| hash_str(&j.spec.idempotency_key));
                    let delay_ms = retry.delay_ms(next_attempt, seed).min(MAX_RETRY_DELAY_MS);
                    self.set_state(id, JobState::Backoff { until_ms: now_ms.saturating_add(delay_ms), attempt: next_attempt });
                    self.events.push_back(Event::Retrying { job: id, next_attempt, delay_ms, error });
                } else {
                    self.finish(id, JobState::Failed { error });
                }
            }
        }
        Ok(())
    }

    /// Cancels jobs and everything downstream of them. Jobs that haven't started are cancelled at
    /// once; the returned running jobs must be told to stop, and end when they report back.
    pub fn cancel(&mut self, scope: CancelScope) -> Vec<JobId> {
        let roots: Vec<JobId> = match scope {
            CancelScope::All => self.jobs.iter().filter(|(_, j)| !j.state.is_finished()).map(|(id, _)| *id).collect(),
            CancelScope::Jobs(ids) => ids.into_iter().filter(|id| self.jobs.contains_key(id)).collect(),
        };
        // Everything downstream of the roots, iteratively (no recursion on hostile graphs).
        let mut targets: Vec<JobId> = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = roots;
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            targets.push(id);
            if let Some(j) = self.jobs.get(&id) {
                stack.extend(j.dependents.iter().copied());
            }
        }
        targets.sort_by_key(|id| self.jobs.get(id).map_or(u64::MAX, |j| j.seq));
        // Every target is cancelled by the user, so all of them are labelled Cancelled (none
        // Skipped); their downstream is in the target set already.
        let mut running = Vec::new();
        for id in targets {
            let Some(job) = self.jobs.get_mut(&id) else { continue };
            match &mut job.state {
                JobState::Running { cancel_requested, .. } => {
                    if !*cancel_requested {
                        *cancel_requested = true;
                        running.push(id);
                    }
                }
                s if s.is_finished() => {}
                _ => {
                    // No downstream propagation: the downstream is in `targets` and gets
                    // Cancelled itself rather than Skipped.
                    self.set_state(id, JobState::Cancelled);
                    self.events.push_back(Event::Cancelled { job: id });
                }
            }
        }
        running
    }

    /// Puts failed, cancelled and skipped jobs back in the queue (the queue panel's "retry"),
    /// with fresh attempts and a **new idempotency key** each, so the provider starts a new
    /// generation instead of replaying the failed one. Their inputs must be done, unfinished, or
    /// requeued in the same call (pass skipped downstream jobs along with the failed one).
    pub fn requeue(&mut self, jobs: &[(JobId, String)]) -> Result<(), SchedulerError> {
        let targets: HashSet<JobId> = jobs.iter().map(|(id, _)| *id).collect();
        let keys: HashMap<JobId, &String> = jobs.iter().map(|(id, k)| (*id, k)).collect();
        for id in &targets {
            let job = self.jobs.get(id).ok_or(SchedulerError::UnknownJob(*id))?;
            if !job.state.is_finished() || job.state == JobState::Done {
                return Err(SchedulerError::NotFinished(*id));
            }
            if let Some(dep) =
                job.spec.deps.iter().find(|d| !targets.contains(d) && self.jobs.get(d).is_some_and(|dj| dj.state.is_finished() && dj.state != JobState::Done))
            {
                return Err(SchedulerError::InputNotRequeued { job: *id, dep: *dep });
            }
        }
        let mut order: Vec<JobId> = targets.iter().copied().collect();
        order.sort_by_key(|id| self.jobs.get(id).map_or(u64::MAX, |j| j.seq));
        for id in &order {
            if let Some(job) = self.jobs.get_mut(id) {
                job.state = JobState::Waiting;
                job.attempts = 0;
                if let Some(key) = keys.get(id) {
                    job.spec.idempotency_key = (*key).clone();
                }
                self.unfinished += 1;
                self.events.push_back(Event::Queued { job: *id });
            }
        }
        for id in &order {
            let deps: Vec<JobId> = self.jobs.get(id).map(|j| j.spec.deps.clone()).unwrap_or_default();
            let mut pending = 0;
            let mut unique = deps;
            unique.sort_unstable();
            unique.dedup();
            for d in unique {
                if let Some(dep) = self.jobs.get_mut(&d)
                    && dep.state != JobState::Done
                {
                    pending += 1;
                    if !dep.dependents.contains(id) {
                        dep.dependents.push(*id);
                    }
                }
            }
            if let Some(job) = self.jobs.get_mut(id) {
                job.pending_deps = pending;
            }
        }
        for id in order {
            self.settle(id);
        }
        Ok(())
    }

    /// Drops finished jobs from memory (a long session's queue). Their ids can't be named as
    /// dependencies afterwards.
    pub fn forget_finished(&mut self) {
        self.jobs.retain(|_, j| !j.state.is_finished());
        // Remaining jobs must not point at forgotten ones: an id may be reused by a new job. An
        // unfinished job's finished inputs are all Done (a failed one would have skipped it), so
        // dropping them from `deps` loses nothing.
        let live: HashSet<JobId> = self.jobs.keys().copied().collect();
        for job in self.jobs.values_mut() {
            job.dependents.retain(|d| live.contains(d));
            job.spec.deps.retain(|d| live.contains(d));
        }
    }

    /// The earliest time a job in backoff becomes ready; wake the driver then.
    pub fn next_wakeup_ms(&self) -> Option<u64> {
        if self.paused { None } else { self.backoff.first().map(|(until, _, _)| *until) }
    }

    /// Events since the last call, oldest first.
    pub fn drain_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
    }

    pub fn state(&self, id: JobId) -> Option<&JobState> {
        self.jobs.get(&id).map(|j| &j.state)
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        for j in self.jobs.values() {
            match j.state {
                JobState::Waiting => s.waiting += 1,
                JobState::Ready => s.ready += 1,
                JobState::Backoff { .. } => s.backoff += 1,
                JobState::Running { .. } => s.running += 1,
                JobState::Done => s.done += 1,
                JobState::Failed { .. } => s.failed += 1,
                JobState::Cancelled => s.cancelled += 1,
                JobState::Skipped { .. } => s.skipped += 1,
            }
        }
        s
    }

    /// True when no job is waiting, ready, backing off or running.
    pub fn is_idle(&self) -> bool {
        self.unfinished == 0
    }

    /// Moves a job to `state`, keeping the ready queues, backoff set and counters in step. The
    /// only place job states change.
    fn set_state(&mut self, id: JobId, state: JobState) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        let lane = job.spec.lane.clone();
        match job.state {
            JobState::Ready => {
                if let Some(q) = self.ready.get_mut(&lane) {
                    q.remove(&job.seq);
                    if q.is_empty() {
                        self.ready.remove(&lane);
                    }
                }
            }
            JobState::Backoff { until_ms, .. } => {
                self.backoff.remove(&(until_ms, job.seq, id));
            }
            JobState::Running { .. } => {
                self.running_total = self.running_total.saturating_sub(1);
                if let Some(n) = self.running_per_lane.get_mut(&lane) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.running_per_lane.remove(&lane);
                    }
                }
            }
            _ => {}
        }
        let was_finished = job.state.is_finished();
        match state {
            JobState::Ready => {
                self.ready.entry(lane).or_default().insert(job.seq, id);
            }
            JobState::Backoff { until_ms, .. } => {
                self.backoff.insert((until_ms, job.seq, id));
            }
            JobState::Running { .. } => {
                self.running_total += 1;
                *self.running_per_lane.entry(lane).or_insert(0) += 1;
            }
            _ => {}
        }
        if state.is_finished() && !was_finished {
            self.unfinished = self.unfinished.saturating_sub(1);
        }
        job.state = state;
    }

    /// Ends a job with a final state, emits its event, and skips what can no longer run.
    fn finish(&mut self, id: JobId, state: JobState) {
        let event = match &state {
            JobState::Done => Event::Done { job: id },
            JobState::Failed { error } => Event::Failed { job: id, error: error.clone() },
            JobState::Cancelled => Event::Cancelled { job: id },
            JobState::Skipped { because } => Event::Skipped { job: id, because: *because },
            _ => return,
        };
        let successful = state == JobState::Done;
        self.set_state(id, state);
        self.events.push_back(event);
        if !successful {
            self.skip_downstream(id);
        }
    }

    /// Marks every waiting job downstream of `id` as skipped, iteratively.
    fn skip_downstream(&mut self, id: JobId) {
        let mut stack: Vec<(JobId, JobId)> = self.jobs.get(&id).map(|j| j.dependents.iter().map(|d| (*d, id)).collect()).unwrap_or_default();
        while let Some((d, because)) = stack.pop() {
            let Some(job) = self.jobs.get(&d) else { continue };
            if job.state.is_finished() || matches!(job.state, JobState::Running { .. }) {
                continue;
            }
            self.set_state(d, JobState::Skipped { because });
            self.events.push_back(Event::Skipped { job: d, because });
            if let Some(j) = self.jobs.get(&d) {
                stack.extend(j.dependents.iter().map(|x| (*x, d)));
            }
        }
    }
}

/// Kahn's algorithm over the new jobs (existing jobs can't depend on new ones, so a cycle can
/// only run through them). Returns them in dependency order.
fn topological(specs: &[JobSpec], new_ids: &HashSet<JobId>) -> Result<Vec<JobId>, SchedulerError> {
    let mut indegree: HashMap<JobId, usize> = HashMap::new();
    let mut users: HashMap<JobId, Vec<JobId>> = HashMap::new();
    for s in specs {
        let deps: HashSet<&JobId> = s.deps.iter().filter(|d| new_ids.contains(d)).collect();
        indegree.insert(s.id, deps.len());
        for d in deps {
            users.entry(*d).or_default().push(s.id);
        }
    }
    let mut queue: Vec<JobId> = specs.iter().map(|s| s.id).filter(|id| indegree.get(id) == Some(&0)).collect();
    queue.reverse();
    let mut topo = Vec::with_capacity(specs.len());
    while let Some(id) = queue.pop() {
        topo.push(id);
        for u in users.get(&id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(u) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    queue.push(*u);
                }
            }
        }
    }
    if topo.len() != specs.len() {
        let stuck = indegree.iter().filter(|(_, n)| **n > 0).map(|(id, _)| *id).min().unwrap_or(JobId(0));
        return Err(SchedulerError::Cycle(stuck));
    }
    Ok(topo)
}

fn hash_str(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64, deps: &[u64], lane: &str) -> JobSpec {
        JobSpec {
            id: JobId(id),
            deps: deps.iter().map(|d| JobId(*d)).collect(),
            lane: Lane::new(lane),
            retry: RetryPolicy::NEVER,
            idempotency_key: format!("nodecraft:run1:{id}"),
        }
    }

    fn ids(starts: &[Start]) -> Vec<u64> {
        starts.iter().map(|s| s.job.0).collect()
    }

    fn sched() -> Scheduler {
        Scheduler::new(Limits::default())
    }

    #[test]
    fn dependencies_run_in_order() {
        let mut s = sched();
        s.add(vec![job(3, &[2], "a"), job(1, &[], "a"), job(2, &[1], "a")]).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [1]);
        assert!(s.start_ready(0).is_empty());
        s.report(JobId(1), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [2]);
        s.report(JobId(2), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [3]);
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        assert!(s.is_idle());
        assert_eq!(s.summary().done, 3);
        let events = s.drain_events();
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Queued { .. })).count(), 3);
        assert_eq!(events.last(), Some(&Event::Done { job: JobId(3) }));
        assert!(s.drain_events().is_empty());
    }

    #[test]
    fn shared_fan_in_success_inspects_dependencies_only_on_final_prerequisite() {
        const ROOTS: u64 = 256;
        const SINKS: u64 = 4;
        let mut scheduler = Scheduler::new(Limits { global: ROOTS as usize, default_per_lane: ROOTS as usize, per_lane: HashMap::new() });
        // Duplicate input IDs are legal; pending counts and dependent notifications normalize them.
        let deps: Vec<u64> = (0..ROOTS).flat_map(|id| [id, id]).collect();
        let mut jobs: Vec<JobSpec> = (0..ROOTS).map(|id| job(id, &[], "roots")).collect();
        jobs.extend((ROOTS..ROOTS + SINKS).map(|id| job(id, &deps, "sinks")));
        scheduler.add(jobs).unwrap();
        assert_eq!(scheduler.start_ready(0).len(), ROOTS as usize);
        scheduler.dependency_inspections.set(0);
        for id in 0..ROOTS {
            scheduler.report(JobId(id), Outcome::Done, 0).unwrap();
            if id + 1 < ROOTS {
                assert_eq!(scheduler.summary().waiting, SINKS as usize);
                assert!(scheduler.start_ready(0).is_empty(), "fan-in cannot start early");
            }
        }
        // Count actual settle predicate inspections, excluding setup. One scan per sink,
        // linear in its stored dependency list, instead of a scan after every prerequisite.
        let inspections = scheduler.dependency_inspections.get();
        assert_eq!(inspections, deps.len() * SINKS as usize);
        assert_eq!(ids(&scheduler.start_ready(0)), (ROOTS..ROOTS + SINKS).collect::<Vec<_>>());
        for id in ROOTS..ROOTS + SINKS {
            scheduler.report(JobId(id), Outcome::Done, 0).unwrap();
        }
        assert!(scheduler.is_idle());
    }

    #[test]
    fn fan_in_waits_through_retry_then_releases_once() {
        let mut scheduler = sched();
        scheduler.add(vec![JobSpec { retry: RetryPolicy::default(), ..job(1, &[], "a") }, job(2, &[], "a"), job(3, &[1, 1, 2], "a")]).unwrap();
        assert_eq!(ids(&scheduler.start_ready(0)), [1, 2]);
        scheduler.report(JobId(2), Outcome::Done, 0).unwrap();
        scheduler.report(JobId(1), Outcome::Failed { error: "busy".into(), transient: true }, 0).unwrap();
        assert_eq!(scheduler.state(JobId(3)), Some(&JobState::Waiting));
        let wake = scheduler.next_wakeup_ms().unwrap();
        assert_eq!(ids(&scheduler.start_ready(wake)), [1]);
        scheduler.report(JobId(1), Outcome::Done, wake).unwrap();
        assert_eq!(ids(&scheduler.start_ready(wake)), [3]);
        assert!(scheduler.start_ready(wake).is_empty());
        scheduler.report(JobId(3), Outcome::Done, wake).unwrap();
        assert!(scheduler.is_idle());
        let events = scheduler.drain_events();
        assert_eq!(events.iter().filter(|event| matches!(event, Event::Started { job: JobId(3), .. })).count(), 1);
    }

    #[test]
    fn fan_in_failure_or_cancellation_still_skips_and_requeues() {
        for outcome in [Outcome::Failed { error: "blocked".into(), transient: false }, Outcome::Cancelled] {
            let mut scheduler = sched();
            scheduler.add(vec![job(1, &[], "a"), job(2, &[], "a"), job(3, &[1, 2], "a")]).unwrap();
            assert_eq!(ids(&scheduler.start_ready(0)), [1, 2]);
            scheduler.report(JobId(1), Outcome::Done, 0).unwrap();
            assert_eq!(scheduler.state(JobId(3)), Some(&JobState::Waiting));
            scheduler.report(JobId(2), outcome, 0).unwrap();
            assert_eq!(scheduler.state(JobId(3)), Some(&JobState::Skipped { because: JobId(2) }));
            assert!(scheduler.is_idle());
            scheduler.requeue(&[(JobId(2), "retry:2".into()), (JobId(3), "retry:3".into())]).unwrap();
            assert_eq!(scheduler.state(JobId(3)), Some(&JobState::Waiting));
            assert_eq!(ids(&scheduler.start_ready(0)), [2]);
            scheduler.report(JobId(2), Outcome::Done, 0).unwrap();
            assert_eq!(ids(&scheduler.start_ready(0)), [3]);
            scheduler.report(JobId(3), Outcome::Done, 0).unwrap();
            assert!(scheduler.is_idle());
        }
    }

    #[test]
    fn concurrency_limits_and_no_head_of_line_blocking() {
        let mut limits = Limits { global: 3, default_per_lane: 2, per_lane: HashMap::new() };
        limits.per_lane.insert(Lane::new("slow"), 1);
        let mut s = Scheduler::new(limits);
        s.add(vec![job(1, &[], "slow"), job(2, &[], "slow"), job(3, &[], "fast"), job(4, &[], "fast"), job(5, &[], "fast")]).unwrap();
        // slow takes one, the second slow job waits, fast fills the rest up to the global limit.
        assert_eq!(ids(&s.start_ready(0)), [1, 3, 4]);
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [5]);
        s.report(JobId(1), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [2]);
    }

    #[test]
    fn transient_failures_retry_with_backoff_and_a_stable_key() {
        let mut s = sched();
        let retry = RetryPolicy { max_attempts: 3, base_delay_ms: 1000, max_delay_ms: 60_000 };
        s.add(vec![JobSpec { retry, ..job(1, &[], "fal") }, job(2, &[1], "fal")]).unwrap();
        let first = s.start_ready(0);
        assert_eq!(first[0].attempt, 1);
        s.report(JobId(1), Outcome::Failed { error: "rate limited".into(), transient: true }, 10).unwrap();
        let wake = s.next_wakeup_ms().unwrap();
        assert!((10 + 750..=10 + 1000).contains(&wake), "{wake}");
        assert!(s.start_ready(wake - 1).is_empty(), "not before the backoff ends");
        let second = s.start_ready(wake);
        assert_eq!((second[0].attempt, &second[0].idempotency_key), (2, &first[0].idempotency_key));
        s.report(JobId(1), Outcome::Failed { error: "timeout".into(), transient: true }, wake).unwrap();
        let third = s.start_ready(s.next_wakeup_ms().unwrap());
        assert_eq!(third[0].attempt, 3);
        s.report(JobId(1), Outcome::Failed { error: "timeout".into(), transient: true }, 0).unwrap();
        assert_eq!(s.state(JobId(1)), Some(&JobState::Failed { error: "timeout".into() }), "attempts exhausted");
        assert_eq!(s.state(JobId(2)), Some(&JobState::Skipped { because: JobId(1) }));
        assert!(s.is_idle());
        let retries = s.drain_events().into_iter().filter(|e| matches!(e, Event::Retrying { .. })).count();
        assert_eq!(retries, 2);
    }

    #[test]
    fn permanent_failure_skips_downstream_but_not_siblings() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(2, &[1], "a"), job(3, &[2], "a"), job(4, &[], "a"), job(5, &[4, 3], "a")]).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [1, 4]);
        s.report(JobId(1), Outcome::Failed { error: "content blocked".into(), transient: false }, 0).unwrap();
        assert_eq!(s.state(JobId(2)), Some(&JobState::Skipped { because: JobId(1) }));
        assert_eq!(s.state(JobId(3)), Some(&JobState::Skipped { because: JobId(2) }));
        assert_eq!(s.state(JobId(5)), Some(&JobState::Skipped { because: JobId(3) }));
        s.report(JobId(4), Outcome::Done, 0).unwrap();
        assert!(s.is_idle());
        assert_eq!(s.summary(), Summary { done: 1, failed: 1, skipped: 3, ..Summary::default() });
    }

    #[test]
    fn cancellation() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(2, &[1], "a"), job(3, &[], "b"), job(4, &[], "c")]).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [1, 3, 4]);
        // Cancelling a running job asks it to stop and cancels its downstream at once.
        assert_eq!(s.cancel(CancelScope::Jobs(vec![JobId(1)])), [JobId(1)]);
        assert_eq!(s.state(JobId(2)), Some(&JobState::Cancelled));
        assert_eq!(s.cancel(CancelScope::Jobs(vec![JobId(1)])), Vec::<JobId>::new(), "asked once");
        // A failure after a cancel request is a cancellation, not a retry.
        s.report(JobId(1), Outcome::Failed { error: "aborted".into(), transient: true }, 0).unwrap();
        assert_eq!(s.state(JobId(1)), Some(&JobState::Cancelled));
        // Work that finished despite the request counts as done.
        assert_eq!(s.cancel(CancelScope::All), [JobId(3), JobId(4)]);
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        s.report(JobId(4), Outcome::Cancelled, 0).unwrap();
        assert_eq!(s.state(JobId(3)), Some(&JobState::Done));
        assert_eq!(s.state(JobId(4)), Some(&JobState::Cancelled));
        assert!(s.is_idle());
        assert!(s.cancel(CancelScope::Jobs(vec![JobId(99)])).is_empty());
    }

    #[test]
    fn cancel_all_labels_every_job_cancelled_whatever_the_order() {
        for specs in [vec![job(1, &[], "a"), job(2, &[1], "a")], vec![job(2, &[1], "a"), job(1, &[], "a")]] {
            let mut s = sched();
            s.add(specs).unwrap();
            assert!(s.cancel(CancelScope::All).is_empty());
            assert_eq!(s.state(JobId(1)), Some(&JobState::Cancelled));
            assert_eq!(s.state(JobId(2)), Some(&JobState::Cancelled));
        }
    }

    #[test]
    fn requeue_failed_and_skipped_jobs() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(2, &[1], "a"), job(3, &[], "a")]).unwrap();
        s.start_ready(0);
        s.report(JobId(1), Outcome::Failed { error: "x".into(), transient: false }, 0).unwrap();
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        let k = |id: u64| (JobId(id), format!("nodecraft:run1:{id}:retry1"));
        assert_eq!(s.requeue(&[k(2)]), Err(SchedulerError::InputNotRequeued { job: JobId(2), dep: JobId(1) }));
        assert_eq!(s.requeue(&[k(3)]), Err(SchedulerError::NotFinished(JobId(3))), "done jobs aren't retried");
        s.requeue(&[k(2), k(1)]).unwrap();
        assert_eq!(s.state(JobId(2)), Some(&JobState::Waiting));
        let st = s.start_ready(0);
        assert_eq!((ids(&st), st[0].attempt), (vec![1], 1), "fresh attempts");
        assert_eq!(st[0].idempotency_key, "nodecraft:run1:1:retry1", "a new key, so the provider starts afresh");
        s.report(JobId(1), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [2]);
        s.report(JobId(2), Outcome::Done, 0).unwrap();
        assert!(s.is_idle());
    }

    #[test]
    fn pause_and_forget() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(2, &[], "a")]).unwrap();
        s.set_paused(true);
        assert!(s.start_ready(0).is_empty());
        s.set_paused(false);
        assert_eq!(ids(&s.start_ready(0)), [1, 2]);
        s.report(JobId(1), Outcome::Done, 0).unwrap();
        s.forget_finished();
        assert_eq!(s.state(JobId(1)), None);
        assert!(s.state(JobId(2)).is_some());
        assert!(matches!(s.add(vec![job(3, &[1], "a")]), Err(SchedulerError::UnknownDependency { .. })));
    }

    #[test]
    fn reusing_a_forgotten_id_is_safe() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(3, &[], "b"), job(2, &[1], "a")]).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [1, 3]);
        s.cancel(CancelScope::Jobs(vec![JobId(2)]));
        s.forget_finished();
        s.add(vec![job(4, &[], "c"), job(2, &[4], "a")]).unwrap();
        // The old upstream (1) finishing or failing must not touch the new job 2.
        s.report(JobId(1), Outcome::Failed { error: "x".into(), transient: false }, 0).unwrap();
        assert_eq!(s.state(JobId(2)), Some(&JobState::Waiting));
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [4]);
        s.report(JobId(4), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [2], "runs only after its own input");
    }

    #[test]
    fn reusing_the_id_of_a_forgotten_done_input_is_safe() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(3, &[], "b"), job(2, &[1, 3], "c")]).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [1, 3]);
        s.report(JobId(1), Outcome::Done, 0).unwrap();
        s.forget_finished();
        s.add(vec![job(1, &[], "a")]).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [1]);
        s.report(JobId(1), Outcome::Failed { error: "unrelated".into(), transient: false }, 0).unwrap();
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [2], "both real inputs are done; the new job 1 is unrelated");
    }

    /// 100k independent jobs through the driver's loop shape: must stay roughly linear.
    #[test]
    fn scales_to_many_jobs() {
        let n = 100_000u64;
        let mut s = sched();
        s.add((0..n).map(|i| job(i, &[], if i % 2 == 0 { "a" } else { "b" })).collect()).unwrap();
        let started_at = std::time::Instant::now();
        let mut running = Vec::new();
        let mut done = 0;
        while !s.is_idle() {
            running.extend(s.start_ready(0));
            let _ = s.next_wakeup_ms();
            if let Some(r) = running.pop() {
                s.report(r.job, Outcome::Done, 0).unwrap();
                done += 1;
            }
            s.drain_events();
        }
        assert_eq!(done, n);
        // Generous for debug builds on slow CI; the quadratic version took minutes.
        assert!(started_at.elapsed() < std::time::Duration::from_secs(30), "{:?}", started_at.elapsed());
    }

    #[test]
    fn cancelling_a_job_in_backoff() {
        let mut s = sched();
        s.add(vec![JobSpec { retry: RetryPolicy::default(), ..job(1, &[], "a") }]).unwrap();
        s.start_ready(0);
        s.report(JobId(1), Outcome::Failed { error: "busy".into(), transient: true }, 0).unwrap();
        assert!(s.cancel(CancelScope::All).is_empty());
        assert_eq!(s.state(JobId(1)), Some(&JobState::Cancelled));
        assert!(s.start_ready(u64::MAX).is_empty());
        assert_eq!(s.next_wakeup_ms(), None);
    }

    #[test]
    fn invalid_additions_change_nothing() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a")]).unwrap();
        for bad in [
            vec![job(1, &[], "a")],
            vec![job(2, &[], "a"), job(2, &[], "a")],
            vec![job(2, &[7], "a")],
            vec![job(2, &[2], "a")],
            vec![job(2, &[3], "a"), job(3, &[2], "a")],
            vec![job(2, &[1], "a"), job(3, &[4], "a"), job(4, &[5], "a"), job(5, &[3], "a")],
        ] {
            assert!(s.add(bad.clone()).is_err(), "{bad:?}");
            assert_eq!(s.summary().unfinished(), 1);
        }
        assert_eq!(s.add(vec![job(2, &[2], "a")]), Err(SchedulerError::Cycle(JobId(2))));
        let too_many = JobSpec { deps: vec![JobId(1); MAX_DEPS + 1], ..job(9, &[], "a") };
        assert!(matches!(s.add(vec![too_many]), Err(SchedulerError::TooManyDeps { .. })));
        assert_eq!(s.report(JobId(1), Outcome::Done, 0), Err(SchedulerError::NotRunning(JobId(1))));
        assert_eq!(s.report(JobId(42), Outcome::Done, 0), Err(SchedulerError::UnknownJob(JobId(42))));
    }

    #[test]
    fn later_additions_see_finished_jobs() {
        let mut s = sched();
        s.add(vec![job(1, &[], "a"), job(2, &[], "a")]).unwrap();
        s.start_ready(0);
        s.report(JobId(1), Outcome::Done, 0).unwrap();
        s.report(JobId(2), Outcome::Failed { error: "x".into(), transient: false }, 0).unwrap();
        s.add(vec![job(3, &[1], "a"), job(4, &[2], "a"), job(5, &[4], "a"), job(6, &[3], "a")]).unwrap();
        assert_eq!(s.state(JobId(4)), Some(&JobState::Skipped { because: JobId(2) }));
        assert_eq!(s.state(JobId(5)), Some(&JobState::Skipped { because: JobId(4) }));
        assert_eq!(ids(&s.start_ready(0)), [3]);
        s.report(JobId(3), Outcome::Done, 0).unwrap();
        assert_eq!(ids(&s.start_ready(0)), [6]);
    }

    #[test]
    fn backoff_delays() {
        let p = RetryPolicy { max_attempts: 10, base_delay_ms: 1000, max_delay_ms: 8000 };
        for (attempt, full) in [(2, 1000), (3, 2000), (4, 4000), (5, 8000), (6, 8000)] {
            let d = p.delay_ms(attempt, 7);
            assert!((full * 3 / 4..=full).contains(&d), "attempt {attempt}: {d}");
        }
        let huge = RetryPolicy { max_attempts: u32::MAX, base_delay_ms: u64::MAX, max_delay_ms: u64::MAX };
        assert!(huge.delay_ms(u32::MAX, 1) > 0);
        assert_eq!(RetryPolicy::NEVER.delay_ms(2, 1), 0);
    }

    /// Random DAGs, limits and outcomes, checking the invariants after every step.
    #[test]
    fn random_runs_keep_the_invariants() {
        let mut seed = 0x1234_5678_9abc_def1_u64;
        let mut rnd = move |n: u64| {
            seed = splitmix(seed);
            seed % n.max(1)
        };
        for _ in 0..300 {
            let n = 1 + rnd(40);
            let lanes = ["a", "b", "c"];
            let mut limits = Limits { global: 1 + rnd(5) as usize, default_per_lane: 1 + rnd(3) as usize, per_lane: HashMap::new() };
            limits.per_lane.insert(Lane::new("a"), 1);
            let specs: Vec<JobSpec> = (0..n)
                .map(|i| {
                    let deps: Vec<JobId> = (0..i).filter(|_| rnd(5) == 0).map(JobId).collect();
                    let retry = RetryPolicy { max_attempts: 1 + rnd(3) as u32, base_delay_ms: rnd(50), max_delay_ms: 100 };
                    JobSpec { id: JobId(i), deps, lane: Lane::new(lanes[rnd(3) as usize]), retry, idempotency_key: format!("k{i}") }
                })
                .collect();
            let mut s = Scheduler::new(limits.clone());
            // Half the jobs up front, the rest while the first ones run.
            let split = specs.len() / 2;
            s.add(specs[..split].to_vec()).unwrap();
            let mut late = Some(specs[split..].to_vec());
            let mut now = 0u64;
            let mut running: Vec<Start> = Vec::new();
            let mut keys: HashMap<JobId, String> = HashMap::new();
            let mut steps = 0;
            while !s.is_idle() || late.is_some() {
                steps += 1;
                assert!(steps < 10_000, "no progress");
                for st in s.start_ready(now) {
                    // Dependencies are done before a job starts.
                    let spec = &specs[st.job.0 as usize];
                    assert!(spec.deps.iter().all(|d| s.state(*d) == Some(&JobState::Done)));
                    assert_eq!(keys.entry(st.job).or_insert_with(|| st.idempotency_key.clone()), &st.idempotency_key);
                    running.push(st);
                }
                // Limits hold.
                assert!(running.len() <= limits.global);
                for lane in lanes {
                    let on = running.iter().filter(|r| r.lane.0 == lane).count();
                    assert!(on <= limits.lane(&Lane::new(lane)), "lane {lane}: {on}");
                }
                if steps == 3
                    && let Some(rest) = late.take()
                {
                    s.add(rest).unwrap();
                }
                match rnd(12) {
                    0 => {
                        let target = JobId(rnd(n));
                        for id in s.cancel(CancelScope::Jobs(vec![target])) {
                            assert!(running.iter().any(|r| r.job == id));
                        }
                    }
                    1 if rnd(10) == 0 => {
                        for id in s.cancel(CancelScope::All) {
                            assert!(running.iter().any(|r| r.job == id));
                        }
                    }
                    _ if !running.is_empty() => {
                        let r = running.swap_remove(rnd(running.len() as u64) as usize);
                        let outcome = match rnd(6) {
                            0 => Outcome::Failed { error: "x".into(), transient: rnd(2) == 0 },
                            1 => Outcome::Cancelled,
                            _ => Outcome::Done,
                        };
                        s.report(r.job, outcome, now).unwrap();
                    }
                    _ => now = s.next_wakeup_ms().unwrap_or(now).max(now + 1),
                }
            }
            assert!(running.is_empty());
            assert_eq!(s.summary().unfinished(), 0, "the idle counter agrees with the states");
            // Every job ended exactly once; skipped jobs have an unsuccessful dependency.
            let events = s.drain_events();
            for spec in &specs {
                let ends = events
                    .iter()
                    .filter(|e| match e {
                        Event::Done { job } | Event::Failed { job, .. } | Event::Cancelled { job } | Event::Skipped { job, .. } => *job == spec.id,
                        _ => false,
                    })
                    .count();
                assert_eq!(ends, 1, "{:?}", spec.id);
                if let Some(JobState::Skipped { because }) = s.state(spec.id) {
                    assert!(spec.deps.contains(because));
                    assert_ne!(s.state(*because), Some(&JobState::Done));
                }
            }
        }
    }
}
