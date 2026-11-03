//! What a run holds (#254): its canvas copy carries only active results, and the job outputs it
//! keeps are bounded by [`RunLimits::held_bytes`] (and [`MAX_HELD_OUTPUT_BYTES`] for local jobs'
//! outputs waiting to be taken).
use super::tests::{C, RUN, batched, drive, id, kinds, opts, output, plan_of, project};
use super::*;
use nodecraft_graph::Changeset;

/// Runs nodes 2 and 3 of `p` and records their results as the engine does.
fn run_and_record(p: &mut nodecraft_graph::Project, kinds: &Registry, run_id: u8, text: &str) {
    Change::SetParam { canvas: C, id: id(1), name: "text".into(), value: Some(Value::Text(text.into())) }.apply(p).unwrap();
    let plan = plan_of(p, &[2, 3]);
    // Each run its own id, so its generation ids are new.
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), kinds, opts([run_id; 16], 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, kinds, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    for result in &finished {
        let node = p.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
        Changeset::new(record_changes(C, &node, result.generation.clone()).unwrap()).apply(p).unwrap();
    }
}

#[test]
fn a_run_carries_only_active_results_and_hashes_them_as_the_live_canvas_does() {
    let mut p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    let kinds = kinds();
    // A long history on both nodes, the oldest generation active on 2.
    for n in 0..8u8 {
        run_and_record(&mut p, &kinds, n + 1, &format!("fox {n} {}", "x".repeat(4096)));
    }
    let oldest = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().history.first().unwrap().id;
    Change::SetActiveGeneration { canvas: C, id: id(2), generation: Some(oldest) }.apply(&mut p).unwrap();
    // The source back to what that oldest result was made from: 2 is current, 3 (made from 2's
    // newest result) is stale.
    let first_text = format!("fox 0 {}", "x".repeat(4096));
    Change::SetParam { canvas: C, id: id(1), name: "text".into(), value: Some(Value::Text(first_text)) }.apply(&mut p).unwrap();
    let live = p.canvases().get(&C).unwrap();
    assert_eq!(live.nodes().get(&id(2)).unwrap().history.len(), 8);

    // Both copies hold every node with exactly its active result, and nothing else changes.
    let slim = live.active_only();
    assert_eq!(slim, live.clone().into_active_only());
    slim.validate().unwrap();
    for (node_id, node) in live.nodes() {
        let copy = slim.nodes().get(node_id).unwrap();
        assert_eq!(copy.active, node.active);
        assert_eq!(copy.history.iter().map(|g| g.id).collect::<Vec<_>>(), node.active.into_iter().collect::<Vec<_>>());
        assert_eq!(Node { history: Vec::new(), ..copy.clone() }, Node { history: Vec::new(), ..node.clone() });
    }
    assert!(json_bytes(&slim) * 4 < json_bytes(live), "the history is most of the canvas");
    assert_eq!(snapshot(std::borrow::Cow::Borrowed(live)), slim);
    assert_eq!(snapshot(std::borrow::Cow::Owned(live.clone())), slim);

    // A run starts from that copy and hashes inputs as the live canvas does: 3 runs on 2's
    // active, oldest result, and recording what it made leaves nothing stale.
    assert_eq!(crate::staleness::stale(live, &kinds).unwrap(), [id(3)].into_iter().collect());
    let plan = plan_of(&p, &[3]);
    let (mut run, first) = Run::start(&plan, C, live, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert!(run.canvas.nodes().values().all(|n| n.history.len() <= 1), "the run copies no inactive generation");
    assert_eq!(run.canvas, slim);
    let (finished, failed, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    assert!(failed.is_empty(), "{failed:?}");
    assert!(output(&finished, 3).starts_with("((fox 0 "), "3 ran on 2's active result");
    let node = live.nodes().get(&id(3)).unwrap().clone();
    Changeset::new(record_changes(C, &node, finished[0].generation.clone()).unwrap()).apply(&mut p).unwrap();
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
}

#[test]
fn a_result_past_the_held_byte_limit_fails_its_step_and_saying_so() {
    let p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let outputs = vec![("out".to_owned(), Value::Text("y".repeat(1000)))];
    let bytes = json_bytes(&outputs);
    // Exactly the limit is held; one byte less fails the step, and the steps after it skip.
    for (cap, fits) in [(bytes, true), (bytes - 1, false)] {
        let limits = RunLimits { held_bytes: cap, ..RunLimits::default() };
        let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, limits)).unwrap();
        let progress = run.job_done(&kinds, first.jobs[0].id, outputs.clone());
        if fits {
            assert!(progress.failed.is_empty(), "{progress:?}");
            assert_eq!(progress.finished.len(), 1);
            assert_eq!(run.held_bytes, json_bytes(&progress.finished[0].generation.outputs), "only the step's kept output is held");
            continue;
        }
        assert!(progress.finished.is_empty());
        assert_eq!(progress.failed.iter().map(|f| f.node).collect::<Vec<_>>(), vec![id(2), id(3)]);
        assert!(progress.failed[0].reason.contains(&format!("past {cap} bytes of results")), "{}", progress.failed[0].reason);
        assert!(progress.failed[0].reason.contains("run fewer nodes together"), "the reason says what to do");
        // Not dropped: the failed step's own results are the host's to hold, as for any failure.
        assert!(progress.dropped.is_empty());
        assert_eq!(run.held_bytes, 0);
        assert!(run.is_finished());
        // A repeated report changes nothing.
        assert_eq!(run.job_done(&kinds, first.jobs[0].id, outputs.clone()), Progress::default());
    }
}

#[test]
fn held_bytes_are_given_back_when_outputs_are_used_or_let_go() {
    // 1 → 2 → 3: what 2 kept for 3 stays held; nothing waiting stays counted.
    let p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, _, _) = drive(&mut run, &kinds, first, &BTreeSet::new());
    let produced: usize = finished.iter().map(|r| json_bytes(&r.generation.outputs)).sum();
    assert_eq!(run.held_bytes, produced);
    assert!(run.jobs.values().all(|j| j.outputs.is_empty() && j.output_bytes == 0));

    // A step fanned out over a batch's two items, one job each: the first item's outputs are
    // held until the second item's job fails. That fails the step (a failed item fails its step),
    // and the first item's outputs are let go with it.
    let p = batched();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert_eq!(first.jobs.len(), 2);
    let Some(JobSpec::Source { output, value, .. }) = run.work(first.jobs[0].id).cloned() else { panic!("a source job") };
    let progress = run.job_done(&kinds, first.jobs[0].id, vec![(output, value)]);
    assert!(progress.finished.is_empty() && progress.failed.is_empty());
    assert!(run.held_bytes > 0, "a finished job's outputs are held until its step assembles them");
    let progress = run.job_failed(&kinds, first.jobs[1].id, "no");
    assert!(progress.failed.iter().any(|f| f.node == id(2)), "{progress:?}");
    assert_eq!(run.held_bytes, 0);
    assert!(run.jobs.values().all(|j| j.outputs.is_empty() && j.output_bytes == 0));
}

#[test]
fn local_outputs_waiting_to_be_taken_are_bounded() {
    let value = |n: usize| vec![("out".to_owned(), Value::Text("z".repeat(n)))];
    let one = json_bytes(&value(100));
    let outputs = Outputs::capped(2 * one);
    outputs.put(JobId(1), value(100)).unwrap();
    outputs.put(JobId(2), value(100)).unwrap();
    assert_eq!(outputs.held_bytes(), 2 * one);
    let refused = outputs.put(JobId(3), value(100)).unwrap_err();
    assert!(refused.contains("run fewer nodes together"), "{refused}");
    // Replacing a job's outputs counts only the new ones.
    outputs.put(JobId(2), value(100)).unwrap();
    assert_eq!(outputs.held_bytes(), 2 * one);
    assert_eq!(take_outputs(&outputs, JobId(1)), value(100));
    assert_eq!(outputs.held_bytes(), one);
    assert!(take_outputs(&outputs, JobId(1)).is_empty(), "taken once");
    outputs.put(JobId(3), value(100)).unwrap();
    assert_eq!(outputs.held_bytes(), 2 * one);
    assert_eq!(Outputs::default().0.lock().unwrap().cap, MAX_HELD_OUTPUT_BYTES);

    // The runner fails a local job whose output can't be held, rather than finishing it.
    let runner = SourceRunner { outputs: Outputs::capped(one), ..SourceRunner::default() };
    let run_one = |job: u64, text: String| {
        runner.register(JobId(job), JobSpec::Source { node: id(1), output: "out".into(), value: Value::Text(text) });
        let start = Start { job: JobId(job), lane: Lane::new(LOCAL_LANE), attempt: 1, idempotency_key: "k".into() };
        futures::executor::block_on(crate::driver::JobRunner::run(&runner, start, crate::driver::CancelToken::default()))
    };
    let outcome = run_one(9, "w".repeat(101));
    assert!(matches!(&outcome, Outcome::Failed { error, transient: false } if error.contains("bytes of results")), "{outcome:?}");
    assert_eq!(runner.outputs.held_bytes(), 0);
    assert!(matches!(run_one(10, "w".repeat(100)), Outcome::Done), "exactly the cap is held");
    assert_eq!(runner.outputs.held_bytes(), one);
}

/// Text in, list out, in three jobs: `a`, `b` and `c`.
struct Trio(nodecraft_nodes::NodeDescriptor);
impl nodecraft_nodes::NodeKind for Trio {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<nodecraft_nodes::ModeId> {
        nodecraft_nodes::ModeId::new("trio").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(["a", "b", "c"].into_iter().map(|t| JobSpec::Source { node: ctx.node.id, output: "items".into(), value: Value::Text(t.into()) }).collect())
    }
}

#[test]
fn a_variation_that_fails_partway_through_assembling_lets_go_of_all_its_outputs() {
    let mut kinds = kinds();
    let split = kinds.get("test.split").unwrap().descriptor().clone();
    kinds.register(Box::new(Trio(nodecraft_nodes::NodeDescriptor { id: "test.trio".into(), label: "Trio".into(), ..split }))).unwrap();
    // `project` builds with the default kinds, which lack Trio: add it by hand.
    let mut p = project(&[(1, "source.text", "fox")], &[]);
    let node = kinds.get("test.trio").unwrap().descriptor().instantiate(id(2), &Default::default()).unwrap();
    let edge = nodecraft_graph::Edge {
        id: nodecraft_types::EdgeId::from_bytes([7; 16]),
        from: nodecraft_graph::Endpoint { node: id(1), port: "out".into() },
        to: nodecraft_graph::Endpoint { node: id(2), port: "in".into() },
        order: 0,
    };
    Changeset::new(vec![Change::AddNode { canvas: C, node }, Change::Connect { canvas: C, edge }]).apply(&mut p).unwrap();
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    node.config.insert(crate::variations::VARIATIONS_CONFIG.into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let crate::planner::Planned::Ready(plan) =
        crate::planner::plan(canvas, &kinds, &crate::planner::RunMode::Selected, &[id(2)].into_iter().collect(), &BTreeSet::new()).unwrap()
    else {
        panic!("blocked")
    };
    let (mut run, first) = Run::start(&plan, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert_eq!(first.jobs.len(), 6, "three jobs for each of two variations");
    // The first job of the first variation reports an output its node doesn't have, so that
    // variation fails as it is assembled, before its other two jobs' outputs are read.
    let mut finished = Vec::new();
    let mut failed = Vec::new();
    for (n, job) in first.jobs.iter().enumerate() {
        let Some(JobSpec::Source { output, value, .. }) = run.work(job.id).cloned() else { panic!("a source job") };
        let port = if n == 0 { "nope".to_owned() } else { output };
        let progress = run.job_done(&kinds, job.id, vec![(port, value)]);
        finished.extend(progress.finished);
        failed.extend(progress.failed);
    }
    assert_eq!(finished.len(), 1, "the other variation is kept: {failed:?}");
    assert!(failed.iter().any(|f| f.reason.contains("1 of 2 variations")), "{failed:?}");
    assert!(run.jobs.values().all(|j| j.outputs.is_empty() && j.output_bytes == 0), "nothing waiting stays counted");
    assert_eq!(run.held_bytes, json_bytes(&finished[0].generation.outputs), "only the kept variation's result is held");
}
