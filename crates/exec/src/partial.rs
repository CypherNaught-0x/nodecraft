//! Batches whose items partly failed (#480). A fanned step keeps the items that finished when
//! others fail: it records a partial result ([`nodecraft_graph::Incomplete`]) instead of
//! throwing paid results away. Running the node again while its inputs are unchanged completes
//! that result: the planner keeps the finished items and plans only the missing ones
//! ([`Resume`]), so the items already paid for aren't charged again. A missing item runs under
//! new idempotency keys: what a provider holds under its old ones is the failure or the cancel.
//!
//! A technique node fanned out by a batch (#533) keeps its finished items the same way (#536).
//! Its inlined nodes have no results of their own, so running it again plans only the missing
//! items of the inlined steps fanned over its items ([`resume_techniques`]); the rest are copied
//! from the technique node's partial result.

use crate::planner::{InputSource, Step};
use crate::staleness::{InputHasher, MAX_QUERY_BYTES};
use nodecraft_graph::{Canvas, MissingItem};
use nodecraft_nodes::Registry;
use nodecraft_types::{Blake3, GenerationId, NodeId};
use std::collections::{BTreeMap, BTreeSet};

/// Completing a node's partial result: the finished items are copied from it, and only the
/// missing ones run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    /// The partial result whose finished items are kept; `None` for a step inlined from a
    /// technique, whose technique node keeps them.
    pub generation: Option<GenerationId>,
    /// Its missing items, which this run plans.
    pub missing: Vec<MissingItem>,
}

impl Resume {
    /// Whether item `index` is one this run plans.
    pub fn runs(&self, index: usize) -> bool {
        // Validation keeps `missing` sorted and unique.
        u32::try_from(index).is_ok_and(|index| self.missing.binary_search_by_key(&index, |m| m.item).is_ok())
    }
}

/// Plans only the missing items of fanned steps whose node shows a partial result made from
/// the inputs it has now. Only steps whose jobs are known up front qualify: a step fed by this
/// run can't know its inputs are unchanged until they are made. Anything that doesn't match
/// exactly (other items, another port layout, inputs that can't be hashed) runs in full.
pub(crate) fn resume_partials(canvas: &Canvas, kinds: &Registry, steps: &mut [Step]) {
    let mut hasher = Hashed::default();
    for step in steps {
        let (Some(jobs), Some(fanout)) = (&mut step.jobs, &mut step.fanout) else { continue };
        let Some(resume) = partial(canvas, kinds, &mut hasher, step.node, &fanout.items) else { continue };
        if fanout.jobs.len() != fanout.items.len() {
            continue;
        }
        prune(jobs, &mut fanout.jobs, &resume);
        fanout.resume = Some(resume);
    }
}

/// Plans only the missing items of technique nodes fanned out by a batch whose partial result
/// was made from the inputs they have now (#536). Each step inlined for one runs only those
/// items when nothing else in the run feeds them and every inlined step either is fanned over
/// the technique node's items, each item reading only the same item of the steps before it, or
/// runs once, for free, without reading a fanned one (so it runs again in full and makes what it
/// made before). Anything else runs in full: an item that reads other items, or a paid step
/// shared by every item, would make the missing items from results this run doesn't have.
pub(crate) fn resume_techniques(canvas: &Canvas, kinds: &Registry, steps: &mut [Step], inner: &BTreeMap<NodeId, NodeId>) {
    let owners: BTreeSet<NodeId> = inner.values().copied().collect();
    let mut hasher = Hashed::default();
    for owner in owners {
        let Some(at) = steps.iter().position(|s| s.node == owner) else { continue };
        let Some(step) = steps.get(at) else { continue };
        let (None, Some(fanout)) = (&step.jobs, &step.fanout) else { continue };
        let items = fanout.items.clone();
        let Some(resume) = partial(canvas, kinds, &mut hasher, owner, &items) else { continue };
        let group: BTreeSet<NodeId> = steps.iter().filter(|s| inner.get(&s.node) == Some(&owner)).map(|s| s.node).chain([owner]).collect();
        let fanned: BTreeSet<NodeId> = steps.iter().filter(|s| group.contains(&s.node) && s.fanout.is_some()).map(|s| s.node).collect();
        let fits = steps.iter().filter(|s| group.contains(&s.node)).all(|s| {
            let fed_inside = s.after.iter().all(|before| group.contains(before));
            let aligned = match &s.fanout {
                Some(f) => f.items == items && s.jobs.as_ref().is_none_or(|_| f.jobs.len() == items.len()) && own_items(f, &fanned),
                None => !s.after.iter().any(|before| fanned.contains(before)) && s.jobs.as_ref().is_some_and(|jobs| jobs.iter().all(free)),
            };
            fed_inside && aligned
        });
        if !fits {
            continue;
        }
        for s in steps.iter_mut().filter(|s| fanned.contains(&s.node)) {
            let Some(fanout) = &mut s.fanout else { continue };
            let resume = Resume { generation: if s.node == owner { resume.generation } else { None }, missing: resume.missing.clone() };
            if let Some(jobs) = &mut s.jobs {
                prune(jobs, &mut fanout.jobs, &resume);
            }
            fanout.resume = Some(resume);
        }
    }
}

/// Whether each item of `fanout` reads only the same item of the fanned steps in `fanned`.
fn own_items(fanout: &crate::planner::Fanout, fanned: &BTreeSet<NodeId>) -> bool {
    fanout.cells.iter().enumerate().all(|(cell, inputs)| {
        inputs.iter().flat_map(|input| &input.from).all(|source| match source {
            InputSource::Item { node, index, .. } => !fanned.contains(node) || *index == cell,
            InputSource::Step { node, .. } => !fanned.contains(node),
            InputSource::Value(_) => true,
        })
    })
}

/// Whether a job costs nothing and makes the same result each time it runs.
fn free(job: &nodecraft_nodes::JobSpec) -> bool {
    !matches!(job, nodecraft_nodes::JobSpec::Generate { .. })
}

/// Input hashes, made once per plan.
#[derive(Default)]
struct Hashed(Option<Result<InputHasher, ()>>);

impl Hashed {
    fn now(&mut self, canvas: &Canvas, kinds: &Registry, node: NodeId) -> Option<Blake3> {
        let hasher = self.0.get_or_insert_with(|| InputHasher::new(canvas, kinds, MAX_QUERY_BYTES).map_err(|_| ())).as_mut().ok()?;
        hasher.hash(canvas, kinds, node, &BTreeMap::new()).ok().flatten()
    }
}

/// Completing `node`'s active result, when it is a partial one over `items` made from the
/// inputs the node has now.
fn partial(canvas: &Canvas, kinds: &Registry, hasher: &mut Hashed, node: NodeId, items: &[nodecraft_graph::ItemTag]) -> Option<Resume> {
    let node_ref = canvas.nodes().get(&node)?;
    let partial = node_ref.active.and_then(|active| node_ref.history.iter().find(|g| g.id == active))?;
    let incomplete = partial.incomplete.as_ref()?;
    if partial.items != items || partial.outputs.len() != node_ref.outputs.len() {
        return None;
    }
    let recorded = partial.input_hash?;
    (hasher.now(canvas, kinds, node)? == recorded).then(|| Resume { generation: Some(partial.id), missing: incomplete.missing.clone() })
}

/// Keeps only the jobs of the items `resume` runs; the others' counts become zero.
fn prune(jobs: &mut Vec<nodecraft_nodes::JobSpec>, counts: &mut [usize], resume: &Resume) {
    let mut kept = Vec::with_capacity(jobs.len());
    let mut planned = std::mem::take(jobs).into_iter();
    for (index, count) in counts.iter_mut().enumerate() {
        let item: Vec<_> = planned.by_ref().take(*count).collect();
        if resume.runs(index) {
            kept.extend(item);
        } else {
            *count = 0;
        }
    }
    *jobs = kept;
}
