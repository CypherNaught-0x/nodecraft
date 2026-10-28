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
        assert!(progress.failed[0].reason.contains(&format!("hold more than {cap} bytes of results")), "{}", progress.failed[0].reason);
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

    // A batch step, one job per item: the first item's outputs are held until the second's job
    // fails, which fails the step, and they are let go with it.
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
    let outputs = Outputs::default();
    let value = |n: usize| vec![("out".to_owned(), Value::Text("z".repeat(n)))];
    let one = json_bytes(&value(100));
    outputs.put_within(JobId(1), value(100), 2 * one).unwrap();
    outputs.put_within(JobId(2), value(100), 2 * one).unwrap();
    assert_eq!(outputs.held_bytes(), 2 * one);
    let refused = outputs.put_within(JobId(3), value(100), 2 * one).unwrap_err();
    assert!(refused.contains("run fewer nodes together"), "{refused}");
    // Replacing a job's outputs counts only the new ones.
    outputs.put_within(JobId(2), value(100), 2 * one).unwrap();
    assert_eq!(outputs.held_bytes(), 2 * one);
    assert_eq!(take_outputs(&outputs, JobId(1)), value(100));
    assert_eq!(outputs.held_bytes(), one);
    assert!(take_outputs(&outputs, JobId(1)).is_empty(), "taken once");
    outputs.put_within(JobId(3), value(100), 2 * one).unwrap();
    assert_eq!(outputs.held_bytes(), 2 * one);

    // The runner fails a local job whose output can't be held, rather than finishing it.
    let runner = SourceRunner::default();
    let job = JobSpec::Source { node: id(1), output: "out".into(), value: Value::Text("w".repeat(MAX_HELD_OUTPUT_BYTES)) };
    runner.register(JobId(9), job);
    let start = Start { job: JobId(9), lane: Lane::new(LOCAL_LANE), attempt: 1, idempotency_key: "k".into() };
    let outcome = futures::executor::block_on(crate::driver::JobRunner::run(&runner, start, crate::driver::CancelToken::default()));
    assert!(matches!(&outcome, Outcome::Failed { error, transient: false } if error.contains("MiB of results")), "{outcome:?}");
    assert_eq!(runner.outputs.held_bytes(), 0);
}
