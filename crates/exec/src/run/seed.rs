//! Random seeds (#162): a seed parameter whose effective setting is `random_seed` is drawn when
//! its step's jobs are handed out, after catalog admission has matched the job to the source's
//! submission. The draw is a pure function of the run, the node, the job's place in its step and
//! the parameter, so a retried job sends the same seed whatever order steps ran in, and the
//! result records exactly what was sent.
use nodecraft_nodes::JobSpec;
use nodecraft_types::{Error, NodeId, ParamValues, Unsigned, Value};

use crate::admission::{Usage, Witness, measure};

/// Adds the drawn seeds to `job`, job `index` of `node`'s step, charging them to the step.
pub(super) fn draw(witness: &Witness, usage: &mut Usage, run: &[u8; 16], node: NodeId, index: usize, job: &mut JobSpec) -> Result<(), Error> {
    let seeds = witness.seeds(node, job)?;
    if seeds.is_empty() {
        return Ok(());
    }
    let index = u64::try_from(index).map_err(|_| Error::new(nodecraft_types::ErrorKind::Overflow, "a step has too many jobs to seed"))?;
    let drawn: ParamValues = seeds.iter().map(|(name, max)| (name.clone(), Value::Unsigned(seed(run, node, index, name, *max)))).collect();
    let JobSpec::Generate { params, .. } = job else { return Ok(()) };
    if drawn.keys().any(|name| params.contains_key(name)) {
        return Err(Error::new(nodecraft_types::ErrorKind::InvalidInput, "a drawn seed is already set"));
    }
    usage.reserve(node, measure::params(&drawn)?)?;
    params.extend(drawn);
    Ok(())
}

fn seed(run: &[u8; 16], node: NodeId, index: u64, name: &str, max: Option<Unsigned>) -> Unsigned {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nodecraft.run.seed");
    hasher.update(run);
    hasher.update(node.as_bytes());
    hasher.update(&index.to_le_bytes());
    hasher.update(name.as_bytes());
    let mut bytes = [0u8; 8];
    if let Some(head) = hasher.finalize().as_bytes().get(..8) {
        bytes.copy_from_slice(head);
    }
    let raw = u64::from_le_bytes(bytes);
    Unsigned(match max {
        Some(Unsigned(max)) => max.checked_add(1).map_or(raw, |span| raw % span),
        None => raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_is_fixed_by_its_run_node_job_and_name_and_stays_within_its_bound() {
        let node = NodeId::from_bytes([1; 16]);
        let a = seed(&[1; 16], node, 1, "seed", None);
        assert_eq!(a, seed(&[1; 16], node, 1, "seed", None), "a retry draws the same seed");
        for other in [
            seed(&[2; 16], node, 1, "seed", None),
            seed(&[1; 16], NodeId::from_bytes([2; 16]), 1, "seed", None),
            seed(&[1; 16], node, 2, "seed", None),
            seed(&[1; 16], node, 1, "noise", None),
        ] {
            assert_ne!(a, other);
        }
        for id in 0..256 {
            assert!(seed(&[1; 16], node, id, "seed", Some(Unsigned(9))).0 <= 9);
        }
        assert_eq!(seed(&[1; 16], node, 1, "seed", Some(Unsigned(0))), Unsigned(0));
        assert_eq!(seed(&[1; 16], node, 1, "seed", Some(Unsigned(u64::MAX))), a, "the full range needs no reduction");
    }
}
