use super::*;
use crate::planner::{Planned, RunMode, plan};
use nodecraft_graph::{Edge, Endpoint, Port};
use nodecraft_nodes::{Category, ModeId, NodeDescriptor, NodeKind};
use nodecraft_types::{EdgeId, Modality, ParamSchema, PortType};
use std::collections::BTreeSet;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };
const LIST: PortType = PortType { modality: Modality::Text, many: true };
const RUN: [u8; 16] = [0xab; 16];

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

fn kinds() -> Registry {
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
    kinds.register(Box::new(Split(split))).unwrap();
    kinds
}

fn id(n: u16) -> NodeId {
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

fn plan_of(project: &Project, requested: &[u16]) -> RunPlan {
    let canvas = project.canvases().get(&C).unwrap();
    match plan(canvas, &kinds(), &RunMode::Selected, &requested.iter().map(|n| id(*n)).collect(), &BTreeSet::new()).unwrap() {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => panic!("blocked: {blockers:?}"),
    }
}

/// Runs every job the run hands out (source jobs emit their value), failing those in `fail`.
fn drive(run: &mut Run<'_>, first: Progress, fail: &BTreeSet<NodeId>) -> (Vec<StepResult>, Vec<StepFailure>, Vec<scheduler::JobSpec>) {
    let (mut finished, mut failed, mut all) = (first.finished, first.failed, Vec::new());
    let mut queue = first.jobs;
    while let Some(job) = queue.pop() {
        all.push(job.clone());
        let JobSpec::Source { node, output, value } = run.work(job.id).unwrap().clone();
        let progress = if fail.contains(&node) { run.job_failed(job.id, "provider said no") } else { run.job_done(job.id, vec![(output, value)]) };
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
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
    assert_eq!(first.jobs.len(), 1, "only 2's jobs are known up front");
    let (finished, failed, _) = drive(&mut run, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    assert!(run.is_finished());
    assert_eq!(output(&finished, 2), "(fox)");
    assert_eq!(output(&finished, 3), "((fox))");
    // Recording the results as the engine will leaves nothing stale: the hashes are of the
    // inputs actually used.
    for result in &finished {
        let node = p.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
        assert!(result.generation.input_hash.is_some());
        Changeset::new(record_changes(C, &node, result.generation.clone())).apply(&mut p).unwrap();
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
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
    let (finished, failed, _) = drive(&mut run, first, &BTreeSet::new());
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
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
    let (finished, failed, jobs) = drive(&mut run, first, &[id(2)].into_iter().collect());
    assert!(finished.is_empty());
    assert_eq!(jobs.len(), 1, "nothing after 2 ran");
    assert_eq!(failed.iter().map(|f| f.node).collect::<Vec<_>>(), vec![id(2), id(3), id(4)]);
    assert!(failed[1].reason.starts_with("skipped"));
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
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
    let (finished, failed, _) = drive(&mut run, first, &BTreeSet::new());
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
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, run_id, 7, RetryPolicy::NEVER).unwrap();
        let key = first.jobs[0].idempotency_key.clone();
        let (finished, _, _) = drive(&mut run, first, &BTreeSet::new());
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
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
        let progress = run.job_done(first.jobs[0].id, outputs);
        assert_eq!(progress.failed.len(), 1, "{progress:?}");
        // A late or duplicate report changes nothing.
        assert_eq!(run.job_done(first.jobs[0].id, vec![]), Progress::default());
        assert_eq!(run.job_done(JobId(999), vec![]), Progress::default());
    }
}

#[test]
fn recording_a_result_past_the_history_limit_drops_the_oldest_inactive() {
    let mut node = Node::new(id(1), "test.join");
    for n in 0..MAX_HISTORY {
        node.history.push(Generation::new(GenerationId::from_bytes(u128::try_from(n + 1).unwrap().to_be_bytes()), vec![]));
    }
    node.active = node.history.first().map(|g| g.id);
    let changes = record_changes(C, &node, Generation::new(GenerationId::from_bytes([9; 16]), vec![]));
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
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
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
            scheduler::Event::Done { job } => run.job_done(job, take_outputs(&host.outputs, job)),
            scheduler::Event::Failed { job, error } => run.job_failed(job, &error),
            scheduler::Event::Cancelled { job } => run.job_failed(job, "cancelled"),
            _ => Progress::default(),
        };
        for job in &progress.jobs {
            if let Some(work) = run.work(job.id) {
                host.register(job.id, work.clone());
            }
        }
        if !progress.jobs.is_empty()
            && let Some(handle) = &handle
        {
            drop(handle.add(progress.jobs));
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
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, RUN, 1, RetryPolicy::NEVER).unwrap();
    assert!(first.jobs.is_empty());
    assert_eq!(first.failed.iter().map(|f| f.node).collect::<Vec<_>>(), vec![id(2)]);
    assert!(first.failed[0].reason.contains("more than 4096 jobs"));
    // An empty text splits into no jobs: an empty list, finished without running anything.
    assert_eq!(first.finished.iter().map(|r| (r.node, r.generation.outputs.clone())).collect::<Vec<_>>(), vec![(id(4), vec![Value::List(vec![])])]);
    assert!(run.is_finished());
    assert_eq!(run.job_failed(JobId(1), "late"), Progress::default());
}
