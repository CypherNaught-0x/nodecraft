//! A node asked for several variations makes each in jobs of its own and records each as a
//! generation of its own (#81).
use super::tests::{C, RUN, admitted_calls, batched, calls_project, id, kinds, opts, output, plan_of, project};
use super::*;
use crate::planner::{Planned, RunMode, plan};
use crate::variations::VARIATIONS_CONFIG;
use nodecraft_graph::{Changeset, Project};
use nodecraft_types::Unsigned;

fn vary(p: &mut Project, n: u16, count: u64) {
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().clone();
    node.config.insert(VARIATIONS_CONFIG.into(), Value::Unsigned(Unsigned(count)));
    Change::SetNode { canvas: C, node }.apply(p).unwrap();
}

/// Runs every job the run hands out, in the order handed out, failing the `fail`th (from 0)
/// of them, when given, rather than finishing it.
fn drive(run: &mut Run, kinds: &Registry, first: Progress, fail: Option<usize>) -> (Vec<StepResult>, Vec<StepFailure>, Vec<scheduler::JobSpec>) {
    let (mut finished, mut failed, mut all) = (first.finished, first.failed, Vec::new());
    let mut queue: std::collections::VecDeque<_> = first.jobs.into();
    while let Some(job) = queue.pop_front() {
        let n = all.len();
        all.push(job.clone());
        let Some(JobSpec::Source { output, value, .. }) = run.work(job.id).cloned() else { panic!("only source jobs here") };
        let progress = if Some(n) == fail { run.job_failed(kinds, job.id, "provider said no") } else { run.job_done(kinds, job.id, vec![(output, value)]) };
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
        failed.extend(progress.failed);
    }
    (finished, failed, all)
}

fn record(p: &mut Project, results: &[StepResult]) {
    for result in results {
        let node = p.canvases().get(&C).unwrap().nodes().get(&result.node).unwrap().clone();
        Changeset::new(record_changes(C, &node, result.generation.clone()).unwrap()).apply(p).unwrap();
    }
}

/// 1 "fox" → join 2 → join 3, with 2 asked for three variations.
fn chain() -> Project {
    let mut p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    vary(&mut p, 2, 3);
    p
}

#[test]
fn each_variation_is_its_own_job_and_its_own_generation() {
    let mut p = chain();
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let step = plan.steps.iter().find(|s| s.node == id(2)).unwrap();
    assert_eq!(step.variations, 3);
    assert_eq!(step.jobs.as_ref().map(Vec::len), Some(3), "one job per variation, known up front");
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    assert_eq!(first.jobs.len(), 3);
    let keys: BTreeSet<&str> = first.jobs.iter().map(|j| j.idempotency_key.as_str()).collect();
    assert_eq!(keys.len(), 3, "a key per variation, so each is its own charge");
    let (finished, failed, _) = drive(&mut run, &kinds, first, None);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(run.is_finished());
    let ours: Vec<&StepResult> = finished.iter().filter(|r| r.node == id(2)).collect();
    assert_eq!(ours.len(), 3);
    let ids: BTreeSet<GenerationId> = ours.iter().map(|r| r.generation.id).collect();
    assert_eq!(ids.len(), 3, "each variation is a generation of its own");
    assert!(ours.iter().all(|r| r.generation.outputs == vec![Value::Text("(fox)".into())] && r.generation.input_hash.is_some()));
    // What comes after runs once, on the last variation.
    assert_eq!(finished.iter().filter(|r| r.node == id(3)).count(), 1);
    assert_eq!(output(&finished, 3), "((fox))");
    // Recorded one after another, every variation joins the history and the last is active.
    record(&mut p, &finished);
    let node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap();
    assert_eq!(node.history.len(), 3);
    assert_eq!(node.active, ours.last().map(|r| r.generation.id));
    // The count isn't part of what a node makes: changing it leaves nothing stale.
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
    vary(&mut p, 2, 5);
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
}

#[test]
fn the_first_variation_keeps_the_id_a_single_result_has() {
    let kinds = kinds();
    let single = project(&[(1, "source.text", "fox"), (2, "test.join", "")], &[(1, "out", 2, 0)]);
    let (mut run, first) = Run::start(&plan_of(&single, &[2]), C, single.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (one, _, _) = drive(&mut run, &kinds, first, None);
    let mut varied = single.clone();
    vary(&mut varied, 2, 2);
    let (mut run, first) = Run::start(&plan_of(&varied, &[2]), C, varied.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (two, _, _) = drive(&mut run, &kinds, first, None);
    assert_eq!(two.len(), 2);
    assert_eq!(two.first().map(|r| r.generation.id), one.first().map(|r| r.generation.id));
}

#[test]
fn a_failed_variation_keeps_the_others_and_what_comes_after_runs() {
    let p = chain();
    let kinds = kinds();
    let (mut run, first) = Run::start(&plan_of(&p, &[2, 3]), C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, Some(1));
    assert_eq!(finished.iter().filter(|r| r.node == id(2)).count(), 2, "{finished:?}");
    assert_eq!(output(&finished, 3), "((fox))", "the step is done, so what comes after it runs");
    let [failure] = failed.as_slice() else { panic!("{failed:?}") };
    assert_eq!(failure.node, id(2));
    assert_eq!(failure.reason, "1 of 3 variations didn't finish: provider said no");
    assert!(!run.has_failed(id(2)));
}

#[test]
fn a_step_fails_only_when_every_variation_does() {
    let mut p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    vary(&mut p, 2, 1);
    let kinds = kinds();
    // One variation is a plain step: its one job failing fails it.
    let (mut run, first) = Run::start(&plan_of(&p, &[2, 3]), C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, Some(0));
    assert!(finished.is_empty());
    assert_eq!(failed.len(), 2, "{failed:?}");
    vary(&mut p, 2, 2);
    let (mut run, first) = Run::start(&plan_of(&p, &[2, 3]), C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let mut queue: Vec<_> = first.jobs;
    let mut failed = Vec::new();
    while let Some(job) = queue.pop() {
        let progress = run.job_failed(&kinds, job.id, "provider said no");
        queue.extend(progress.jobs);
        failed.extend(progress.failed);
    }
    assert!(run.has_failed(id(2)) && run.has_failed(id(3)));
    assert_eq!(failed.first().map(|f| f.reason.as_str()), Some("provider said no"));
}

#[test]
fn a_deferred_step_plans_its_variations_once_its_inputs_are_made() {
    // 1 "fox" → join 2 → join 3, with 3 asked for four variations: its jobs wait on 2.
    let mut p = project(&[(1, "source.text", "fox"), (2, "test.join", ""), (3, "test.join", "")], &[(1, "out", 2, 0), (2, "out", 3, 0)]);
    vary(&mut p, 3, 4);
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let step = plan.steps.iter().find(|s| s.node == id(3)).unwrap();
    assert!(step.jobs.is_none());
    assert_eq!(step.variations, 4);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, first, None);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(jobs.len(), 5);
    assert_eq!(finished.iter().filter(|r| r.node == id(3)).count(), 4);
}

#[test]
fn a_variation_of_one_job_has_a_resume_recipe_for_its_own_generation() {
    let (kinds, mut p) = calls_project();
    vary(&mut p, 2, 3);
    let (mut run, first) = admitted_calls(&p, &kinds, &[2]);
    assert_eq!(first.jobs.len(), 3);
    let recipes: Vec<GenerationId> = first.jobs.iter().map(|j| run.resume_recipe(&kinds, j.id).unwrap().generation).collect();
    let mut made = Vec::new();
    for job in &first.jobs {
        made.extend(run.job_done(&kinds, job.id, vec![("out".into(), Value::Text("made".into()))]).finished);
    }
    assert_eq!(recipes, made.iter().map(|r| r.generation.id).collect::<Vec<_>>());
}

#[test]
fn variations_dont_combine_with_a_batch() {
    let mut p = batched();
    vary(&mut p, 2, 3);
    let canvas = p.canvases().get(&C).unwrap();
    let requested = [id(2)].into_iter().collect();
    match plan(canvas, &kinds(), &RunMode::Selected, &requested, &BTreeSet::new()).unwrap() {
        Planned::Blocked(blockers) => assert!(blockers.iter().any(|b| b.node == id(2)), "{blockers:?}"),
        Planned::Ready(_) => panic!("a batch with variations planned"),
    }
}

#[test]
fn source_kinds_ignore_a_variation_count() {
    let mut p = project(&[(1, "source.text", "fox")], &[]);
    vary(&mut p, 1, 3);
    let plan = plan_of(&p, &[1]);
    assert_eq!(plan.steps.first().map(|s| s.variations), Some(1));
}
