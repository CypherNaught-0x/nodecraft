//! Live recompute for action nodes (#88): when an edit leaves action nodes stale, the host plans
//! them (with whatever upstream they need), runs the plan off the UI thread and records the
//! results into the edit's own undo step ([`Session::record_derived`]).
//!
//! Only local work is ever run live: a plan with any non-local job (a model call, which costs
//! money) is left for an explicit run. The three steps are separate so the host can move the
//! middle one to a worker: [`pending`] (UI thread, cheap), [`compute`] (anywhere: it borrows
//! nothing from the session) and [`apply`] (UI thread).
use crate::Session;
use crate::action_exec::LocalActions;
use nodecraft_exec::planner::{Planned, RunMode, RunPlan, plan};
use nodecraft_exec::run::{Run, RunOptions};
use nodecraft_exec::scheduler::RetryPolicy;
use nodecraft_format::AssetStore;
use nodecraft_graph::{Canvas, Generation};
use nodecraft_nodes::{Category, JobSpec, Registry};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Live work for one revision of one canvas.
pub struct LiveJob {
    pub canvas: CanvasId,
    /// The revision it was planned from; results for another revision are dropped.
    pub base: u64,
    plan: RunPlan,
    live: Canvas,
    assets: AssetStore,
}

/// What a live run produced.
pub struct LiveOutcome {
    pub canvas: CanvasId,
    pub base: u64,
    pub results: Vec<(NodeId, Generation)>,
    pub failed: Vec<(NodeId, String)>,
    staged: AssetStore,
}

/// The live work `session`'s current document needs on `canvas`: its stale action nodes, if
/// they can run now with local work only. `skip` names nodes to leave alone (ones that just
/// failed at this revision).
pub fn pending(session: &Session, canvas: CanvasId, skip: &BTreeSet<NodeId>) -> Result<Option<LiveJob>, Error> {
    let kinds = session.kinds();
    let live = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let stale = nodecraft_exec::staleness::stale(live, kinds)?;
    let category = |id: &NodeId| live.nodes().get(id).and_then(|n| kinds.get(&n.kind).ok()).map(|k| k.descriptor().category);
    // Stale action nodes, and ones with no result yet.
    let wanted: BTreeSet<NodeId> = live
        .nodes()
        .iter()
        .filter(|(id, node)| category(id) == Some(Category::Action) && !skip.contains(id) && (node.active.is_none() || stale.contains(id)))
        .map(|(id, _)| *id)
        .collect();
    if wanted.is_empty() {
        return Ok(None);
    }
    // Plan each wanted node on its own terms: one that can't run yet (nothing connected, or a
    // model upstream with no result) mustn't hold the others back.
    let mut runnable = BTreeSet::new();
    for id in &wanted {
        if let Ok(Planned::Ready(plan)) = plan(live, kinds, &RunMode::WithUpstream, &BTreeSet::from([*id]), &stale)
            && local_only(&plan, live, kinds)
        {
            runnable.insert(*id);
        }
    }
    if runnable.is_empty() {
        return Ok(None);
    }
    let Planned::Ready(plan) = plan(live, kinds, &RunMode::WithUpstream, &runnable, &stale)? else { return Ok(None) };
    if !local_only(&plan, live, kinds) {
        return Ok(None);
    }
    Ok(Some(LiveJob { canvas, base: session.revision(), plan, live: live.clone(), assets: session.assets().clone() }))
}

/// Whether every step is a source, an action or structure: nothing that calls a model.
fn local_only(plan: &RunPlan, live: &Canvas, kinds: &Registry) -> bool {
    plan.steps.iter().all(|step| {
        let jobs_local = step.jobs.as_ref().is_none_or(|jobs| jobs.iter().all(|j| matches!(j, JobSpec::Source { .. } | JobSpec::Action { .. })));
        let kind_local = live.nodes().get(&step.node).and_then(|n| kinds.get(&n.kind).ok()).is_some_and(|k| k.descriptor().category != Category::Generation);
        jobs_local && kind_local
    })
}

/// Runs `job` (any thread): sources emit their values, actions run on [`LocalActions`].
pub fn compute(job: LiveJob, kinds: &Registry) -> Result<LiveOutcome, Error> {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&job.base.to_le_bytes());
    let options = RunOptions { id, first_job: 1, limits: Default::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(&job.plan, job.canvas, &job.live, kinds, options)?;
    let actions = LocalActions::new(job.assets);
    let mut queue: VecDeque<_> = first.jobs.into_iter().collect();
    let mut finished: BTreeMap<NodeId, Generation> = first.finished.into_iter().map(|r| (r.node, r.generation)).collect();
    let mut failed: Vec<(NodeId, String)> = first.failed.into_iter().map(|f| (f.node, f.reason)).collect();
    while let Some(scheduled) = queue.pop_front() {
        let progress = match run.work(scheduled.id).cloned() {
            Some(JobSpec::Source { output, value, .. }) => run.job_done(kinds, scheduled.id, vec![(output, value)]),
            Some(JobSpec::Action { action, params, inputs, output, .. }) => {
                match nodecraft_exec::run::ActionExecutor::execute(&actions, &action, &params, &inputs) {
                    Ok(value) => run.job_done(kinds, scheduled.id, vec![(output, value)]),
                    Err(reason) => run.job_failed(scheduled.id, &reason),
                }
            }
            _ => run.job_failed(scheduled.id, "live recompute runs only local work"),
        };
        queue.extend(progress.jobs);
        finished.extend(progress.finished.into_iter().map(|r| (r.node, r.generation)));
        failed.extend(progress.failed.into_iter().map(|f| (f.node, f.reason)));
    }
    // Sources re-emit their values: record only the action nodes' results.
    let actions_only =
        |id: &NodeId| job.live.nodes().get(id).and_then(|n| kinds.get(&n.kind).ok()).is_some_and(|k| k.descriptor().category == Category::Action);
    let results = finished.into_iter().filter(|(id, _)| actions_only(id)).collect();
    Ok(LiveOutcome { canvas: job.canvas, base: job.base, results, failed, staged: actions.take_staged() })
}

/// Records `outcome` if the document is still the one it was computed from. `Ok(false)` when
/// it is out of date (another recompute follows) or produced nothing.
pub fn apply(session: &mut Session, outcome: LiveOutcome) -> Result<bool, Error> {
    if outcome.results.is_empty() || session.revision() != outcome.base {
        return Ok(false);
    }
    match session.record_derived(outcome.canvas, outcome.results, &outcome.staged, outcome.base) {
        Ok(()) => Ok(true),
        Err(error) if error.kind == ErrorKind::Conflict => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
