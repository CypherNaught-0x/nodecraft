use super::{
    bounds::{self, Check, MIB, Ownership},
    provider::{AdapterFixture, Gated},
    runtime::{Fault, ManualRuntime},
};
use nodecraft_engine::{
    Session, generate,
    jobs::{CompletionAccounting, CompletionDelivery, JobBudget, JobPrice, ProviderRunner},
    provider_routing::{ProviderRouting, RegisteredProvider},
    run_cmds::{AuthorizedRun, BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{AdmissionLimits, CancelToken, DriverError, Handle, JobRunner, Runtime, TaskFuture, driver_with_admission},
    run::{Progress, RunLimits, RunOptions, SourceRunner, StepResult, take_outputs},
    scheduler::{self, CancelScope, Event, JobId, Outcome, RetryPolicy, Scheduler, Start},
};
use nodecraft_nodes::JobSpec;
use nodecraft_providers::{JobHandle, JobProgress, RunUpdate};
use nodecraft_types::{CanvasId, Money, NodeId};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
};
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityChange {
    None,
    Source,
    Catalog,
    Budget,
    Routing,
}
/// Command source contains explicit consent; quote tokens are obtained only from Session.
pub struct DriverRequest {
    pub canvas: CanvasId,
    pub nodes: Vec<NodeId>,
    pub mode: &'static str,
    pub amounts: BTreeMap<String, u64>,
    pub partial: bool,
    pub options: RunOptions,
}
impl DriverRequest {
    pub fn selected(canvas: CanvasId, nodes: Vec<NodeId>) -> Self {
        Self {
            canvas,
            nodes,
            mode: "with_upstream",
            amounts: BTreeMap::new(),
            partial: true,
            options: RunOptions {
                id: [0x70; 16],
                first_job: 1,
                limits: RunLimits::default(),
                retry: RetryPolicy { max_attempts: 4, base_delay_ms: 1, max_delay_ms: 2 },
            },
        }
    }
}
#[derive(Clone, Debug)]
pub enum Trigger {
    At(u64),
    Event(JobId, &'static str),
    Update(JobId, &'static str),
    Epoch(JobId, usize, &'static str),
}
#[derive(Debug)]
pub enum ActionKind {
    SubmitGate(usize),
    PollGate(usize),
    Pause(bool),
    Cancel(Vec<JobId>),
    Requeue {
        job: JobId,
        key: String,
        replacement: Option<usize>,
    },
    Replace {
        job: JobId,
        adapter: usize,
    },
    Restore {
        job: JobId,
        adapter: usize,
        base: String,
    },
    Forget,
    /// Deliberate completion/retry-timer/cancel readiness tie; never changes Driver selection.
    ReadyRace {
        adapter: usize,
        completion: JobId,
        retrying: JobId,
    },
    ReplaceLedger,
    RestoreLedger,
    Repair {
        delivery: usize,
        wrong_ledger: bool,
    },
}
#[derive(Debug)]
pub struct Action {
    pub trigger: Trigger,
    pub kind: ActionKind,
}
/// Consumed synthetic descriptions; none of these fields can supply a live network adapter.
pub struct DriverFixture {
    pub catalog: Option<nodecraft_catalog::Catalog>,
    pub adapters: Vec<AdapterFixture>,
    pub selections: Vec<(NodeId, usize)>,
    pub actions: Vec<Action>,
    pub budgets: Budgets,
    pub scheduler: scheduler::Limits,
    pub provider_limits: nodecraft_providers::RunLimits,
    pub admission: AdmissionLimits,
    pub before_authorize: AuthorityChange,
    pub replace_routing_after_authorize: bool,
    pub record: bool,
    pub invalid_record: bool,
    pub conversion_cap: usize,
    pub record_cap: usize,
    pub original_cap: usize,
    pub epoch_ceiling: usize,
}
impl Default for DriverFixture {
    fn default() -> Self {
        Self {
            catalog: None,
            adapters: Vec::new(),
            selections: Vec::new(),
            actions: Vec::new(),
            budgets: Budgets::default(),
            scheduler: Default::default(),
            provider_limits: Default::default(),
            admission: Default::default(),
            before_authorize: AuthorityChange::None,
            replace_routing_after_authorize: false,
            record: true,
            invalid_record: false,
            conversion_cap: 16 * MIB,
            record_cap: MIB,
            original_cap: 8 * MIB,
            epoch_ceiling: 64,
        }
    }
}
#[derive(Debug)]
pub struct Trace {
    pub job: JobId,
    pub epoch: usize,
    pub kind: &'static str,
    pub attempt: u32,
    pub key: Option<String>,
    pub target: Option<nodecraft_types::ProviderTarget>,
    pub generation: u32,
    pub handle: Option<JobHandle>,
}
#[derive(Debug)]
pub struct RetainedDelivery {
    pub job: JobId,
    pub epoch: usize,
    pub attempt: u32,
    pub delivery: CompletionDelivery,
    pub disposition: &'static str,
}
#[derive(Debug)]
pub struct JobReport {
    pub job: JobId,
    pub epoch: usize,
    pub provider: bool,
    pub starts: Vec<u32>,
    pub terminal: Option<&'static str>,
    pub notified: bool,
}
/// Never Clone: Session, original paid output and accounting-only repair stay independently owned.
pub struct DriverOutcome {
    pub session: Session,
    pub trace: Vec<Trace>,
    pub jobs: Vec<JobReport>,
    pub deliveries: Vec<RetainedDelivery>,
    pub results: Vec<StepResult>,
    pub failures: Vec<(NodeId, String)>,
    pub budget: JobBudget,
    pub previous_ledgers: Vec<UsageLog>,
    pub adapters: Vec<Arc<Gated>>,
    pub scheduler: Option<Scheduler>,
    pub recorded: bool,
    pub record_dispatches: usize,
    pub cleanup_complete: bool,
    pub peak_global: usize,
    pub peak_lanes: BTreeMap<String, usize>,
    pub ownership: Ownership,
    pub quote_jobs: Vec<(NodeId, u64)>,
}
pub struct HarnessError {
    pub code: &'static str,
    pub outcome: Box<DriverOutcome>,
}
impl std::fmt::Debug for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessError").field("code", &self.code).finish()
    }
}
impl DriverOutcome {
    /// A per-epoch causal projection. Independent jobs are never sorted into a fabricated total order.
    pub fn semantic_report(&mut self) -> Check<serde_json::Value> {
        let mut n = 512usize;
        for t in &self.trace {
            let strings =
                t.key.as_ref().map_or(0, String::len).checked_add(t.target.as_ref().map_or(0, |t| t.adapter().len() + 64)).ok_or("report_overflow")?;
            n = n.checked_add(4096).and_then(|n| strings.checked_mul(6).and_then(|s| n.checked_add(s))).ok_or("report_overflow")?;
        }
        for j in &self.jobs {
            n = n.checked_add(1024).and_then(|n| j.starts.len().checked_mul(128).and_then(|s| n.checked_add(s))).ok_or("report_overflow")?;
        }
        let log = lock(&self.budget.log);
        let mut entries = 0usize;
        for e in log.entries() {
            entries += 1;
            if entries > 1024 {
                return Err("report_ledger_count");
            }
            let strings = e.provider.len().checked_add(e.model.len()).and_then(|n| n.checked_add(e.idempotency_key.len())).ok_or("report_overflow")?;
            n = n.checked_add(4096).and_then(|n| strings.checked_mul(6).and_then(|s| n.checked_add(s))).ok_or("report_overflow")?;
        }
        n = n
            .checked_add(self.adapters.len().checked_mul(1024).ok_or("report_overflow")?)
            .and_then(|n| self.deliveries.len().checked_mul(4096).and_then(|s| n.checked_add(s)))
            .ok_or("report_overflow")?;
        Ownership::charge(&mut self.ownership.serialized, n, 16 * MIB)?;
        let rows:Vec<_>=self.jobs.iter().map(|j|{
   let transitions:Vec<_>=self.trace.iter().filter(|t|t.job==j.job&&t.epoch==j.epoch&&matches!(t.kind,"queued"|"started"|"retrying"|"done"|"failed"|"cancelled"|"skipped")).map(|t|json!({"kind":t.kind,"attempt":t.attempt})).collect();
   let updates:Vec<_>=self.trace.iter().filter(|t|t.job==j.job&&t.epoch==j.epoch&&matches!(t.kind,"submitted"|"partial"|"poll_retry")).map(|t|json!({"kind":t.kind,"attempt":t.attempt,"key":t.key,"adapter":t.target.as_ref().map(|t|t.adapter()),"instance":t.target.as_ref().and_then(|t|t.instance()),"generation":t.generation})).collect();
   json!({"job":j.job.0,"epoch":j.epoch,"provider":j.provider,"attempts":j.starts,"terminal":j.terminal,"notified":j.notified,"accepted":self.trace.iter().any(|t|t.job==j.job&&t.epoch==j.epoch&&t.kind=="accepted"),"rejected":self.trace.iter().any(|t|t.job==j.job&&t.epoch==j.epoch&&t.kind=="rejected"),"transitions":transitions,"updates":updates})
  }).collect();
        let calls: Vec<_> = self
            .adapters
            .iter()
            .map(|a| {
                let c = a.counters();
                json!({"submit":c.submits,"poll":c.polls,"cancel":c.cancels,"synthetic":a.synthetic})
            })
            .collect();
        let accounting: Vec<_> = self
            .deliveries
            .iter()
            .map(|d| match &d.delivery.accounting {
                CompletionAccounting::NotBudgeted => "not_budgeted",
                CompletionAccounting::Recorded => "recorded",
                CompletionAccounting::NoReportedCost => "no_reported_cost",
                CompletionAccounting::Failed(_) => "failed",
            })
            .collect();
        let deliveries: Vec<_> = self
            .deliveries
            .iter()
            .map(|d| {
                let shapes: Vec<_> = d
                    .delivery
                    .completed
                    .outputs
                    .iter()
                    .map(|o| match o {
                        nodecraft_providers::Output::Text(s) => json!({"type":"text","bytes":s.len()}),
                        nodecraft_providers::Output::Bytes { mime, bytes } => json!({"type":"bytes","mime":mime,"bytes":bytes.len()}),
                        nodecraft_providers::Output::Url { .. } => json!({"type":"unexpected_url"}),
                    })
                    .collect();
                json!({"job":d.job.0,"epoch":d.epoch,"attempt":d.attempt,"disposition":d.disposition,"outputs":shapes})
            })
            .collect();
        let ledger: Vec<_> = log
            .entries()
            .map(|e| json!({"node":e.node,"provider":e.provider,"instance":e.instance,"key":e.idempotency_key,"model":e.model,"cost":e.cost,"status":e.status}))
            .collect();
        Ok(
            json!({"deliveries":deliveries,"ledger":ledger,"jobs":rows,"recorded":self.recorded,"results":self.results.len(),"retained":self.deliveries.len(),"peak":self.peak_global,"cleanup":self.cleanup_complete,"calls":calls,"accounting":accounting}),
        )
    }
}
#[derive(Clone, Copy)]
struct Active {
    job: JobId,
    epoch: usize,
    attempt: u32,
}
struct Shared {
    source_jobs: BTreeSet<JobId>,
    current: Option<Active>,
    epochs: BTreeMap<JobId, usize>,
    queued_owners: VecDeque<(JobId, usize)>,
    running: BTreeMap<JobId, Active>,
    messages: VecDeque<Message>,
    deliveries: Vec<RetainedDelivery>,
    events: usize,
    updates: usize,
    callback_bytes: usize,
}
struct Message {
    job: JobId,
    epoch: usize,
    kind: &'static str,
    attempt: u32,
    key: Option<String>,
    target: Option<nodecraft_types::ProviderTarget>,
    generation: u32,
    handle: Option<JobHandle>,
}
struct PollScope {
    shared: Arc<Mutex<Shared>>,
    previous: Option<Active>,
}
impl Drop for PollScope {
    fn drop(&mut self) {
        lock(&self.shared).current = self.previous;
    }
}
struct Tagged {
    future: TaskFuture<Outcome>,
    shared: Arc<Mutex<Shared>>,
    active: Active,
}
impl Future for Tagged {
    type Output = Outcome;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Outcome> {
        let previous = lock(&self.shared).current.replace(self.active);
        let _scope = PollScope { shared: self.shared.clone(), previous };
        self.future.as_mut().poll(cx)
    }
}
struct Composite {
    provider: Arc<ProviderRunner>,
    sources: SourceRunner,
    shared: Arc<Mutex<Shared>>,
}
impl JobRunner for Composite {
    fn run(&self, start: Start, cancel: CancelToken) -> TaskFuture<Outcome> {
        let active = {
            let mut s = lock(&self.shared);
            let active = Active { job: start.job, epoch: s.epochs.get(&start.job).copied().unwrap_or(usize::MAX), attempt: start.attempt };
            s.running.insert(start.job, active);
            active
        };
        let source = lock(&self.shared).source_jobs.contains(&start.job);
        let future = if source { self.sources.run(start, cancel) } else { self.provider.run(start, cancel) };
        Box::pin(Tagged { future, shared: self.shared.clone(), active })
    }
}
type AckFuture = TaskFuture<Result<Result<(), scheduler::SchedulerError>, DriverError>>;
struct Ack {
    future: AckFuture,
    batch: Vec<(JobId, usize)>,
}
struct Controller {
    out: DriverOutcome,
    run: Option<AuthorizedRun>,
    runtime: Arc<ManualRuntime>,
    runner: Arc<ProviderRunner>,
    sources: SourceRunner,
    shared: Arc<Mutex<Shared>>,
    handle: Option<Handle>,
    end: Arc<Mutex<Option<Scheduler>>>,
    acks: Vec<Ack>,
    actions: Vec<Option<Action>>,
    bindings: Vec<RegisteredProvider>,
    prices: BTreeMap<NodeId, Option<JobPrice>>,
    notified: BTreeSet<JobId>,
    requests: BTreeMap<JobId, nodecraft_engine::jobs::QueuedJob>,
    running: BTreeMap<JobId, String>,
    fixture: DriverFixture,
    request: DriverRequest,
    fanout: usize,
    fault: Fault,
    emissions: Arc<Mutex<super::synthetic::Emissions>>,
    calls: Arc<Mutex<super::provider::Calls>>,
}

fn event_parts(e: Event) -> (JobId, &'static str, u32) {
    match e {
        Event::Queued { job } => (job, "queued", 0),
        Event::Started { job, attempt } => (job, "started", attempt),
        Event::Retrying { job, next_attempt, .. } => (job, "retrying", next_attempt),
        Event::Done { job } => (job, "done", 0),
        Event::Failed { job, .. } => (job, "failed", 0),
        Event::Cancelled { job } => (job, "cancelled", 0),
        Event::Skipped { job, .. } => (job, "skipped", 0),
    }
}
fn event_message(shared: &Arc<Mutex<Shared>>, fault: &Fault, event: Event) {
    let (job, kind, attempt) = event_parts(event);
    let mut s = lock(shared);
    s.events += 1;
    if s.events > 4096 || s.messages.len() >= 256 {
        fault.set("event_capacity");
        return;
    }
    let epoch = if kind == "queued" {
        let owner = s.queued_owners.iter().position(|(j, _)| *j == job).and_then(|i| s.queued_owners.remove(i)).map(|(_, epoch)| epoch);
        if let Some(epoch) = owner {
            s.epochs.insert(job, epoch);
        }
        owner
    } else if kind == "started" {
        s.epochs.get(&job).copied()
    } else {
        s.running.get(&job).map(|a| a.epoch).or_else(|| s.epochs.get(&job).copied())
    }
    .unwrap_or(usize::MAX);
    s.messages.push_back(Message { job, epoch, kind, attempt, key: None, target: None, generation: 0, handle: None });
}
impl Controller {
    fn new(session: Session, request: DriverRequest, fixture: DriverFixture) -> Self {
        let runtime = Arc::new(ManualRuntime::default());
        let fault = runtime.fault.clone();
        let shared = Arc::new(Mutex::new(Shared {
            source_jobs: BTreeSet::new(),
            current: None,
            epochs: BTreeMap::new(),
            queued_owners: VecDeque::with_capacity(128),
            running: BTreeMap::new(),
            messages: VecDeque::with_capacity(256),
            deliveries: Vec::with_capacity(1024),
            events: 0,
            updates: 0,
            callback_bytes: 0,
        }));
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(fixture.budgets.clone())),
            session: "driver-fixture".into(),
            wall_clock_ms: Arc::new(|| 0),
        };
        let sink = shared.clone();
        let failed = fault.clone();
        let updates = shared.clone();
        let failed_updates = fault.clone();
        let runner = Arc::new(
            ProviderRunner::new(
                runtime.clone(),
                fixture.provider_limits,
                Arc::new(move |job, delivery| {
                    let mut s = lock(&sink);
                    let a = s.current.unwrap_or(Active { job, epoch: usize::MAX, attempt: 0 });
                    if a.job != job || a.epoch >= 128 || a.attempt > 8 {
                        failed.set("delivery_identity");
                    }
                    if bounds::completion(&delivery.completed, &[]).is_err() {
                        failed.set("completion_payload");
                    }
                    if s.deliveries.len() >= 1024 {
                        failed.set("delivery_capacity");
                    }
                    s.deliveries.push(RetainedDelivery { job, epoch: a.epoch, attempt: a.attempt, delivery, disposition: "unprocessed" });
                }),
                Arc::new(move |job, sent, update| {
                    let mut s = lock(&updates);
                    s.updates += 1;
                    let a = s.current.unwrap_or(Active { job, epoch: usize::MAX, attempt: 0 });
                    let (kind, handle, text) = match update {
                        RunUpdate::Submitted(h) => {
                            let n = h.remote_id.len().saturating_add(bounds::json(&h.data, 0).map_or(usize::MAX, |c| c.owned));
                            ("submitted", Some(h), n)
                        }
                        RunUpdate::PollRetry(e) => ("poll_retry", None, e.message.len()),
                        RunUpdate::Progress(JobProgress::Running { message, partial, .. }) => {
                            let n = message.as_ref().map_or(0, String::len)
                                + partial.as_ref().map_or(0, |p| match p {
                                    nodecraft_providers::Output::Text(t) => t.len(),
                                    nodecraft_providers::Output::Bytes { bytes, .. } => bytes.len(),
                                    nodecraft_providers::Output::Url { .. } => usize::MAX,
                                });
                            (if partial.is_some() { "partial" } else { "progress" }, None, n)
                        }
                        _ => ("progress", None, 0),
                    };
                    let bytes = sent.key.len().checked_add(sent.target.adapter().len()).and_then(|n| n.checked_add(text)).and_then(|n| n.checked_add(256));
                    if text > 1024
                        || sent.key.len() > 1024
                        || s.updates > 32768
                        || s.messages.len() >= 256
                        || bytes.is_none_or(|n| s.callback_bytes.checked_add(n).is_none_or(|n| n > 4 * MIB))
                    {
                        failed_updates.set("callback_capacity");
                        return;
                    }
                    s.callback_bytes += bytes.unwrap_or(0);
                    s.messages.push_back(Message {
                        job,
                        epoch: a.epoch,
                        kind,
                        attempt: a.attempt,
                        key: Some(sent.key.to_owned()),
                        target: Some(sent.target.clone()),
                        generation: sent.generation,
                        handle,
                    });
                }),
            )
            .with_budget(budget.clone()),
        );
        let sources = SourceRunner::default();
        Self {
            out: DriverOutcome {
                session,
                trace: Vec::with_capacity(4096),
                jobs: Vec::with_capacity(128),
                deliveries: Vec::with_capacity(1024),
                results: Vec::with_capacity(64),
                failures: Vec::with_capacity(64),
                budget,
                previous_ledgers: Vec::with_capacity(32),
                adapters: Vec::with_capacity(64),
                scheduler: None,
                recorded: false,
                record_dispatches: 0,
                cleanup_complete: false,
                peak_global: 0,
                peak_lanes: BTreeMap::new(),
                ownership: Ownership { admin: 5 * MIB / 2, ..Default::default() },
                quote_jobs: Vec::with_capacity(64),
            },
            run: None,
            runtime,
            runner,
            sources,
            shared,
            handle: None,
            end: Arc::new(Mutex::new(None)),
            acks: Vec::with_capacity(128),
            actions: Vec::with_capacity(32),
            bindings: Vec::with_capacity(64),
            prices: BTreeMap::new(),
            notified: BTreeSet::new(),
            requests: BTreeMap::new(),
            running: BTreeMap::new(),
            fixture,
            request,
            fanout: 0,
            fault,
            emissions: Arc::default(),
            calls: Arc::default(),
        }
    }
    fn initialize(&mut self) -> Check<Progress> {
        if self.request.nodes.len() > 64
            || self.fixture.adapters.len() > 64
            || self.fixture.actions.len() > 32
            || self.fixture.conversion_cap > 16 * MIB
            || self.fixture.record_cap > MIB
            || self.request.options.retry.max_attempts > 8
        {
            return Err("fixture_limits");
        }
        if self.fixture.epoch_ceiling == 0 || self.fixture.epoch_ceiling > 128 || self.request.options.retry.max_attempts == 0 {
            return Err("fixture_limits");
        }
        if self.request.amounts.len() > 16 || self.request.amounts.keys().any(|k| k.len() > 8) || self.fixture.selections.len() > 128 {
            return Err("fixture_limits");
        }
        for action in &self.fixture.actions {
            match &action.kind {
                ActionKind::Cancel(jobs) if jobs.len() > 128 => return Err("action_limit"),
                ActionKind::Requeue { key, .. } if key.len() > 64 => return Err("action_limit"),
                ActionKind::Restore { base, .. } if base.len() > 64 => return Err("action_limit"),
                _ => {}
            }
            if matches!(action.trigger,Trigger::At(t) if t>60_000) {
                return Err("action_deadline");
            }
        }
        let canvas = self.out.session.document().canvases().get(&self.request.canvas).ok_or("canvas")?;
        if canvas.nodes().len() > 64 || canvas.edges().len() > 128 {
            return Err("fixture_graph");
        }
        self.fanout = canvas.edges().len().checked_add(1).and_then(|n| n.checked_mul(4)).ok_or("fixture_graph")?;
        let mut payload = 0usize;
        for n in canvas.nodes().values() {
            payload = payload.checked_add(1024 + n.kind.len() + n.title.len()).ok_or("fixture_payload")?;
            for port in n.inputs.iter().chain(n.outputs.iter()) {
                payload = payload.checked_add(128 + port.name.len()).ok_or("fixture_payload")?;
            }
            for generation in &n.history {
                payload = payload.checked_add(512).ok_or("fixture_payload")?;
                if payload > 8 * MIB {
                    return Err("fixture_payload");
                }
                for v in &generation.outputs {
                    payload = payload.checked_add(bounds::value(v, 0)?.owned).ok_or("fixture_payload")?;
                }
            }
            for (key, v) in n.params.iter().chain(n.config.iter()) {
                payload = payload.checked_add(128 + key.len()).ok_or("fixture_payload")?;
                let c = bounds::value(v, 0)?;
                payload = payload.checked_add(c.owned).ok_or("fixture_payload")?;
                if payload > 8 * MIB {
                    return Err("fixture_payload");
                }
            }
            if n.title.len() > 1024 {
                return Err("fixture_title");
            }
        }
        if payload > 8 * MIB {
            return Err("fixture_payload");
        }
        // AdmittedRunPlan owns a whole Canvas snapshot, including comments/fields/tags.
        // Count that borrowed shape, not merely selected params, before quote/prepare copies.
        let snapshot = bounds::canvas(canvas, (16 * MIB) / self.fanout.max(1))?;
        let initial = snapshot.checked_mul(self.fanout).ok_or("run_ownership")?;
        Ownership::charge(&mut self.out.ownership.run, initial, 16 * MIB)?;
        let catalogs = if self.fixture.before_authorize == AuthorityChange::Catalog { 2 } else { 1 };
        if self.fixture.catalog.is_none() {
            Ownership::charge(&mut self.out.ownership.admin, 128 * 1024 * catalogs, 4 * MIB)?;
        }
        let mut routing = ProviderRouting::default();
        for a in self.fixture.adapters.drain(..) {
            let instance = a.instance;
            let g = a.build(self.fault.clone(), self.emissions.clone(), self.calls.clone())?;
            let binding = match instance {
                Some(id) => routing.bind_named(g.clone(), id),
                None => routing.bind_default(g.clone()),
            }
            .map_err(|_| "binding")?;
            self.bindings.push(binding);
            self.out.adapters.push(g);
        }
        for (node, index) in &self.fixture.selections {
            let b = self.bindings.get(*index).ok_or("selection")?;
            routing.select(*node, b.target().adapter(), b.target()).map_err(|_| "selection")?;
        }
        self.out.session.set_provider_routing(routing).map_err(|_| "routing")?;
        let catalog = self.fixture.catalog.take().map(Ok).unwrap_or_else(crate::catalog::mock_catalog).map_err(|_| "catalog")?;
        let metadata = bounds::catalog(&catalog, (4 * MIB).saturating_sub(self.out.ownership.admin))?.checked_mul(catalogs).ok_or("catalog_ownership")?;
        Ownership::charge(&mut self.out.ownership.admin, metadata, 4 * MIB)?;
        self.out.session.swap_resource(Some(RunCatalog::new(catalog).map_err(|_| "catalog")?));
        let budget = lock(&self.out.budget.log);
        let state = BudgetState::from_usage(self.fixture.budgets.clone(), &budget, "driver-fixture");
        drop(budget);
        self.out.session.swap_resource(Some(state));
        let prepared = Arc::new(Mutex::new(None::<PreparedRun>));
        let queue = prepared.clone();
        self.out
            .session
            .register_run_host(move |p| {
                let mut q = lock(&queue);
                if q.is_some() {
                    return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Busy, "fixture prepared queue full"));
                }
                *q = Some(p);
                Ok(json!({"queued":true}))
            })
            .map_err(|_| "run_host")?;
        let mut request = json!({"canvas":self.request.canvas,"mode":self.request.mode,"nodes":self.request.nodes});
        let quoted = self.out.session.dispatch("run.quote", &request).map_err(|_| "quote")?;
        let token = quoted.get("token").and_then(|v| v.as_str()).filter(|s| s.len() <= 1024).ok_or("quote_token")?;
        request.as_object_mut().ok_or("request")?.insert("accept".into(), json!({"token":token,"amounts":self.request.amounts,"partial":self.request.partial}));
        self.out.session.dispatch("run.start", &request).map_err(|_| "start")?;
        let p = lock(&prepared).take().ok_or("prepared")?;
        if p.quote().steps.len() > 64 {
            return Err("quote_steps");
        }
        let initial = p.quote().steps.iter().try_fold(0usize, |n, s| usize::try_from(s.jobs).ok().and_then(|jobs| n.checked_add(jobs))).ok_or("epoch_limit")?;
        let extra = self.fixture.actions.iter().filter(|a| matches!(a.kind, ActionKind::Requeue { .. } | ActionKind::Replace { .. })).count();
        let total = initial.checked_add(extra).ok_or("epoch_limit")?;
        if total > self.fixture.epoch_ceiling
            || total.checked_mul(usize::try_from(self.request.options.retry.max_attempts).map_err(|_| "attempt_limit")?).is_none_or(|n| n > 1024)
        {
            return Err("epoch_limit");
        }
        let max_attempts = self
            .fixture
            .epoch_ceiling
            .checked_mul(usize::try_from(self.request.options.retry.max_attempts).map_err(|_| "attempt_limit")?)
            .ok_or("attempt_limit")?;
        self.out.ownership.original = lock(&self.emissions).activate(max_attempts, self.fixture.original_cap)?;
        Ownership::charge(&mut self.out.ownership.delivery_metadata, max_attempts.checked_mul(4096).ok_or("delivery_metadata")?, 4 * MIB)?;
        for step in &p.quote().steps {
            self.out.quote_jobs.push((step.node, step.jobs));
            let bytes = step.models.iter().try_fold(0usize, |n, s| n.checked_add(s.len() + 1)).ok_or("quote_models")?;
            if bytes > 1024 {
                return Err("quote_models");
            }
            let price = match step.per_job {
                Ok(None) => None,
                Ok(Some(estimate)) => Some(JobPrice { node: step.node, model: step.models.join("+"), estimate: Some(estimate) }),
                Err(_) => Some(JobPrice { node: step.node, model: step.models.join("+"), estimate: None }),
            };
            self.prices.insert(step.node, price);
        }
        match self.fixture.before_authorize {
            AuthorityChange::None => {}
            AuthorityChange::Source => {
                let node = self.request.nodes.first().ok_or("source_mutation")?;
                self.out
                    .session
                    .dispatch(
                        "node.param_set",
                        &json!({"canvas":self.request.canvas,"id":node,"name":"style","value":{"type":"text","value":"authority mutation"}}),
                    )
                    .map_err(|_| "source_mutation")?;
            }
            AuthorityChange::Catalog => {
                self.out.session.swap_resource(Some(RunCatalog::new(crate::catalog::mock_catalog().map_err(|_| "catalog")?).map_err(|_| "catalog")?));
            }
            AuthorityChange::Budget => {
                self.out.session.swap_resource(Some(BudgetState {
                    budgets: Budgets { per_call: Some(Money::new(nodecraft_types::Currency::USD, 0)), ..Default::default() },
                    spent: Default::default(),
                }));
            }
            AuthorityChange::Routing => self.out.session.set_provider_routing(ProviderRouting::default()).map_err(|_| "routing")?,
        }
        let (run, progress) = self.out.session.authorize_prepared_run(p, self.request.options).map_err(|_| "authorize")?;
        self.run = Some(run);
        if self.fixture.replace_routing_after_authorize {
            self.out.session.set_provider_routing(ProviderRouting::default()).map_err(|_| "routing")?;
        }
        let scheduler = Scheduler::new(std::mem::take(&mut self.fixture.scheduler));
        let (driver, handle) = driver_with_admission(scheduler, self.fixture.admission).map_err(|_| "driver_admission")?;
        self.handle = Some(handle);
        let composite = Arc::new(Composite { provider: self.runner.clone(), sources: self.sources.clone(), shared: self.shared.clone() });
        let end = self.end.clone();
        let shared = self.shared.clone();
        let fault = self.fault.clone();
        let runtime = self.runtime.clone();
        self.runtime.spawn(Box::pin(async move {
            let scheduler = driver.run(composite, runtime, move |e| event_message(&shared, &fault, e)).await;
            *lock(&end) = Some(scheduler);
        }));
        self.actions = self.fixture.actions.drain(..).map(Some).collect();
        Ok(progress)
    }
    fn trace(&mut self, m: Message) -> Check<()> {
        Ownership::charge(
            &mut self.out.ownership.admin,
            256 + m.key.as_ref().map_or(0, String::len) + m.target.as_ref().map_or(0, |t| t.adapter().len() + 64),
            4 * MIB,
        )?;
        self.out.trace.push(Trace {
            job: m.job,
            epoch: m.epoch,
            kind: m.kind,
            attempt: m.attempt,
            key: m.key,
            target: m.target,
            generation: m.generation,
            handle: m.handle,
        });
        Ok(())
    }
    fn reserve_epoch(&mut self, attempts: usize) -> Check<usize> {
        if self.out.ownership.epochs >= self.fixture.epoch_ceiling
            || attempts > usize::try_from(self.request.options.retry.max_attempts).map_err(|_| "attempt_limit")?
        {
            return Err("epoch_limit");
        }
        self.out.ownership.epoch(attempts)
    }
    fn absorb(&mut self, progress: Progress) -> Check<()> {
        if self.out.ownership.epochs.checked_add(progress.jobs.len()).is_none_or(|n| n > self.fixture.epoch_ceiling)
            || progress.jobs.iter().any(|s| s.retry.max_attempts > self.request.options.retry.max_attempts)
        {
            return Err("epoch_limit");
        }
        if progress.jobs.len() > 128 || progress.finished.len() + self.out.results.len() > 64 || self.acks.len() >= 128 {
            return Err("progress_capacity");
        }
        for f in progress.failed {
            if self.out.failures.len() >= 64 {
                return Err("failure_capacity");
            }
            Ownership::charge(&mut self.out.ownership.admin, f.reason.len().min(1024) + 128, 4 * MIB)?;
            self.out.failures.push((f.node, "run_failed".into()));
        }
        self.out.results.extend(progress.finished);
        for job in progress.cancel {
            self.out.ownership.command()?;
            self.handle.as_ref().ok_or("driver_closed")?.cancel(CancelScope::Jobs(vec![job])).map_err(|_| "cancel_admission")?;
        }
        let mut specs = Vec::with_capacity(progress.jobs.len());
        let mut batch = Vec::with_capacity(progress.jobs.len());
        for spec in progress.jobs {
            let attempts = usize::try_from(spec.retry.max_attempts).map_err(|_| "attempt_limit")?;
            let epoch = self.reserve_epoch(attempts)?;
            let run = self.run.as_ref().ok_or("run")?;
            let work = run.work(spec.id).ok_or("work")?;
            match work {
                JobSpec::Source { value, .. } => {
                    let charge = bounds::value(value, 0)?.owned.checked_mul(self.fanout).ok_or("source_ownership")?;
                    Ownership::charge(&mut self.out.ownership.run, charge, 16 * MIB)?;
                    self.sources.register(spec.id, work.clone());
                    lock(&self.shared).source_jobs.insert(spec.id);
                }
                JobSpec::Generate { node, .. } => {
                    let charge = bounds::work(work, &spec.idempotency_key, attempts)?;
                    Ownership::charge(&mut self.out.ownership.admin, charge, 4 * MIB)?;
                    let price = self.prices.get(node).cloned().flatten();
                    let queued = run.queued_job(spec.id, &spec.idempotency_key, price).map_err(|_| "provider_job")?;
                    self.requests.insert(spec.id, queued.clone());
                    self.runner.register(spec.id, queued);
                }
            }
            lock(&self.shared).queued_owners.push_back((spec.id, epoch));
            self.out.jobs.push(JobReport {
                job: spec.id,
                epoch,
                provider: matches!(work, JobSpec::Generate { .. }),
                starts: Vec::with_capacity(attempts),
                terminal: None,
                notified: false,
            });
            batch.push((spec.id, epoch));
            specs.push(spec);
        }
        if !specs.is_empty() {
            self.out.ownership.command()?;
            let future = Box::pin(self.handle.as_ref().ok_or("driver_closed")?.add(specs));
            self.acks.push(Ack { future, batch });
        }
        Ok(())
    }
}

impl Controller {
    fn notify(&mut self, job: JobId, epoch: usize, kind: &'static str) -> Check<()> {
        let index = self.out.jobs.iter().position(|j| j.job == job && j.epoch == epoch).ok_or("terminal_epoch")?;
        if self.out.jobs.get(index).is_some_and(|j| j.terminal.is_some()) {
            return Err("duplicate_terminal");
        }
        if let Some(row) = self.out.jobs.get_mut(index) {
            row.terminal = Some(kind);
        }
        if self.out.jobs.iter().any(|j| j.job == job && j.epoch < epoch && j.terminal.is_none()) {
            return Ok(());
        }
        if lock(&self.shared).running.get(&job).is_none_or(|a| a.epoch == epoch) {
            self.running.remove(&job);
        }
        if !self.notified.insert(job) {
            return Ok(());
        }
        let provider = self.out.jobs.get(index).is_some_and(|j| j.provider);
        let mut good = None;
        if kind == "done" {
            if provider {
                let delivery_index = self.out.deliveries.iter().position(|d| d.job == job && d.epoch == epoch).ok_or("missing_delivery")?;
                let run = self.run.as_ref().ok_or("run")?;
                let Some(JobSpec::Generate { outputs, .. }) = run.work(job) else { return Err("work") };
                let d = self.out.deliveries.get(delivery_index).ok_or("delivery")?;
                if matches!(d.delivery.accounting, CompletionAccounting::Failed(_)) {
                    return Err("done_accounting_failed");
                }
                let (_, charge) = bounds::completion(&d.delivery.completed, outputs)?;
                Ownership::charge(&mut self.out.ownership.conversion, charge, self.fixture.conversion_cap)?;
                match generate::produced(outputs, &d.delivery.completed) {
                    Ok(converted) => {
                        let n = bounds::routed(&converted.outputs)?.checked_mul(self.fanout).and_then(|n| n.checked_add(1024)).ok_or("run_ownership")?;
                        Ownership::charge(&mut self.out.ownership.run, n, 16 * MIB)?;
                        for (expected, bytes) in converted.assets {
                            let actual = self.out.session.import_asset(&bytes).map_err(|_| "asset_import")?;
                            if actual != expected {
                                return Err("asset_identity");
                            }
                        }
                        good = Some(converted.outputs);
                        if let Some(d) = self.out.deliveries.get_mut(delivery_index) {
                            d.disposition = "converted";
                        }
                    }
                    Err(_) => {
                        if let Some(d) = self.out.deliveries.get_mut(delivery_index) {
                            d.disposition = "conversion_refused";
                        }
                    }
                }
            } else {
                let outputs = take_outputs(&self.sources.outputs, job);
                if outputs.is_empty() {
                    return Err("source_outputs");
                }
                good = Some(outputs);
            }
        }
        if provider && kind != "done" {
            if lock(&self.shared).epochs.get(&job) == Some(&epoch) {
                self.runner.settle(job);
            }
            for d in self.out.deliveries.iter_mut().filter(|d| d.job == job && d.epoch == epoch) {
                d.disposition = if matches!(d.delivery.accounting, CompletionAccounting::Failed(_)) { "accounting_failed" } else { "terminal_failed" };
            }
        }
        let run = self.run.as_mut().ok_or("run")?;
        let progress = match good {
            Some(outputs) => run.job_done(self.out.session.kinds(), job, outputs),
            None => run.job_failed(job, "fixture terminal or conversion refusal"),
        };
        if let Some(row) = self.out.jobs.get_mut(index) {
            row.notified = true;
        }
        self.absorb(progress)
    }
    fn drain(&mut self) -> Check<bool> {
        let mut changed = false;
        {
            let mut shared = lock(&self.shared);
            for mut d in shared.deliveries.drain(..) {
                if self.notified.contains(&d.job) {
                    d.disposition = "late_retained";
                }
                self.out.deliveries.push(d);
                changed = true;
            }
        }
        loop {
            let message = lock(&self.shared).messages.pop_front();
            let Some(message) = message else { break };
            changed = true;
            let (job, epoch, kind, attempt) = (message.job, message.epoch, message.kind, message.attempt);
            if kind == "started" {
                let row = self.out.jobs.iter_mut().find(|j| j.job == job && j.epoch == epoch).ok_or("start_epoch")?;
                if row.starts.last().is_some_and(|n| *n >= attempt) {
                    return Err("attempt_order");
                }
                row.starts.push(attempt);
                let lane = self
                    .run
                    .as_ref()
                    .and_then(|r| r.work(job))
                    .map(|w| match w {
                        JobSpec::Source { .. } => Ok(nodecraft_exec::run::LOCAL_LANE.to_owned()),
                        JobSpec::Generate { .. } => self
                            .run
                            .as_ref()
                            .ok_or("run")
                            .and_then(|r| r.binding(job).map_err(|_| "binding"))
                            .and_then(|b| b.target().lane_name().map_err(|_| "lane")),
                    })
                    .transpose()?
                    .unwrap_or_default();
                self.running.insert(job, lane);
                self.out.peak_global = self.out.peak_global.max(self.running.len());
                for lane in self.running.values() {
                    let count = self.running.values().filter(|l| *l == lane).count();
                    let peak = self.out.peak_lanes.entry(lane.clone()).or_default();
                    *peak = (*peak).max(count);
                }
            }
            if kind == "retrying" {
                self.running.remove(&job);
            }
            self.trace(message)?;
            if matches!(kind, "done" | "failed" | "cancelled" | "skipped") {
                self.notify(job, epoch, kind)?;
            }
        }
        Ok(changed)
    }
    fn acknowledgements(&mut self) -> Check<bool> {
        let w = futures::task::noop_waker();
        let mut changed = false;
        let mut i = 0;
        while i < self.acks.len() {
            let verdict = match self.acks.get_mut(i) {
                Some(a) => a.future.as_mut().poll(&mut Context::from_waker(&w)),
                None => return Err("ack_index"),
            };
            match verdict {
                Poll::Pending => i += 1,
                Poll::Ready(result) => {
                    let a = self.acks.remove(i);
                    changed = true;
                    match result {
                        Ok(Ok(())) => {
                            for (job, epoch) in a.batch {
                                self.trace(Message { job, epoch, kind: "accepted", attempt: 0, key: None, target: None, generation: 0, handle: None })?;
                            }
                        }
                        Ok(Err(_)) | Err(DriverError::Full | DriverError::PayloadLimit) => {
                            for (job, epoch) in a.batch {
                                lock(&self.shared).queued_owners.retain(|owner| *owner != (job, epoch));
                                self.trace(Message { job, epoch, kind: "rejected", attempt: 0, key: None, target: None, generation: 0, handle: None })?;
                                if self.out.jobs.iter().filter(|j| j.job == job).all(|j| j.starts.is_empty() || j.terminal.is_some()) {
                                    self.runner.forget(job);
                                }
                                self.notify(job, epoch, "rejected")?;
                            }
                        }
                        Err(DriverError::Closed) => return Err("ambiguous_ack"),
                    }
                }
            }
        }
        Ok(changed)
    }
    fn action_ready(&self, t: &Trigger) -> bool {
        match t {
            Trigger::At(ms) => self.runtime.now_ms() >= *ms,
            Trigger::Event(job, k) | Trigger::Update(job, k) => self.out.trace.iter().any(|x| x.job == *job && x.kind == *k),
            Trigger::Epoch(job, epoch, k) => self.out.trace.iter().any(|x| x.job == *job && x.epoch == *epoch && x.kind == *k),
        }
    }
    fn actions(&mut self) -> Check<bool> {
        let mut changed = false;
        for i in 0..self.actions.len() {
            let ready = self.actions.get(i).and_then(Option::as_ref).is_some_and(|a| self.action_ready(&a.trigger));
            if !ready {
                continue;
            }
            let a = self.actions.get_mut(i).and_then(Option::take).ok_or("action")?;
            changed = true;
            match a.kind {
                ActionKind::SubmitGate(index) => self.out.adapters.get(index).ok_or("action_adapter")?.submit_gate.open(),
                ActionKind::PollGate(index) => self.out.adapters.get(index).ok_or("action_adapter")?.poll_gate.open(),
                ActionKind::Pause(p) => {
                    self.out.ownership.command()?;
                    self.handle.as_ref().ok_or("driver_closed")?.set_paused(p).map_err(|_| "pause")?;
                }
                ActionKind::Cancel(jobs) => {
                    self.out.ownership.command()?;
                    self.handle.as_ref().ok_or("driver_closed")?.cancel(CancelScope::Jobs(jobs)).map_err(|_| "cancel")?;
                }
                ActionKind::Requeue { job, key, replacement } => {
                    if self.acks.len() >= 128 {
                        return Err("ack_capacity");
                    }
                    let epoch = self.reserve_epoch(usize::try_from(self.request.options.retry.max_attempts).map_err(|_| "attempt_limit")?)?;
                    if let Some(adapter) = replacement {
                        self.replace(job, adapter)?;
                    }
                    lock(&self.shared).queued_owners.push_back((job, epoch));
                    let provider = !lock(&self.shared).source_jobs.contains(&job);
                    self.out.jobs.push(JobReport { job, epoch, provider, starts: Vec::with_capacity(8), terminal: None, notified: false });
                    self.out.ownership.command()?;
                    let future = Box::pin(self.handle.as_ref().ok_or("driver_closed")?.requeue(vec![(job, key)]));
                    self.acks.push(Ack { future, batch: vec![(job, epoch)] });
                }
                ActionKind::Replace { job, adapter } => {
                    let epoch = self.reserve_epoch(usize::try_from(self.request.options.retry.max_attempts).map_err(|_| "attempt_limit")?)?;
                    self.replace(job, adapter)?;

                    self.out.jobs.push(JobReport {
                        job,
                        epoch,
                        provider: true,
                        starts: Vec::with_capacity(8),
                        terminal: Some("registered_only"),
                        notified: false,
                    });
                }
                ActionKind::Restore { job, adapter, base } => {
                    let binding = self.bindings.get(adapter).ok_or("restore_binding")?;
                    let trace = self.out.trace.iter().rev().find(|t| t.job == job && t.kind == "submitted").ok_or("restore_handle")?;
                    if trace.target.as_ref() != Some(binding.target())
                        || trace.key.as_deref() != Some(nodecraft_engine::jobs::sent_key(&base, trace.generation).as_str())
                    {
                        return Err("restore_identity");
                    }
                    let handle = trace.handle.as_ref().ok_or("restore_handle")?;
                    let charge = bounds::json(&handle.data, 0)?.owned.checked_add(handle.remote_id.len() + base.len() + 256).ok_or("restore_ownership")?;
                    Ownership::charge(&mut self.out.ownership.admin, charge, 4 * MIB)?;
                    self.runner.restore_target_generation(binding, base, trace.generation, handle.clone()).map_err(|_| "restore_refused")?;
                }
                ActionKind::Forget => {
                    self.out.ownership.command()?;
                    self.handle.as_ref().ok_or("driver_closed")?.forget_finished().map_err(|_| "forget")?;
                    self.trace(Message { job: JobId(0), epoch: 0, kind: "forgot_finished", attempt: 0, key: None, target: None, generation: 0, handle: None })?;
                    // Command admission does not prove Scheduler forgot a registration.
                    // A replacement may already be installed while its Queued event is pending.
                    // Retain it until the returned Scheduler proves the current job absent.
                }
                ActionKind::ReadyRace { adapter, completion, retrying } => {
                    if !self.out.trace.iter().any(|t| t.job == retrying && t.kind == "retrying") {
                        return Err("race_retry_not_ready");
                    }
                    let retry_deadline = self.runtime.driver_timer_deadline().ok_or("race_timer_not_ready")?;
                    self.out.adapters.get(adapter).ok_or("action_adapter")?.poll_gate.open();
                    if !self.runtime.advance_with(Some(retry_deadline)) || self.runtime.now_ms() != retry_deadline {
                        return Err("race_timer_not_ready");
                    }
                    for _ in 0..256 {
                        if !self.runtime.poll_worker() {
                            break;
                        }
                    }
                    if !lock(&self.shared).deliveries.iter().any(|d| d.job == completion) {
                        return Err("race_completion_not_ready");
                    }
                    self.out.ownership.command()?;
                    self.handle.as_ref().ok_or("driver_closed")?.cancel(CancelScope::Jobs(vec![completion])).map_err(|_| "cancel")?;
                    self.trace(Message {
                        job: completion,
                        epoch: 0,
                        kind: "completion_timer_cancel_ready",
                        attempt: 0,
                        key: None,
                        target: None,
                        generation: 0,
                        handle: None,
                    })?;
                }
                ActionKind::ReplaceLedger => {
                    let previous = std::mem::take(&mut *lock(&self.out.budget.log));
                    self.out.previous_ledgers.push(previous);
                }
                ActionKind::RestoreLedger => {
                    let previous = self.out.previous_ledgers.pop().ok_or("previous_ledger")?;
                    let displaced = std::mem::replace(&mut *lock(&self.out.budget.log), previous);
                    self.out.previous_ledgers.push(displaced);
                }
                ActionKind::Repair { delivery, wrong_ledger } => {
                    let d = self.out.deliveries.get_mut(delivery).ok_or("repair_delivery")?;
                    let CompletionAccounting::Failed(f) = &mut d.delivery.accounting else { return Err("repair_status") };
                    let retry = f.retry.as_mut().ok_or("repair_capability")?;
                    if wrong_ledger {
                        let budget = JobBudget {
                            log: Arc::new(Mutex::new(UsageLog::default())),
                            budgets: self.out.budget.budgets.clone(),
                            session: "driver-fixture".into(),
                            wall_clock_ms: self.out.budget.wall_clock_ms.clone(),
                        };
                        if retry.retry(&budget).is_ok() {
                            return Err("wrong_ledger_repaired");
                        }
                    } else {
                        retry.retry(&self.out.budget).map_err(|_| "repair_refused")?;
                        if retry.retry(&self.out.budget).is_ok() {
                            return Err("repair_reused");
                        }
                        d.disposition = "accounting_repaired_retained";
                    }
                }
            }
            self.trace(Message { job: JobId(0), epoch: 0, kind: "action_acknowledged", attempt: 0, key: None, target: None, generation: 0, handle: None })?;
        }
        Ok(changed)
    }
    fn replace(&mut self, job: JobId, adapter: usize) -> Check<()> {
        let b = self.bindings.get(adapter).ok_or("replacement_binding")?.clone();
        let work = self.run.as_ref().and_then(|r| r.work(job)).ok_or("work")?;
        let charge = bounds::work(work, "replacement reservation", usize::try_from(self.request.options.retry.max_attempts).map_err(|_| "attempt_limit")?)?;
        Ownership::charge(&mut self.out.ownership.admin, charge, 4 * MIB)?;
        let queued = self.requests.get(&job).ok_or("replacement_request")?;
        let mut replacement = queued.clone();
        replacement.provider = b;
        self.runner.register(job, replacement);
        Ok(())
    }
    fn pump(&mut self) -> Check<()> {
        for _ in 0..100_000 {
            self.drain()?;
            self.acknowledgements()?;
            self.actions()?;
            if let Some(f) = self.fault.get() {
                return Err(f);
            }
            let done = self.run.as_ref().is_some_and(|r| r.is_finished());
            if done && self.actions.iter().all(Option::is_none) && self.acks.is_empty() {
                self.handle.take();
            }
            if lock(&self.end).is_some() && self.runtime.idle() {
                self.drain()?;
                self.out.scheduler = lock(&self.end).take();
                self.out.cleanup_complete = true;
                self.reconcile()?;
                self.forget_observed();
                return if done { Ok(()) } else { Err("unnotified_run") };
            }
            if self.runtime.poll_one() {
                continue;
            }
            let action_time = self
                .actions
                .iter()
                .flatten()
                .filter_map(|a| match a.trigger {
                    Trigger::At(t) if t > self.runtime.now_ms() => Some(t),
                    _ => None,
                })
                .min();
            if self.runtime.advance_with(action_time) {
                continue;
            }
            return Err("stalled_gate");
        }
        Err("poll_capacity")
    }
    fn forget_observed(&self) {
        let Some(scheduler) = self.out.scheduler.as_ref() else { return };
        if !self.acks.is_empty() || !lock(&self.shared).queued_owners.is_empty() {
            return;
        }
        for row in &self.out.jobs {
            if row.provider
                && matches!(row.terminal, Some("done" | "failed" | "cancelled" | "skipped"))
                && lock(&self.shared).epochs.get(&row.job) == Some(&row.epoch)
                && scheduler.state(row.job).is_none()
            {
                self.runner.forget(row.job);
            }
        }
    }
    fn reconcile(&self) -> Check<()> {
        let scheduler = self.out.scheduler.as_ref().ok_or("missing_scheduler")?;
        for row in &self.out.jobs {
            let terminal = row.terminal.ok_or("unnotified_epoch")?;
            if lock(&self.shared).epochs.get(&row.job) != Some(&row.epoch) || matches!(terminal, "rejected" | "registered_only") {
                continue;
            }
            let state = scheduler.state(row.job);
            // Explicit forget may remove a proven terminal scheduler row; never a live one.
            if state.is_none() && self.out.trace.iter().any(|t| t.kind == "forgot_finished") {
                continue;
            }
            let matches = match terminal {
                "done" => matches!(state, Some(scheduler::JobState::Done)),
                "failed" => matches!(state, Some(scheduler::JobState::Failed { .. })),
                "cancelled" => matches!(state, Some(scheduler::JobState::Cancelled)),
                "skipped" => matches!(state, Some(scheduler::JobState::Skipped { .. })),
                _ => false,
            };
            if !matches {
                return Err("scheduler_terminal_mismatch");
            }
        }
        Ok(())
    }
    fn shutdown(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.cancel(CancelScope::All);
            drop(h);
        }
        for a in &mut self.actions {
            *a = None;
        }
        for _ in 0..10_000 {
            let _ = self.drain();
            let _ = self.acknowledgements();
            if self.runtime.idle() {
                self.out.cleanup_complete = true;
                break;
            }
            if !self.runtime.poll_one() && !self.runtime.advance() {
                break;
            }
        }
        self.out.scheduler = lock(&self.end).take();
        self.runtime.stop();
        let mut s = lock(&self.shared);
        for mut d in s.deliveries.drain(..) {
            d.disposition = "shutdown_retained";
            self.out.deliveries.push(d);
        }
        self.out.cleanup_complete &= self.out.scheduler.is_some();
    }
    fn record(&mut self) -> Check<()> {
        if !self.fixture.record || self.out.results.is_empty() {
            return Ok(());
        }
        let charge = bounds::record(&self.out.results)?;
        if charge.raw > self.fixture.record_cap {
            return Err("record_envelope");
        }
        Ownership::charge(&mut self.out.ownership.serialized, charge.escaped, 16 * MIB)?;
        let results: Vec<_> = self.out.results.iter().map(|r| json!({"node":r.node,"generation":r.generation})).collect();
        let mut params = json!({"canvas":self.request.canvas,"results":results});
        if self.fixture.invalid_record {
            let result =
                params.get_mut("results").and_then(|v| v.as_array_mut()).and_then(|r| r.first_mut()).and_then(|r| r.as_object_mut()).ok_or("record_fixture")?;
            result.insert("node".into(), json!(NodeId::from_bytes([255; 16])));
        }
        self.out.record_dispatches += 1;
        self.out.session.dispatch("run.record", &params).map_err(|_| "record_dispatch")?;
        self.out.recorded = true;
        for d in &mut self.out.deliveries {
            if d.disposition == "converted" {
                d.disposition = "recorded";
            }
        }
        Ok(())
    }
}
/// All callbacks only move bounded messages. Session authorization, Run notifications and record
/// commands happen here outside callbacks, never inside nested block_on or a provider future.
pub fn run_driver(session: Session, request: DriverRequest, fixture: DriverFixture) -> Result<DriverOutcome, HarnessError> {
    let mut c = Controller::new(session, request, fixture);
    let result = c.initialize().and_then(|p| c.absorb(p)).and_then(|_| c.pump()).and_then(|_| c.record());
    match result {
        Ok(()) => Ok(c.out),
        Err(code) => {
            c.shutdown();
            Err(HarnessError { code, outcome: Box::new(c.out) })
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn fixed_slots_and_gate_headers_fit_precharged_admin() {
        let fixed = 4096 * std::mem::size_of::<Trace>()
            + 2048 * std::mem::size_of::<RetainedDelivery>()
            + 128 * (std::mem::size_of::<JobReport>() + std::mem::size_of::<Ack>())
            + 256 * std::mem::size_of::<Message>()
            + 64 * std::mem::size_of::<StepResult>()
            + 64 * super::super::provider::fixed_gate_storage()
            + 512 * 1024;
        println!("fixed slot/gate/header charge {fixed}, reserved {}", 5 * MIB / 2);
        assert!(fixed <= 5 * MIB / 2);
    }
}
