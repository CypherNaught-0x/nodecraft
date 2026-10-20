//! Batches whose items partly failed (#480). A fanned step keeps the items that finished when
//! others fail: it records a partial result ([`nodecraft_graph::Incomplete`]) instead of
//! throwing paid results away. Running the node again while its inputs are unchanged completes
//! that result: the planner keeps the finished items and plans only the missing ones
//! ([`Resume`]), so the items already paid for aren't charged again. A missing item runs under
//! new idempotency keys: what a provider holds under its old ones is the failure or the cancel.

use crate::planner::Step;
use crate::staleness::{InputHasher, MAX_QUERY_BYTES};
use nodecraft_graph::{Canvas, MissingItem};
use nodecraft_nodes::Registry;
use nodecraft_types::GenerationId;
use std::collections::BTreeMap;

/// Completing a node's partial result: the finished items are copied from it, and only the
/// missing ones run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    /// The partial result whose finished items are kept.
    pub generation: GenerationId,
    /// Its missing items, which this run plans.
    pub missing: Vec<MissingItem>,
}

impl Resume {
    /// Whether item `index` is one this run plans.
    pub fn runs(&self, index: usize) -> bool {
        self.missing.iter().any(|m| usize::try_from(m.item).is_ok_and(|item| item == index))
    }
}

/// Plans only the missing items of fanned steps whose node shows a partial result made from
/// the inputs it has now. Only steps whose jobs are known up front qualify: a step fed by this
/// run can't know its inputs are unchanged until they are made. Anything that doesn't match
/// exactly (other items, another port layout, inputs that can't be hashed) runs in full.
pub(crate) fn resume_partials(canvas: &Canvas, kinds: &Registry, steps: &mut [Step]) {
    let mut hasher = None;
    let none = BTreeMap::new();
    for step in steps {
        let (Some(jobs), Some(fanout)) = (&mut step.jobs, &mut step.fanout) else { continue };
        let Some(node) = canvas.nodes().get(&step.node) else { continue };
        let Some(partial) = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)) else { continue };
        let Some(incomplete) = &partial.incomplete else { continue };
        if partial.items != fanout.items || partial.outputs.len() != node.outputs.len() || fanout.jobs.len() != fanout.items.len() {
            continue;
        }
        let Some(recorded) = partial.input_hash else { continue };
        let hasher = match &mut hasher {
            Some(hasher) => hasher,
            None => match InputHasher::new(canvas, kinds, MAX_QUERY_BYTES) {
                Ok(made) => hasher.insert(made),
                Err(_) => return,
            },
        };
        if !matches!(hasher.hash(canvas, kinds, step.node, &none), Ok(Some(now)) if now == recorded) {
            continue;
        }
        let resume = Resume { generation: partial.id, missing: incomplete.missing.clone() };
        let mut kept = Vec::with_capacity(jobs.len());
        let mut planned = std::mem::take(jobs).into_iter();
        for (index, count) in fanout.jobs.iter_mut().enumerate() {
            let item: Vec<_> = planned.by_ref().take(*count).collect();
            if resume.runs(index) {
                kept.extend(item);
            } else {
                *count = 0;
            }
        }
        *jobs = kept;
        fanout.resume = Some(resume);
    }
}
