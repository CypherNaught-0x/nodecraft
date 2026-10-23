//! Variations (#81): a generation node asked for several results makes each one in jobs of its
//! own, in one run. Each variation is a separate paid request with its own idempotency key and
//! drawn seeds, fails on its own, and is recorded as a generation of its own on the node; the
//! last one recorded becomes the active result.
//!
//! The count lives in the node's `config` under [`VARIATIONS_CONFIG`], not its parameters, so
//! it never reaches a provider and changing it never makes a result stale.

use nodecraft_graph::Node;
use nodecraft_nodes::JobSpec;
use nodecraft_types::Value;

/// The node `config` key holding how many variations a run makes: a whole number from 1 to
/// [`MAX_VARIATIONS`]. Absent means one.
pub const VARIATIONS_CONFIG: &str = "nodecraft.variations";

/// The most variations one run of a node makes.
pub const MAX_VARIATIONS: usize = 20;

/// How many variations `node` asks for: its configured count, clamped to 1..=[`MAX_VARIATIONS`].
/// A count that isn't a whole number (a hand-edited file) counts as one.
pub fn requested(node: &Node) -> usize {
    match node.config.get(VARIATIONS_CONFIG) {
        Some(Value::Unsigned(n)) => usize::try_from(n.0).unwrap_or(MAX_VARIATIONS).clamp(1, MAX_VARIATIONS),
        _ => 1,
    }
}

/// One variation's jobs, repeated for `count` variations, in variation order.
pub(crate) fn repeat(jobs: Vec<JobSpec>, count: usize) -> Vec<JobSpec> {
    if count <= 1 {
        return jobs;
    }
    let mut all = Vec::with_capacity(jobs.len().saturating_mul(count));
    for _ in 0..count {
        all.extend(jobs.iter().cloned());
    }
    all
}

/// For a step of `count` variations with `jobs` jobs in all: the variation each job is for.
/// `Err` when the jobs don't split evenly.
pub(crate) fn of_jobs(count: usize, jobs: usize) -> Result<Vec<usize>, String> {
    if count <= 1 || jobs == 0 {
        return Ok(Vec::new());
    }
    if !jobs.is_multiple_of(count) {
        return Err("its variations' jobs don't add up".into());
    }
    let each = jobs / count;
    Ok((0..jobs).map(|n| n / each).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{NodeId, Unsigned};

    fn node(value: Option<Value>) -> Node {
        let mut node = Node::new(NodeId::from_bytes([1; 16]), "generation.image");
        if let Some(value) = value {
            node.config.insert(VARIATIONS_CONFIG.into(), value);
        }
        node
    }

    #[test]
    fn the_count_is_clamped_and_hostile_values_count_as_one() {
        assert_eq!(requested(&node(None)), 1);
        assert_eq!(requested(&node(Some(Value::Unsigned(Unsigned(4))))), 4);
        assert_eq!(requested(&node(Some(Value::Unsigned(Unsigned(0))))), 1);
        assert_eq!(requested(&node(Some(Value::Unsigned(Unsigned(u64::MAX))))), MAX_VARIATIONS);
        assert_eq!(requested(&node(Some(Value::Text("9".into())))), 1);
    }

    #[test]
    fn jobs_split_evenly_into_variations() {
        assert_eq!(of_jobs(1, 3).unwrap(), Vec::<usize>::new());
        assert_eq!(of_jobs(3, 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(of_jobs(2, 4).unwrap(), vec![0, 0, 1, 1]);
        assert_eq!(of_jobs(3, 0).unwrap(), Vec::<usize>::new());
        assert!(of_jobs(3, 4).is_err());
    }
}
