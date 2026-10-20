use super::*;
use crate::planner::{Planned, RunMode, plan, value_bytes};
use nodecraft_graph::{Changeset, Edge, Endpoint, Port, Project};
use nodecraft_nodes::{Category, ModeId, NodeDescriptor, NodeKind};
use nodecraft_types::{EdgeId, Modality, ParamSchema, PortType};
use nodecraft_types::{ProjectId, Unsigned};
use std::collections::BTreeSet;

pub(super) const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };
const LIST: PortType = PortType { modality: Modality::Text, many: true };
pub(super) const RUN: [u8; 16] = [0xab; 16];

fn text_of(value: &Value) -> String {
    match value {
        Value::Text(t) => t.clone(),
        Value::List(items) => items.iter().map(text_of).collect::<Vec<_>>().join("+"),
        other => format!("{other:?}"),
    }
}

/// List text in, text out: `(a+b+…)`.
struct Join(NodeDescriptor);
impl NodeKind for Join {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("join").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let joined = ctx.inputs.iter().map(|i| text_of(&i.value)).collect::<Vec<_>>().join("+");
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(format!("({joined})")) }])
    }
}

/// Text in, list out: one job per character.
struct Split(NodeDescriptor);
impl NodeKind for Split {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("split").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let text = ctx.inputs.first().map(|i| text_of(&i.value)).unwrap_or_default();
        Ok(text.chars().map(|c| JobSpec::Source { node: ctx.node.id, output: "items".into(), value: Value::Text(c.to_string()) }).collect())
    }
}

pub(super) fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let join = NodeDescriptor {
        id: "test.join".into(),
        label: "Join".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: LIST }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }],
        params: ParamSchema::default(),
        quick_add: None,
    };
    kinds.register(Box::new(Join(join.clone()))).unwrap();
    let split = NodeDescriptor {
        id: "test.split".into(),
        label: "Split".into(),
        inputs: vec![Port { name: "in".into(), port_type: TEXT }],
        outputs: vec![Port { name: "items".into(), port_type: LIST }],
        ..join
    };
    kinds.register(Box::new(Split(split.clone()))).unwrap();
    let upper = NodeDescriptor { id: "test.upper".into(), label: "Upper".into(), outputs: vec![Port { name: "out".into(), port_type: TEXT }], ..split };
    kinds.register(Box::new(Upper(upper))).unwrap();
    kinds
}

/// Text in, text out: upper case.
struct Upper(NodeDescriptor);
impl NodeKind for Upper {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("upper").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let text = ctx.inputs.first().map(|i| text_of(&i.value)).unwrap_or_default();
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(text.to_uppercase()) }])
    }
}

pub(super) fn id(n: u16) -> NodeId {
    let mut bytes = [0u8; 16];
    bytes[..2].copy_from_slice(&n.to_be_bytes());
    bytes[15] = 1;
    NodeId::from_bytes(bytes)
}

/// Builds a project from nodes `(n, kind, text)` and wires `(from, from port, to, order)`.
fn project(nodes: &[(u16, &str, &str)], wires: &[(u16, &str, u16, u32)]) -> Project {
    let kinds = kinds();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Run").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut changes = Vec::new();
    for (n, kind, text) in nodes {
        let values = if *kind == "source.text" { [("text".to_owned(), Value::Text((*text).into()))].into_iter().collect() } else { Default::default() };
        let node = kinds.get(kind).unwrap().descriptor().instantiate(id(*n), &values).unwrap();
        changes.push(Change::AddNode { canvas: C, node });
    }
    for (e, (from, port, to, order)) in wires.iter().enumerate() {
        let mut bytes = [0u8; 16];
        bytes[..4].copy_from_slice(&u32::try_from(e).unwrap().to_be_bytes());
        bytes[15] = 2;
        let edge = Edge {
            id: EdgeId::from_bytes(bytes),
            from: Endpoint { node: id(*from), port: (*port).into() },
            to: Endpoint { node: id(*to), port: "in".into() },
            order: *order,
        };
        changes.push(Change::Connect { canvas: C, edge });
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    project
}

pub(super) fn opts(id: [u8; 16], first_job: u64, limits: RunLimits) -> RunOptions {
    RunOptions { id, first_job, limits, retry: RetryPolicy::NEVER }
}

pub(super) fn plan_of(project: &Project, requested: &[u16]) -> RunPlan {
    let canvas = project.canvases().get(&C).unwrap();
    match plan(canvas, &kinds(), &RunMode::Selected, &requested.iter().map(|n| id(*n)).collect(), &BTreeSet::new()).unwrap() {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => panic!("blocked: {blockers:?}"),
    }
}

/// Runs every job the run hands out (source jobs emit their value), failing those in `fail`.
fn drive(run: &mut Run, kinds: &Registry, first: Progress, fail: &BTreeSet<NodeId>) -> (Vec<StepResult>, Vec<StepFailure>, Vec<scheduler::JobSpec>) {
    let (mut finished, mut failed, mut all) = (first.finished, first.failed, Vec::new());
    let mut queue = first.jobs;
    while let Some(job) = queue.pop() {
        all.push(job.clone());
        let Some(JobSpec::Source { node, output, value }) = run.work(job.id).cloned() else { panic!("only source jobs in these tests") };
        let progress =
            if fail.contains(&node) { run.job_failed(kinds, job.id, "provider said no") } else { run.job_done(kinds, job.id, vec![(output, value)]) };
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
        failed.extend(progress.failed);
    }
    (finished, failed, all)
}

fn output(results: &[StepResult], n: u16) -> String {
    results.iter().find(|r| r.node == id(n)).map(|r| r.generation.outputs.iter().map(text_of).collect::<Vec<_>>().join(",")).unwrap_or_default()
}

#[test]
fn a_chain_runs_deferred_steps_on_upstream_outputs_and_leaves_nothing_stale() {
    let mut p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert_eq!(first.jobs.len(), 1, "only 2's jobs are known up front");
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    assert!(run.is_finished());
    assert_eq!(output(&finished, 2), "(fox)");
    assert_eq!(output(&finished, 3), "((fox))");
    // Recording the results as the engine will leaves nothing stale: the hashes are of the
    // inputs actually used.
    for result in &finished {
        let node = p.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
        assert!(result.generation.input_hash.is_some());
        Changeset::new(record_changes(C, &node, result.generation.clone()).unwrap()).apply(&mut p).unwrap();
    }
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
    // Editing the source makes both stale.
    Change::SetParam { canvas: C, id: id(1), name: "text".into(), value: Some(Value::Text("owl".into())) }.apply(&mut p).unwrap();
    assert_eq!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap(), [id(2), id(3)].into_iter().collect());
}

#[test]
fn fan_in_keeps_noodle_order_and_list_outputs_are_spliced() {
    // 1 "ab" → split 2 → (list) join 4; 3 "c" → join 4 after it.
    let p = project(
        &[(1, "source.text", "ab"), (2, "test.split", ""), (3, "source.text", "c"), (4, "test.join", "")],
        &[(1, "out", 2, 0), (2, "items", 4, 0), (3, "out", 4, 1)],
    );
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 4]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(output(&finished, 2), "a+b", "two jobs assembled into the list output");
    assert_eq!(output(&finished, 4), "(a+b+c)", "spliced, then the second noodle");
}

#[test]
fn a_failed_step_skips_everything_after_it() {
    let p = project(
        &[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", ""), (4, "test.join", "")],
        &[(1, "out", 2, 0), (2, "out", 3, 0), (3, "out", 4, 0)],
    );
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3, 4]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, first, &[id(2)].into_iter().collect());
    assert!(finished.is_empty());
    assert_eq!(jobs.len(), 1, "nothing after 2 ran");
    assert_eq!(failed.iter().map(|f| f.node).collect::<Vec<_>>(), vec![id(2), id(3), id(4)]);
    assert!(failed[1].reason.starts_with("skipped: “test.join”"), "{}", failed[1].reason);
    assert!(run.is_finished());
}

#[test]
fn a_list_fanned_in_past_the_item_limit_is_refused_at_run_time() {
    // #210: 4,096 noodles from a list output are fine to plan (one item each until the run),
    // but spliced they are 8,192 items.
    let wires: Vec<(u16, &str, u16, u32)> = std::iter::once((1, "out", 2, 0)).chain((0..4096u32).map(|n| (2, "items", 3, n))).collect();
    let p = project(&[(1, "source.text", "ab"), (2, "test.split", ""), (3, "test.join", "")], &wires);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert_eq!(finished.iter().map(|r| r.node).collect::<Vec<_>>(), vec![id(2)]);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].reason.contains("more than 4096 items"), "{}", failed[0].reason);
}

#[test]
fn keys_and_generation_ids_are_deterministic_per_run() {
    let p = project(&[(1, "source.text", "fox"), (2, "test.join", "")], &[(1, "out", 2, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2]);
    let start = |run_id| {
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(run_id, 7, RunLimits::default())).unwrap();
        let key = first.jobs[0].idempotency_key.clone();
        let (finished, _, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
        (key, finished[0].generation.id)
    };
    let (key, generation) = start(RUN);
    assert_eq!(start(RUN), (key.clone(), generation));
    assert_ne!(start([1; 16]).1, generation);
    assert_eq!(key, format!("nodecraft:{}:7", "ab".repeat(16)));
    assert!(key.len() < 64 && key.is_ascii());
}

#[test]
fn results_that_dont_fit_their_outputs_fail_the_step() {
    let p = project(&[(1, "source.text", "fox"), (2, "test.join", "")], &[(1, "out", 2, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2]);
    for outputs in [
        vec![],
        vec![("out".to_owned(), Value::Text("a".into())), ("out".to_owned(), Value::Text("b".into()))],
        vec![("nope".to_owned(), Value::Text("a".into()))],
    ] {
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
        let progress = run.job_done(&kinds, first.jobs[0].id, outputs);
        assert_eq!(progress.failed.len(), 1, "{progress:?}");
        // A late or duplicate report changes nothing.
        assert_eq!(run.job_done(&kinds, first.jobs[0].id, vec![]), Progress::default());
        assert_eq!(run.job_done(&kinds, JobId(999), vec![]), Progress::default());
    }
}

#[test]
fn recording_a_result_past_the_history_limit_drops_the_oldest_inactive() {
    let mut node = Node::new(id(1), "test.join");
    for n in 0..MAX_HISTORY {
        node.history.push(Generation::new(GenerationId::from_bytes(u128::try_from(n + 1).unwrap().to_be_bytes()), vec![]));
    }
    node.active = node.history.first().map(|g| g.id);
    let changes = record_changes(C, &node, Generation::new(GenerationId::from_bytes([9; 16]), vec![])).unwrap();
    assert!(matches!(changes.first(), Some(Change::RemoveGeneration { generation, .. }) if Some(*generation) == node.history.get(1).map(|g| g.id)));
    assert!(matches!(changes.get(1), Some(Change::AddGeneration { index, .. }) if *index == MAX_HISTORY - 1));
}

#[test]
fn a_plan_runs_end_to_end_through_the_scheduler_and_driver() {
    use crate::driver::{Runtime, TaskFuture, driver};
    use futures::executor::{ThreadPool, block_on};
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Pool(ThreadPool, AtomicU64);
    impl Runtime for Pool {
        fn spawn(&self, task: TaskFuture<()>) {
            self.0.spawn_ok(task);
        }
        fn sleep(&self, ms: u64) -> TaskFuture<()> {
            self.1.fetch_add(ms, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            self.1.load(Ordering::SeqCst)
        }
    }
    let p = project(
        &[(1, "source.text", "ab"), (2, "test.split", ""), (3, "test.join", ""), (4, "test.join", "")],
        &[(1, "out", 2, 0), (2, "items", 3, 0), (3, "out", 4, 0)],
    );
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3, 4]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let runner = SourceRunner::default();
    let mut scheduler = scheduler::Scheduler::new(scheduler::Limits::default());
    for job in &first.jobs {
        runner.register(job.id, run.work(job.id).unwrap().clone());
    }
    scheduler.add(first.jobs.clone()).unwrap();
    let (driver, handle) = driver(scheduler);
    let mut handle = Some(handle);
    // The driver keeps a panicking listener from stalling the queue, so check outside it.
    let results = Arc::new(Mutex::new(first.finished.clone()));
    let collected = Arc::clone(&results);
    let host = runner.clone();
    let events = move |event: scheduler::Event| {
        let progress = match event {
            scheduler::Event::Done { job } => run.job_done(&kinds, job, take_outputs(&host.outputs, job)),
            scheduler::Event::Failed { job, error } => run.job_failed(&kinds, job, &error),
            scheduler::Event::Cancelled { job } => run.job_cancelled(&kinds, job, "cancelled"),
            _ => Progress::default(),
        };
        for job in &progress.jobs {
            if let Some(work) = run.work(job.id) {
                host.register(job.id, work.clone());
            }
        }
        if let Some(handle) = &handle {
            if !progress.cancel.is_empty() {
                let _ = handle.cancel(scheduler::CancelScope::Jobs(progress.cancel.clone()));
            }
            if !progress.jobs.is_empty() {
                // Sent at once; a refusal would be reported back as failed jobs.
                drop(handle.add(progress.jobs));
            }
        }
        collected.lock().unwrap().extend(progress.finished);
        if run.is_finished() {
            handle = None;
        }
    };
    let runtime = Arc::new(Pool(ThreadPool::new().unwrap(), AtomicU64::new(0)));
    let scheduler = block_on(driver.run(Arc::new(runner), runtime, events));
    assert!(scheduler.is_idle());
    let results = results.lock().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(output(&results, 4), "((a+b))");
}

#[test]
fn too_many_jobs_are_refused_and_a_step_with_none_finishes_at_once() {
    let long = "x".repeat(MAX_STEP_JOBS + 1);
    let p = project(&[(1, "source.text", &long), (2, "test.split", ""), (3, "source.text", ""), (4, "test.split", "")], &[(1, "out", 2, 0), (3, "out", 4, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 4]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert!(first.jobs.is_empty());
    assert_eq!(first.failed.iter().map(|f| f.node).collect::<Vec<_>>(), vec![id(2)]);
    assert!(first.failed[0].reason.contains("more than 4096 jobs"));
    // An empty text splits into no jobs: an empty list, finished without running anything.
    assert_eq!(first.finished.iter().map(|r| (r.node, r.generation.outputs.clone())).collect::<Vec<_>>(), vec![(id(4), vec![Value::List(vec![])])]);
    assert!(run.is_finished());
    assert_eq!(run.job_failed(&kinds, JobId(1), "late"), Progress::default());
}

/// Text in, text out, through `n` jobs that each emit a piece: lets a test fail one of several.
pub(super) struct Pieces(pub(super) NodeDescriptor);
impl NodeKind for Pieces {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("pieces").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok((0..3).map(|n| JobSpec::Source { node: ctx.node.id, output: "items".into(), value: Value::Text(n.to_string()) }).collect())
    }
}

#[test]
fn fan_out_runs_every_branch_and_a_failed_job_cancels_its_siblings() {
    // 1 → 2 → {3, 4}: both branches run on 2's output.
    let p = project(
        &[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", ""), (4, "test.join", "")],
        &[(1, "out", 2, 0), (2, "out", 3, 0), (2, "out", 4, 0)],
    );
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3, 4]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert!(failed.is_empty());
    assert_eq!((output(&finished, 3), output(&finished, 4)), ("((fox))".into(), "((fox))".into()));

    // A three-job step: one job fails; the two others are to be cancelled, and a late report
    // from one of them changes nothing.
    let mut kinds = kinds;
    let pieces = NodeDescriptor {
        id: "test.pieces".into(),
        label: "Pieces".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: TEXT }],
        outputs: vec![Port { name: "items".into(), port_type: LIST }],
        params: ParamSchema::default(),
        quick_add: None,
    };
    kinds.register(Box::new(Pieces(pieces))).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Pieces").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let text =
        kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".to_owned(), Value::Text("x".into()))].into_iter().collect()).unwrap();
    let node = kinds.get("test.pieces").unwrap().descriptor().instantiate(id(2), &Default::default()).unwrap();
    let wire =
        Edge { id: EdgeId::from_bytes([5; 16]), from: Endpoint { node: id(1), port: "out".into() }, to: Endpoint { node: id(2), port: "in".into() }, order: 0 };
    Changeset::new(vec![Change::AddNode { canvas: C, node: text }, Change::AddNode { canvas: C, node }, Change::Connect { canvas: C, edge: wire }])
        .apply(&mut p)
        .unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Ready(plan) = crate::planner::plan(canvas, &kinds, &RunMode::Selected, &[id(2)].into_iter().collect(), &BTreeSet::new()).unwrap() else {
        panic!()
    };
    let (mut run, first) = Run::start(&plan, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let ids: Vec<JobId> = first.jobs.iter().map(|j| j.id).collect();
    assert_eq!(ids.len(), 3);
    assert!(run.job_done(&kinds, ids[0], vec![("items".into(), Value::Text("0".into()))]).finished.is_empty());
    let progress = run.job_failed(&kinds, ids[1], "no");
    assert_eq!(progress.cancel, vec![ids[2]]);
    assert_eq!(progress.failed.len(), 1);
    assert_eq!(run.job_done(&kinds, ids[2], vec![]), Progress::default());
    assert!(run.is_finished());
}

#[test]
fn byte_limits_apply_at_run_time() {
    let p = project(&[(1, "source.text", "abcdef"), (2, "test.split", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "items", 3, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let each = crate::planner::value_bytes(&Value::Text("a".into()));
    for (limits, says) in [
        (RunLimits { step_bytes: each * 5, ..RunLimits::default() }, "its inputs are larger"),
        (RunLimits { run_bytes: each * 5, ..RunLimits::default() }, "the run's inputs are larger"),
        (RunLimits { list_items: 5, ..RunLimits::default() }, "more than 5 items"),
    ] {
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, limits)).unwrap();
        let (_, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].reason.contains(says), "{}", failed[0].reason);
    }
}

#[test]
fn hashes_match_staleness_through_routers_and_splicing() {
    // 1 "ab" → split 2 → router 5 → join 3 (list, spliced through the router).
    let p = project(&[(1, "source.text", "ab"), (2, "test.split", ""), (3, "test.join", "")], &[(1, "out", 2, 0)]);
    let kinds = kinds();
    let mut p = p;
    let router = kinds
        .get(nodecraft_nodes::router::ID)
        .unwrap()
        .instantiate_instance(id(5), nodecraft_nodes::router::config(Modality::Text), &Default::default())
        .unwrap();
    let wire = |e: u8, from: (u16, &str), to: (u16, &str)| Edge {
        id: EdgeId::from_bytes([e; 16]),
        from: Endpoint { node: id(from.0), port: from.1.into() },
        to: Endpoint { node: id(to.0), port: to.1.into() },
        order: 0,
    };
    Changeset::new(vec![
        Change::AddNode { canvas: C, node: router },
        Change::Connect { canvas: C, edge: wire(40, (2, "items"), (5, "in")) },
        Change::Connect { canvas: C, edge: wire(41, (5, "out"), (3, "in")) },
    ])
    .apply(&mut p)
    .unwrap();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    for result in &finished {
        let node = p.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
        Changeset::new(record_changes(C, &node, result.generation.clone()).unwrap()).apply(&mut p).unwrap();
    }
    assert!(finished.iter().all(|r| r.generation.input_hash.is_some()));
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
}

#[test]
fn malformed_plans_are_refused_and_an_empty_run_is_finished_at_once() {
    let p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    let kinds = kinds();
    let canvas = p.canvases().get(&C).unwrap();
    let plan = plan_of(&p, &[2, 3]);
    // Out of order (3 before the step it comes after), a self-dependency, a duplicate.
    let mut reversed = plan.clone();
    reversed.steps.reverse();
    let mut selfish = plan.clone();
    selfish.steps[0].after.push(id(2));
    let mut twice = plan.clone();
    twice.steps.push(plan.steps[0].clone());
    for bad in [reversed, selfish, twice] {
        assert!(Run::start(&bad, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).is_err());
    }
    let (run, first) = Run::start(&RunPlan { steps: vec![], edges: vec![] }, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert!(run.is_finished() && first == Progress::default());
}

#[test]
fn recording_a_large_result_drops_old_generations_until_it_fits() {
    let big = |n: u8| Generation::new(GenerationId::from_bytes([n; 16]), vec![Value::Text("x".repeat(1_000_000))]);
    let mut node = Node::new(id(1), "test.join");
    for n in 1..=8 {
        node.history.push(big(n));
    }
    node.active = Some(GenerationId::from_bytes([8; 16]));
    let changes = record_changes(C, &node, big(9)).unwrap();
    let removed: Vec<_> = changes.iter().filter_map(|c| if let Change::RemoveGeneration { generation, .. } = c { Some(*generation) } else { None }).collect();
    assert!(!removed.is_empty() && !removed.contains(&GenerationId::from_bytes([8; 16])), "the active one stays");
    assert_eq!(removed.first(), Some(&GenerationId::from_bytes([1; 16])), "oldest first");
    // A result larger than a node can hold can't be recorded at all.
    assert!(record_changes(C, &node, Generation::new(GenerationId::from_bytes([10; 16]), vec![Value::Text("x".repeat(MAX_NODE_BYTES))])).is_err());
}

#[test]
fn inputs_are_dropped_before_the_parameters_so_a_drawn_seed_survives() {
    let job = |n: usize| nodecraft_graph::JobSettings {
        provider: "mock".into(),
        instance: None,
        model: "remote".into(),
        params: [("seed".into(), Value::Unsigned(Unsigned(42)))].into(),
        inputs: vec![nodecraft_graph::JobInput { role: "prompt".into(), value: Value::Text(format!("{n}{}", "x".repeat(1_000_000))) }],
        item: None,
    };
    let large = Generation { settings: (0..9).map(job).collect(), ..Generation::new(GenerationId::from_bytes([1; 16]), vec![Value::Text("made".into())]) };
    let kept = super::recordable(large.clone()).unwrap();
    assert_eq!(kept.settings.len(), 9);
    assert!(kept.settings.iter().all(|job| job.inputs.is_empty() && job.params.get("seed") == Some(&Value::Unsigned(Unsigned(42)))));
    // Beside a large active result, too.
    let mut node = Node::new(id(1), "test.join");
    node.history.push(Generation::new(GenerationId::from_bytes([2; 16]), vec![Value::Text("x".repeat(1_000_000))]));
    node.active = Some(GenerationId::from_bytes([2; 16]));
    let changes = record_changes(C, &node, Generation { settings: (0..8).map(job).collect(), ..large }).unwrap();
    let recorded = changes.iter().find_map(|c| if let Change::AddGeneration { generation, .. } = c { Some(generation) } else { None }).unwrap();
    assert_eq!(recorded.settings.len(), 8);
    assert!(recorded.settings.iter().all(|job| job.inputs.is_empty() && !job.params.is_empty()));
}

#[test]
fn a_result_that_fits_beside_the_active_one_only_without_its_settings_is_recorded_without_them() {
    let text = |n: usize| Value::Text(format!("{n}{}", "x".repeat(1_000_000)));
    let mut node = Node::new(id(1), "test.join");
    node.history.push(Generation::new(GenerationId::from_bytes([1; 16]), (0..5).map(text).collect()));
    node.active = Some(GenerationId::from_bytes([1; 16]));
    let job = |n: usize| nodecraft_graph::JobSettings {
        provider: "mock".into(),
        instance: None,
        model: "remote".into(),
        params: [("prompt".into(), text(n))].into(),
        inputs: Vec::new(),
        item: None,
    };
    let result = Generation { settings: (0..3).map(job).collect(), ..Generation::new(GenerationId::from_bytes([2; 16]), vec![text(9)]) };
    let changes = record_changes(C, &node, result.clone()).unwrap();
    let recorded = changes.iter().find_map(|c| if let Change::AddGeneration { generation, .. } = c { Some(generation) } else { None }).unwrap();
    assert!(recorded.settings.is_empty() && recorded.outputs == result.outputs, "the paid result is kept, its settings dropped");
    // A result that doesn't fit even without them still fails.
    let outputs = (0..4).map(text).collect();
    assert!(record_changes(C, &node, Generation { outputs, ..result }).is_err());
}

/// List in, list out: one job per item (so an empty list plans no jobs).
struct Each(NodeDescriptor);
impl NodeKind for Each {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("each").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let items = match ctx.inputs.first().map(|i| &i.value) {
            Some(Value::List(items)) => items.clone(),
            _ => vec![],
        };
        Ok(items.into_iter().map(|value| JobSpec::Source { node: ctx.node.id, output: "items".into(), value }).collect())
    }
}

#[test]
fn a_long_chain_of_steps_without_jobs_finishes_without_recursion() {
    let mut kinds = kinds();
    let each = NodeDescriptor {
        id: "test.each".into(),
        label: "Each".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: LIST }],
        outputs: vec![Port { name: "items".into(), port_type: LIST }],
        params: ParamSchema::default(),
        quick_add: None,
    };
    kinds.register(Box::new(Each(each))).unwrap();
    // An empty split feeds 9,990 chained "each" steps (the document holds 10,000 nodes): every
    // one plans no jobs. Recursing per step overflowed the stack at about 1,000.
    const N: u16 = 9_990;
    let mut p = Project::new(ProjectId::from_bytes([4; 16]), "Chain").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let mut changes = vec![
        Change::AddNode {
            canvas: C,
            node: kinds
                .get("source.text")
                .unwrap()
                .descriptor()
                .instantiate(id(0), &[("text".to_owned(), Value::Text(String::new()))].into_iter().collect())
                .unwrap(),
        },
        Change::AddNode { canvas: C, node: kinds.get("test.split").unwrap().descriptor().instantiate(id(1), &Default::default()).unwrap() },
    ];
    let edge = |n: u16, from: (u16, &str), to: u16| {
        let mut bytes = [0u8; 16];
        bytes[..2].copy_from_slice(&n.to_be_bytes());
        bytes[15] = 3;
        Edge {
            id: EdgeId::from_bytes(bytes),
            from: Endpoint { node: id(from.0), port: from.1.into() },
            to: Endpoint { node: id(to), port: "in".into() },
            order: 0,
        }
    };
    changes.push(Change::Connect { canvas: C, edge: edge(0, (0, "out"), 1) });
    for n in 2..N + 2 {
        changes.push(Change::AddNode { canvas: C, node: kinds.get("test.each").unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap() });
        changes.push(Change::Connect { canvas: C, edge: edge(n, (n - 1, "items"), n) });
    }
    for chunk in changes.chunks(5_000) {
        Changeset::new(chunk.to_vec()).apply(&mut p).unwrap();
    }
    let canvas = p.canvases().get(&C).unwrap();
    let requested = (1..N + 2).map(id).collect();
    let Planned::Ready(plan) = crate::planner::plan(canvas, &kinds, &RunMode::Selected, &requested, &BTreeSet::new()).unwrap() else { panic!("blocked") };
    let (run, first) = Run::start(&plan, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert!(run.is_finished());
    assert_eq!(first.finished.len(), usize::from(N) + 1);
    assert!(first.jobs.is_empty() && first.failed.is_empty());
}

#[test]
fn omitted_defaults_match_materialized_stamps_and_budget_refusal_retains_results() {
    use nodecraft_types::{ParamControl, ParamSpec};
    let mut kinds = kinds();
    let mut descriptor = kinds.get("test.join").unwrap().descriptor().clone();
    descriptor.id = "test.default_join".into();
    descriptor.params.params.push(ParamSpec {
        name: "prompt".into(),
        label: "Prompt".into(),
        description: None,
        control: ParamControl::Text { multiline: true, max_bytes: 1_000_000 },
        required: false,
        default: Some(Value::Text("x".repeat(600_000))),
        visible_if: None,
    });
    kinds.register(Box::new(Join(descriptor))).unwrap();
    let mut p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    for n in [2, 3] {
        let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().clone();
        node.kind = "test.default_join".into();
        assert!(node.params.is_empty());
        Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    }
    let plan = match plan(p.canvases().get(&C).unwrap(), &kinds, &RunMode::Selected, &[id(2), id(3)].into_iter().collect(), &BTreeSet::new()).unwrap() {
        Planned::Ready(plan) => plan,
        other => panic!("unexpected plan {other:?}"),
    };
    for budget in [crate::staleness::MAX_QUERY_BYTES, 1_000_000] {
        let canvas = p.canvases().get(&C).unwrap();
        let (mut run, first) = Run::start(&plan, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
        run.hasher = crate::staleness::InputHasher::new(canvas, &kinds, budget).unwrap();
        let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
        assert!(failed.is_empty());
        assert_eq!(finished.len(), 2, "hash refusal must not discard completed work");
        assert!(finished[0].generation.input_hash.is_some());
        if budget == 1_000_000 {
            assert!(finished[1].generation.input_hash.is_none(), "unavailable stamp is recorded honestly");
        } else {
            let mut recorded = p.clone();
            for result in finished {
                let node = recorded.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
                assert!(result.generation.input_hash.is_some());
                Changeset::new(record_changes(C, &node, result.generation).unwrap()).apply(&mut recorded).unwrap();
            }
            assert!(crate::staleness::stale(recorded.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
        }
    }
}

#[test]
fn failed_materialization_siblings_cannot_copy_past_the_shared_budget() {
    let p = project(
        &[(1, "source.text", "ab"), (2, "test.split", ""), (3, "test.join", ""), (4, "test.join", ""), (5, "test.join", ""), (6, "test.join", "")],
        &[(1, "out", 2, 0), (2, "items", 3, 0), (2, "items", 4, 0), (2, "items", 5, 0), (2, "items", 6, 0)],
    );
    let kinds = kinds();
    let each = value_bytes(&Value::Text("a".into()));
    let cap = 2 * each;
    for late in ["unavailable", "cardinality", "items", "bytes"] {
        let mut plan = plan_of(&p, &[2, 3, 4, 5, 6]);
        for step in plan.steps.iter_mut().filter(|s| [id(3), id(4), id(5)].contains(&s.node)) {
            let input = step.inputs.first_mut().unwrap();
            let value = || InputSource::Value(Value::Text("a".into()));
            match late {
                "unavailable" => input.from.push(InputSource::Step { node: id(2), port: "missing".into(), list: true }),
                "cardinality" => {
                    input.port_type = TEXT;
                    input.from = vec![value(), value()];
                }
                "items" => input.from = vec![value(), value(), value()],
                "bytes" => input.from.push(value()),
                _ => panic!(),
            }
        }
        let limits = RunLimits { list_items: if late == "bytes" { 3 } else { 2 }, step_bytes: cap, run_bytes: cap };
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, limits)).unwrap();
        let (finished, failed, jobs) = drive(&mut run, &kinds, first, &BTreeSet::new());
        assert_eq!(finished.iter().map(|s| s.node).collect::<Vec<_>>(), vec![id(2), id(6)], "{late}");
        assert_eq!(failed.iter().map(|s| s.node).collect::<Vec<_>>(), vec![id(3), id(4), id(5)], "{late}");
        let reason = match late {
            "unavailable" => "no missing output",
            "cardinality" => "takes one value",
            "items" => "more than 2 items",
            "bytes" => "its inputs are larger",
            _ => panic!(),
        };
        assert!(failed.iter().all(|failure| failure.reason.contains(reason)), "{late}: {failed:?}");
        assert_eq!(output(&finished, 6), "(a+b)");
        assert!(run.copied_input_bytes.get() <= cap, "{late}: copied {} bytes despite cap {cap}", run.copied_input_bytes.get());
        assert_eq!(run.run_bytes, cap);
        assert!(run.is_finished());
        for job in jobs {
            assert_eq!(run.job_done(&kinds, job.id, vec![]), Progress::default());
        }
    }
}

#[test]
fn materialization_exact_and_one_over_limits_admit_only_copied_values() {
    let p = project(&[(1, "source.text", "ab"), (2, "test.split", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "items", 3, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let cap = 2 * value_bytes(&Value::Text("a".into()));
    for limits in [
        RunLimits { list_items: 2, step_bytes: cap, run_bytes: cap },
        RunLimits { list_items: 1, step_bytes: cap, run_bytes: cap },
        RunLimits { list_items: 2, step_bytes: cap - 1, run_bytes: cap },
        RunLimits { list_items: 2, step_bytes: cap, run_bytes: cap - 1 },
    ] {
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, limits)).unwrap();
        let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
        let accepted = limits.list_items == 2 && limits.step_bytes == cap && limits.run_bytes == cap;
        assert_eq!(finished.len(), if accepted { 2 } else { 1 });
        assert_eq!(failed.len(), usize::from(!accepted));
        assert_eq!(run.run_bytes, if accepted { cap } else { 0 });
        assert_eq!(run.copied_input_bytes.get(), run.run_bytes, "a rejected input is not copied or charged");
    }
}

struct RejectMaterialized(Join);
impl NodeKind for RejectMaterialized {
    fn descriptor(&self) -> &NodeDescriptor {
        self.0.descriptor()
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        self.0.mode(inputs)
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Err(invalid("custom planning rejected materialized inputs"))
    }
}

#[test]
fn copies_stay_charged_when_kind_planning_fails_and_remaining_budget_is_usable() {
    let mut p = project(
        &[(1, "source.text", "ab"), (2, "test.split", ""), (3, "test.join", ""), (4, "test.join", ""), (5, "test.join", "")],
        &[(1, "out", 2, 0), (2, "items", 3, 0), (2, "items", 4, 0), (2, "items", 5, 0)],
    );
    let mut kinds = kinds();
    let mut descriptor = kinds.get("test.join").unwrap().descriptor().clone();
    descriptor.id = "test.reject_materialized".into();
    kinds.register(Box::new(RejectMaterialized(Join(descriptor)))).unwrap();
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(3)).unwrap().clone();
    node.kind = "test.reject_materialized".into();
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Ready(mut plan) = plan(canvas, &kinds, &RunMode::Selected, &[id(2), id(3), id(4), id(5)].into_iter().collect(), &BTreeSet::new()).unwrap()
    else {
        panic!("blocked")
    };
    // The independent sibling needs just one value of the remaining allowance.
    plan.steps.iter_mut().find(|s| s.node == id(4)).unwrap().inputs[0].from = vec![InputSource::Value(Value::Text("z".into()))];
    let each = value_bytes(&Value::Text("a".into()));
    let limits = RunLimits { list_items: 2, step_bytes: 2 * each, run_bytes: 3 * each };
    let (mut run, first) = Run::start(&plan, C, canvas, &kinds, opts(RUN, 1, limits)).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert_eq!(finished.iter().map(|r| r.node).collect::<Vec<_>>(), vec![id(2), id(4)]);
    assert_eq!(output(&finished, 4), "(z)");
    assert_eq!(failed.len(), 2);
    assert!(failed[0].reason.contains("custom planning rejected"));
    assert!(failed[1].reason.contains("the run's inputs are larger"));
    assert_eq!(run.run_bytes, 3 * each);
    assert_eq!(run.copied_input_bytes.get(), 3 * each);
    assert!(run.is_finished());
    for job in jobs {
        assert_eq!(run.job_failed(&kinds, job.id, "late cancellation"), Progress::default());
    }
}

#[test]
fn runtime_size_preflight_matches_all_values_and_has_depth_bounded_scratch() {
    use nodecraft_types::{AssetRef, Blake3, Integer, MediaMeta, Number, Unsigned};
    let leaf = Value::Text("leaf".into());
    let nest = |depth| (0..depth).fold(leaf.clone(), |value, _| Value::List(vec![value]));
    let wide = Value::List(vec![Value::Bool(true); nodecraft_graph::MAX_VALUE_ITEMS - 1]);
    let values = vec![
        Value::Bool(true),
        Value::Integer(Integer(i64::MIN)),
        Value::Unsigned(Unsigned(u64::MAX)),
        Value::Number(Number::new(f64::MAX).unwrap()),
        leaf.clone(),
        Value::Media(AssetRef { hash: Blake3::from_bytes([1; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() }),
        Value::List(vec![Value::Text("nested".into()), Value::List(vec![Value::Bool(false)])]),
        nest(nodecraft_graph::MAX_VALUE_DEPTH),
        wide,
    ];
    let p = project(&[(1, "source.text", "x"), (2, "test.split", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "items", 3, 0)]);
    let kinds = kinds();
    for value in values {
        let required = value_bytes(&value);
        let mut plan = plan_of(&p, &[2, 3]);
        plan.steps.iter_mut().find(|s| s.node == id(3)).unwrap().inputs[0].from = vec![InputSource::Value(value.clone())];
        for budget in [required, required - 1] {
            let limits = RunLimits { list_items: 1, step_bytes: budget, run_bytes: budget };
            let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, limits)).unwrap();
            let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
            assert_eq!(failed.is_empty(), budget == required);
            assert_eq!(finished.len(), if budget == required { 2 } else { 1 });
            assert_eq!(run.run_bytes, if budget == required { required } else { 0 });
            assert_eq!(run.copied_input_bytes.get(), run.run_bytes);
            assert!(run.preflight_stack_peak.get() <= nodecraft_graph::MAX_VALUE_DEPTH + 1);
            if let Value::List(items) = &value
                && items.len() == nodecraft_graph::MAX_VALUE_ITEMS - 1
            {
                assert_eq!(run.preflight_stack_peak.get(), 2, "wide-list scratch tracks root/child depth, not4095children");
            }
        }
    }
    for value in [nest(nodecraft_graph::MAX_VALUE_DEPTH + 1), Value::List(vec![Value::Bool(true); nodecraft_graph::MAX_VALUE_ITEMS])] {
        let mut plan = plan_of(&p, &[2, 3]);
        plan.steps.iter_mut().find(|s| s.node == id(3)).unwrap().inputs[0].from = vec![InputSource::Value(value)];
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
        let (_, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].reason.contains("document"));
        assert_eq!(run.run_bytes, 0);
        assert_eq!(run.copied_input_bytes.get(), 0);
        assert!(run.preflight_stack_peak.get() <= nodecraft_graph::MAX_VALUE_DEPTH + 1);
    }
}

/// Text in, text out, through one model call on provider `mock`.
struct Calls(NodeDescriptor);
impl NodeKind for Calls {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("calls").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let outputs = vec![nodecraft_nodes::GenerateOutput { port: "out".into(), modality: Modality::Text }];
        Ok(vec![JobSpec::Generate {
            instance: None,
            node: ctx.node.id,
            outputs,
            provider: "mock".into(),
            model: "m".into(),
            params: Default::default(),
            inputs: vec![],
        }])
    }
}

#[test]
fn model_calls_get_their_provider_lane_and_the_source_runner_refuses_them() {
    use crate::driver::JobRunner;
    let mut kinds = kinds();
    let calls = NodeDescriptor {
        id: "test.calls".into(),
        label: "Calls".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: LIST }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }],
        params: ParamSchema {
            params: vec![nodecraft_types::ParamSpec {
                name: "model".into(),
                label: "Model".into(),
                description: None,
                control: nodecraft_types::ParamControl::Model,
                required: false,
                default: Some(Value::Text("m".into())),
                visible_if: None,
            }],
        },
        quick_add: None,
    };
    kinds.register(Box::new(Calls(calls))).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([5; 16]), "Calls").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let text =
        kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".to_owned(), Value::Text("x".into()))].into_iter().collect()).unwrap();
    let node = kinds.get("test.calls").unwrap().descriptor().instantiate(id(2), &Default::default()).unwrap();
    let wire =
        Edge { id: EdgeId::from_bytes([6; 16]), from: Endpoint { node: id(1), port: "out".into() }, to: Endpoint { node: id(2), port: "in".into() }, order: 0 };
    Changeset::new(vec![Change::AddNode { canvas: C, node: text }, Change::AddNode { canvas: C, node }, Change::Connect { canvas: C, edge: wire }])
        .apply(&mut p)
        .unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    assert!(matches!(crate::planner::plan(canvas, &kinds, &RunMode::Selected, &[id(2)].into_iter().collect(), &BTreeSet::new()).unwrap(), Planned::Blocked(_)));
    let mut targets = nodecraft_types::TargetSelection::empty();
    targets.insert(id(2), &nodecraft_types::ProviderTarget::default_instance("mock").unwrap()).unwrap();
    let Planned::Ready(plan) =
        crate::planner::plan_with_targets(canvas, &kinds, &RunMode::Selected, &[id(2)].into_iter().collect(), &BTreeSet::new(), &targets).unwrap()
    else {
        panic!()
    };
    let catalog = crate::admission::tests::free_catalog("m", "mock", "m");
    let (admitted, _) = crate::admission::admit_default_fixture(plan, C, canvas.clone(), &kinds, &catalog, Default::default()).unwrap();
    let (run, first) = Run::start_admitted(admitted, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let job = first.jobs.first().unwrap();
    assert_eq!(job.lane, Lane::for_target(&nodecraft_types::ProviderTarget::default_instance("mock").unwrap()).unwrap());
    let runner = SourceRunner::default();
    runner.register(job.id, run.work(job.id).unwrap().clone());
    let start = Start { job: job.id, lane: job.lane.clone(), attempt: 1, idempotency_key: job.idempotency_key.clone() };
    let outcome = futures::executor::block_on(runner.run(start, crate::driver::CancelToken::default()));
    assert!(matches!(outcome, Outcome::Failed { transient: false, .. }), "{outcome:?}");
}

#[test]
fn unadmitted_manual_generate_cannot_escape_as_source_work() {
    let p = project(&[(1, "source.text", "public")], &[]);
    let mut plan = plan_of(&p, &[1]);
    plan.steps[0].jobs = Some(vec![JobSpec::Generate {
        instance: None,
        node: id(1),
        outputs: vec![],
        provider: "unquoted".into(),
        model: "unquoted".into(),
        params: Default::default(),
        inputs: vec![],
    }]);
    let (_, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds(), opts(RUN, 1, RunLimits::default())).unwrap();
    assert!(first.jobs.is_empty(), "unquoted generation must not be scheduled");
    assert_eq!(first.failed.len(), 1);
}

/// 5 (batch "fox\nowl") → upper 2 → upper 3 → join 4 (collects every item).
pub(super) fn batched() -> Project {
    let kinds = kinds();
    let mut p = project(&[(2, "test.upper", ""), (3, "test.upper", ""), (4, "test.join", "")], &[(2, "out", 3, 0), (3, "out", 4, 0)]);
    let mut batch = kinds.get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(id(5), Default::default(), &Default::default()).unwrap();
    batch.params.insert("text".into(), Value::Text("fox\nowl".into()));
    Changeset::new(vec![
        Change::AddNode { canvas: C, node: batch },
        Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes([50; 16]),
                from: Endpoint { node: id(5), port: "item".into() },
                to: Endpoint { node: id(2), port: "in".into() },
                order: 0,
            },
        },
    ])
    .apply(&mut p)
    .unwrap();
    p
}

#[test]
fn a_batch_runs_each_item_through_a_chain_and_records_its_items() {
    let mut p = batched();
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3, 4]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert_eq!(first.jobs.len(), 2, "one job per item, known up front");
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    assert!(run.is_finished());
    let result = |n: u16| finished.iter().find(|r| r.node == id(n)).unwrap().generation.clone();
    // One value per item, in item order, tagged with the item it came from.
    assert_eq!(result(2).outputs, vec![Value::List(vec![Value::Text("FOX".into()), Value::Text("OWL".into())])]);
    assert_eq!(result(3).outputs, result(2).outputs);
    assert_eq!(result(2).items.len(), 2);
    assert_eq!(result(3).items, result(2).items, "items keep their ids downstream");
    // A list input collects every item into one run.
    assert!(result(4).items.is_empty());
    assert_eq!(output(&finished, 4), "(FOX+OWL)");
    assert!(finished.iter().all(|r| r.generation.settings.is_empty()), "only provider jobs record settings");

    for result in &finished {
        let node = p.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
        Changeset::new(record_changes(C, &node, result.generation.clone()).unwrap()).apply(&mut p).unwrap();
    }
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
    // A recorded fanned result fans its consumers out again, without rerunning it.
    let plan = plan_of(&p, &[3]);
    assert_eq!(plan.steps.first().unwrap().fanout.as_ref().unwrap().items, result(2).items);
    // Editing the batch's items makes everything downstream stale.
    Change::SetParam { canvas: C, id: id(5), name: "text".into(), value: Some(Value::Text("fox\nbat".into())) }.apply(&mut p).unwrap();
    assert_eq!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap(), [id(2), id(3), id(4)].into_iter().collect());
}

#[test]
fn a_failed_item_fails_its_step_rather_than_dropping_a_row() {
    let p = batched();
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, &[id(2)].into_iter().collect());
    assert!(finished.is_empty(), "{finished:?}");
    assert!(failed.iter().any(|f| f.node == id(2)));
    assert!(run.is_finished());
}

#[test]
fn settings_too_large_to_keep_are_dropped_but_a_result_too_large_still_fails() {
    let job = |n: usize| nodecraft_graph::JobSettings {
        provider: "mock".into(),
        instance: None,
        model: "remote".into(),
        params: [("prompt".into(), Value::Text(format!("{n}{}", "x".repeat(1_000_000))))].into(),
        inputs: Vec::new(),
        item: None,
    };
    let small = Generation { settings: vec![job(0)], ..Generation::new(GenerationId::from_bytes([1; 16]), vec![Value::Text("made".into())]) };
    assert_eq!(super::recordable(small.clone()).unwrap(), small);
    let large = Generation { settings: (0..9).map(job).collect(), ..small.clone() };
    let kept = super::recordable(large).unwrap();
    assert!(kept.settings.is_empty() && kept.outputs == small.outputs, "the result is kept without its settings");
    let outputs = (0..9).map(|_| Value::Text("x".repeat(1_000_000))).collect();
    assert!(super::recordable(Generation { outputs, ..small }).is_err());
}
