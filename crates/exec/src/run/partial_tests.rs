//! A batch whose items partly failed keeps the items that finished (#480).
use super::tests::{C, RUN, batched, id, kinds, opts, plan_of};
use super::*;
use nodecraft_graph::{Changeset, Project};

/// Runs every job the run hands out; `end` says which to fail (`Some(false)`) or cancel
/// (`Some(true)`) by the text they would emit. Returns the results, failures and jobs.
fn drive(run: &mut Run, kinds: &Registry, first: Progress, end: impl Fn(&str) -> Option<bool>) -> (Vec<StepResult>, Vec<StepFailure>, Vec<scheduler::JobSpec>) {
    let (mut finished, mut failed, mut all) = (first.finished, first.failed, Vec::new());
    let mut queue = first.jobs;
    while let Some(job) = queue.pop() {
        all.push(job.clone());
        let Some(JobSpec::Source { output, value, .. }) = run.work(job.id).cloned() else { panic!("only source jobs here") };
        let Value::Text(text) = &value else { panic!("text jobs only") };
        let progress = match end(text) {
            Some(true) => run.job_cancelled(kinds, job.id, "cancelled"),
            Some(false) => run.job_failed(kinds, job.id, "provider said no"),
            None => run.job_done(kinds, job.id, vec![(output, value)]),
        };
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

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

/// The key of node 2's job for `item` among `jobs`, found by the run that handed them out.
fn key_of(run: &Run, jobs: &[scheduler::JobSpec], item: &str) -> String {
    jobs.iter()
        .find(|j| matches!(run.work(j.id), Some(JobSpec::Source { node, value: Value::Text(t), .. }) if *node == id(2) && t == item))
        .unwrap()
        .idempotency_key
        .clone()
}

#[test]
fn a_failed_item_keeps_the_others_and_running_again_runs_only_it() {
    let mut p = batched();
    let kinds = kinds();
    let plan = plan_of(&p, &[2, 3]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, first, |t| (t == "OWL").then_some(false));
    assert!(run.is_finished());
    let first_key = key_of(&run, &jobs, "OWL");
    // The finished item is kept; the failed one is an empty list in its place.
    let [partial] = finished.as_slice() else { panic!("{finished:?}") };
    assert_eq!(partial.node, id(2));
    assert_eq!(partial.generation.outputs, vec![Value::List(vec![text("FOX"), Value::List(Vec::new())])]);
    let incomplete = partial.generation.incomplete.clone().unwrap();
    assert_eq!(incomplete.missing, vec![MissingItem { item: 1, why: Missing::Failed }]);
    // It still says what went wrong, and what needs every item doesn't run.
    let reasons: Vec<_> = failed.iter().map(|f| (f.node, f.reason.as_str())).collect();
    assert!(reasons.contains(&(id(2), "1 of 2 items didn't finish: provider said no")), "{reasons:?}");
    assert!(reasons.iter().any(|(node, reason)| *node == id(3) && reason.ends_with("didn't produce a result for every item")), "{reasons:?}");
    record(&mut p, &finished);
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().contains(&id(2)), "a partial result waits to be completed");

    // Running again plans only the missing item, under a fresh key: the provider failed the old one.
    let plan = plan_of(&p, &[2, 3]);
    let step = plan.steps.iter().find(|s| s.node == id(2)).unwrap();
    let fanout = step.fanout.as_ref().unwrap();
    assert_eq!(fanout.jobs, vec![0, 1]);
    assert_eq!(step.jobs.as_ref().unwrap().len(), 1);
    assert_eq!(fanout.resume.as_ref().and_then(|r| r.generation), Some(partial.generation.id));
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts([0xcd; 16], 1, RunLimits::default())).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, first, |_| None);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(jobs.iter().filter(|j| matches!(run.work(j.id), Some(JobSpec::Source { node, .. }) if *node == id(2))).count(), 1, "the kept item doesn't run");
    assert_ne!(key_of(&run, &jobs, "OWL"), first_key);
    let result = |n: u16| finished.iter().find(|r| r.node == id(n)).unwrap().generation.clone();
    assert_eq!(result(2).outputs, vec![Value::List(vec![text("FOX"), text("OWL")])]);
    assert!(result(2).incomplete.is_none(), "completed");
    assert_eq!(result(2).items, partial.generation.items, "items keep their ids");
    assert_eq!(result(3).outputs, result(2).outputs, "what comes after runs on every item");
    record(&mut p, &finished);
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
}

#[test]
fn a_cancelled_item_is_shown_as_cancelled_and_runs_again_under_a_fresh_key() {
    let mut p = batched();
    let kinds = kinds();
    let plan = plan_of(&p, &[2]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, first, |t| (t == "OWL").then_some(true));
    let first_key = key_of(&run, &jobs, "OWL");
    assert_eq!(failed.iter().map(|f| f.reason.as_str()).collect::<Vec<_>>(), ["cancelled"], "shown as cancelled, not failed");
    let missing = &finished.first().unwrap().generation.incomplete.as_ref().unwrap().missing;
    assert_eq!(missing, &[MissingItem { item: 1, why: Missing::Cancelled }]);
    record(&mut p, &finished);

    let plan = plan_of(&p, &[2]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts([0xcd; 16], 1, RunLimits::default())).unwrap();
    let (finished, _, jobs) = drive(&mut run, &kinds, first, |_| None);
    assert_ne!(key_of(&run, &jobs, "OWL"), first_key, "under the old key a provider answers with the cancel");
    assert!(finished.first().unwrap().generation.incomplete.is_none());
}

#[test]
fn a_partial_result_with_other_inputs_now_runs_in_full_and_none_finished_fails_the_step() {
    let mut p = batched();
    let kinds = kinds();
    let plan = plan_of(&p, &[2]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, _, _) = drive(&mut run, &kinds, first, |t| (t == "OWL").then_some(false));
    record(&mut p, &finished);
    Change::SetParam { canvas: C, id: id(5), name: "text".into(), value: Some(text("fox\nowl\nbat")) }.apply(&mut p).unwrap();
    let plan = plan_of(&p, &[2]);
    let step = plan.steps.iter().find(|s| s.node == id(2)).unwrap();
    assert!(step.fanout.as_ref().unwrap().resume.is_none());
    assert_eq!(step.jobs.as_ref().unwrap().len(), 3, "every item runs");

    // When no item finishes, the step fails as before and records nothing.
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts([0xcd; 16], 1, RunLimits::default())).unwrap();
    let (finished, failed, _) = drive(&mut run, &kinds, first, |_| Some(false));
    assert!(finished.is_empty());
    assert_eq!(failed.iter().map(|f| f.reason.as_str()).collect::<Vec<_>>(), ["provider said no"]);
}

#[test]
fn an_item_with_several_jobs_fails_alone_and_its_siblings_results_are_let_go() {
    // A batch of two into a kind that plans three jobs per item.
    let mut kinds = kinds();
    let pieces = nodecraft_nodes::NodeDescriptor {
        id: "test.pieces".into(),
        label: "Pieces".into(),
        category: nodecraft_nodes::Category::Generation,
        inputs: vec![nodecraft_graph::Port {
            name: "in".into(),
            port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: false },
        }],
        outputs: vec![nodecraft_graph::Port {
            name: "items".into(),
            port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: true },
        }],
        params: Default::default(),
        quick_add: None,
    };
    kinds.register(Box::new(super::tests::Pieces(pieces))).unwrap();
    let mut p = batched();
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    node.kind = "test.pieces".into();
    node.outputs =
        vec![nodecraft_graph::Port { name: "items".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: true } }];
    let out = p.canvases().get(&C).unwrap().edges().values().filter(|e| e.from.node == id(2)).map(|e| e.id).collect::<Vec<_>>();
    let mut changes: Vec<Change> = out.into_iter().map(|id| Change::Disconnect { canvas: C, id }).collect();
    changes.push(Change::SetNode { canvas: C, node });
    Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let crate::planner::Planned::Ready(plan) =
        crate::planner::plan(canvas, &kinds, &crate::planner::RunMode::Selected, &[id(2)].into_iter().collect(), &Default::default()).unwrap()
    else {
        panic!("blocked")
    };
    let (mut run, first) = Run::start(&plan, C, canvas, &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let ids: Vec<JobId> = first.jobs.iter().map(|j| j.id).collect();
    assert_eq!(ids.len(), 6);
    let done = |n: &str| vec![("items".to_owned(), text(n))];
    assert_eq!(run.job_done(&kinds, ids[0], done("0")), Progress::default());
    // Item 0's second job fails: only its own unfinished sibling is cancelled, and its finished
    // sibling's result is let go for the host to hold (#611).
    let progress = run.job_failed(&kinds, ids[1], "provider said no");
    assert_eq!(progress.cancel, vec![ids[2]]);
    assert_eq!(progress.dropped, vec![ids[0]]);
    assert!(progress.failed.is_empty() && progress.finished.is_empty(), "item 1 is still running");
    assert!(run.is_running(id(2)) && !run.has_failed(id(2)));
    // The cancelled sibling's late result is let go too, once.
    assert_eq!(run.job_done(&kinds, ids[2], done("2")), Progress { dropped: vec![ids[2]], ..Progress::default() });
    assert_eq!(run.job_done(&kinds, ids[2], done("2")), Progress::default());
    let mut last = Progress::default();
    for (n, job) in ids[3..].iter().enumerate() {
        last = run.job_done(&kinds, *job, done(&n.to_string()));
    }
    assert!(run.is_finished());
    let [result] = last.finished.as_slice() else { panic!("{last:?}") };
    let incomplete = result.generation.incomplete.as_ref().unwrap();
    assert_eq!(incomplete.missing, vec![MissingItem { item: 0, why: Missing::Failed }]);
    assert_eq!(result.generation.outputs, vec![Value::List(vec![Value::List(Vec::new()), Value::List(vec![text("0"), text("1"), text("2")])])]);
    assert_eq!(last.failed.iter().map(|f| f.reason.as_str()).collect::<Vec<_>>(), ["1 of 2 items didn't finish: provider said no"]);
}

#[test]
fn a_resumed_item_that_fails_again_keeps_the_finished_ones() {
    let mut p = batched();
    let kinds = kinds();
    let plan = plan_of(&p, &[2]);
    let (mut run, first) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts(RUN, 1, RunLimits::default())).unwrap();
    let (finished, _, _) = drive(&mut run, &kinds, first, |t| (t == "OWL").then_some(false));
    record(&mut p, &finished);
    let first = finished.first().unwrap().generation.clone();

    let plan = plan_of(&p, &[2]);
    let (mut run, start) = Run::start(&plan, C, p.canvases().get(&C).unwrap(), &kinds, opts([0xcd; 16], 1, RunLimits::default())).unwrap();
    let (finished, failed, jobs) = drive(&mut run, &kinds, start, |t| (t == "OWL").then_some(false));
    assert_eq!(jobs.len(), 1, "only the owl ran");
    assert_eq!(failed.iter().map(|f| f.reason.as_str()).collect::<Vec<_>>(), ["1 of 2 items didn't finish: provider said no"]);
    let again = &finished.first().unwrap().generation;
    assert_ne!(again.id, first.id);
    assert_eq!((&again.outputs, &again.items, &again.incomplete), (&first.outputs, &first.items, &first.incomplete), "the fox is kept as it was");
    assert_eq!(again.settings, first.settings);
}
