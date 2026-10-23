//! Cached results a run reuses instead of running (#91). `run.quote` and `run.start` look for
//! them among the steps they planned, using [`crate::cache`], and plan again with those nodes
//! settled: a run never quotes, admits or pays for a node with caching on whose identical result
//! is already in the project, whether it is already active or only has to be selected or copied.
//! `run.start` applies the reused results as their own undo step before the run goes to the host.
//!
//! Technique nodes never reuse in a run: a technique runs its version's nodes, which may call a
//! model, and its own result would be reused in their place. Live recompute reuses only those
//! made of local work.

use crate::cache::{self, Hit};
use nodecraft_exec::planner::RunPlan;
use nodecraft_graph::{Change, Changeset, Project};
use nodecraft_nodes::{Registry, technique};
use nodecraft_types::{CanvasId, Error, NodeId};
use std::collections::BTreeSet;

/// The results a run reuses, and the project with them in place, which the run is planned on.
pub(super) struct Reuse {
    /// Every node that doesn't run: given a cached result, or holding it already.
    settled: BTreeSet<NodeId>,
    /// The changes giving the nodes their cached results; empty when every one already holds it.
    pub changes: Vec<Change>,
    /// The project with `changes` applied; `None` when there are none.
    project: Option<Project>,
}

impl Reuse {
    /// The nodes that don't run.
    pub fn settled(&self) -> &BTreeSet<NodeId> {
        &self.settled
    }
    /// The nodes that don't run, as a quote lists them.
    pub fn reused(&self) -> Vec<NodeId> {
        self.settled.iter().copied().take(super::MAX_LISTED).collect()
    }
    /// Every node that doesn't run, as the start reports them (at most [`cache::MAX_HITS`] of
    /// them get a result).
    pub fn all(&self) -> Vec<NodeId> {
        self.settled.iter().copied().collect()
    }
    /// The project the run is planned on: `document` with the reused results in place.
    pub fn project<'a>(&'a self, document: &'a Project) -> &'a Project {
        self.project.as_ref().unwrap_or(document)
    }
}

/// The nodes that don't run, the hits among them and the changes applying those.
type Picked = (BTreeSet<NodeId>, Vec<Hit>, Vec<Change>);

/// The steps that reuse, given the nodes that may: a node reuses only when everything it reads in
/// this run reuses too, since one fed by a step that runs would be stale as soon as that step
/// finished. Steps are in dependency order, so a step's `after` is decided by the time it is
/// reached.
fn settle(plan: &RunPlan, may: impl Fn(&NodeId) -> bool) -> BTreeSet<NodeId> {
    let mut settled = BTreeSet::new();
    for step in &plan.steps {
        if may(&step.node) && step.after.iter().all(|before| settled.contains(before)) {
            settled.insert(step.node);
        }
    }
    settled
}

/// The cached results `plan`'s steps on `canvas` can reuse, if any. Nodes inlined from a
/// technique aren't on the document and never reuse, nor do technique nodes (see the module
/// docs). The changes stay within what one undo step may hold: past it, the latest hits in
/// dependency order run instead.
pub(super) fn find(document: &Project, canvas: CanvasId, kinds: &Registry, plan: &RunPlan) -> Result<Option<Reuse>, Error> {
    let Some(on) = document.canvases().get(&canvas) else { return Ok(None) };
    let candidates: Vec<NodeId> = plan
        .steps
        .iter()
        .map(|step| step.node)
        .filter(|id| !plan.inner.contains_key(id) && on.nodes().get(id).is_some_and(|node| node.kind != technique::ID && cache::enabled(node, kinds)))
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    let found = cache::lookup(document, canvas, kinds, candidates, cache::fresh_id)?;
    if found.hits.is_empty() && found.current.is_empty() {
        return Ok(None);
    }
    // Hit nodes in step order, so the budget below drops the latest first.
    let order: Vec<NodeId> = plan.steps.iter().map(|step| step.node).filter(|id| found.hits.iter().any(|hit| hit.node() == *id)).collect();
    let hit_of = |id: &NodeId| found.hits.iter().find(|hit| hit.node() == *id);
    let pick = |keep: usize| -> Result<Picked, Error> {
        let allowed: BTreeSet<NodeId> = order.iter().take(keep).copied().collect();
        let settled = settle(plan, |id| allowed.contains(id) || found.current.contains(id));
        let hits: Vec<Hit> = order.iter().filter(|id| settled.contains(id)).filter_map(|id| hit_of(id).cloned()).collect();
        let changes = cache::changes(canvas, on, &hits)?;
        Ok((settled, hits, changes))
    };
    let fits = |changes: &[Change]| Changeset::new(changes.to_vec()).validate_replay_budget(crate::session::MAX_HISTORY_BYTES.saturating_sub(12)).is_ok();
    let mut picked = pick(order.len())?;
    if !fits(&picked.2) {
        // The most hits, in step order, whose changes fit (none always fit): a binary search.
        let (mut low, mut high) = (0usize, order.len().saturating_sub(1));
        while low < high {
            let mid = low.saturating_add(high).saturating_add(1) / 2;
            if fits(&pick(mid)?.2) {
                low = mid;
            } else {
                high = mid.saturating_sub(1);
            }
        }
        picked = pick(low)?;
    }
    let (settled, hits, changes) = picked;
    if settled.is_empty() {
        return Ok(None);
    }
    // The look's own copy has every hit applied; reuse it unless some were left to run.
    let project = if changes.is_empty() {
        None
    } else if hits.len() == found.hits.len()
        && let Some(project) = found.project
    {
        Some(project)
    } else {
        let mut project = document.clone();
        Changeset::new(changes.clone()).apply(&mut project)?;
        Some(project)
    };
    Ok(Some(Reuse { settled, changes, project }))
}
