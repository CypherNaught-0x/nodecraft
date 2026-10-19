//! Input order badges (#64): the noodles into a list input carry numbered badges near their
//! input end, and dragging one onto another moves it there (`edge.reorder`, one undo step).
use nodecraft_graph::{Canvas, Endpoint};
use nodecraft_types::{EdgeId, NodeId};
use std::collections::{BTreeMap, BTreeSet};

/// How far along a noodle its badge sits: near the input end, where noodles into one input are
/// still apart (they meet at the port itself).
pub const BADGE_AT: f32 = 0.7;

/// A list input, as (node, port).
pub type Input = (NodeId, String);

/// The input an endpoint names.
pub fn input(to: &Endpoint) -> Input {
    (to.node, to.port.clone())
}

/// List inputs fed by more than one noodle, each with its edges first to last.
pub fn ordered_inputs(canvas: &Canvas) -> BTreeMap<Input, Vec<EdgeId>> {
    let mut groups: BTreeMap<Input, Vec<(u32, EdgeId)>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        let many = canvas
            .nodes()
            .get(&edge.to.node)
            .and_then(|node| node.inputs.iter().find(|port| port.name == edge.to.port))
            .is_some_and(|port| port.port_type.many);
        if many {
            groups.entry(input(&edge.to)).or_default().push((edge.order, edge.id));
        }
    }
    groups
        .into_iter()
        .filter(|(_, edges)| edges.len() > 1)
        .map(|(to, mut edges)| {
            edges.sort();
            (to, edges.into_iter().map(|(_, id)| id).collect())
        })
        .collect()
}

/// `edges` with `moved` taken out and put at the clamped `target`.
/// Returns `None` for duplicate IDs, a missing ID, or an unchanged order.
pub fn moved(edges: &[EdgeId], moved: EdgeId, target: usize) -> Option<Vec<EdgeId>> {
    let mut seen = BTreeSet::new();
    if edges.iter().any(|id| !seen.insert(*id)) {
        return None;
    }
    let from = edges.iter().position(|id| *id == moved)?;
    let target = target.min(edges.len().saturating_sub(1));
    if from == target {
        return None;
    }
    let mut order: Vec<EdgeId> = edges.iter().copied().filter(|id| *id != moved).collect();
    // Unique IDs make this clamp redundant; keep it as defense in depth at the insertion boundary.
    order.insert(target.min(order.len()), moved);
    Some(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(n: u8) -> EdgeId {
        EdgeId::from_bytes([n; 16])
    }

    #[test]
    fn moving_a_badge_reorders_and_a_no_op_is_none() {
        let edges = [e(1), e(2), e(3)];
        assert_eq!(moved(&edges, e(3), 0), Some(vec![e(3), e(1), e(2)]));
        assert_eq!(moved(&edges, e(1), 2), Some(vec![e(2), e(3), e(1)]));
        assert_eq!(moved(&edges, e(1), 99), Some(vec![e(2), e(3), e(1)]), "past the end is the end");
        assert_eq!(moved(&edges, e(2), 1), None);
        assert_eq!(moved(&edges, e(9), 0), None, "not in this input");
    }

    #[test]
    fn issue333_duplicate_moved_ids_are_refused_without_panicking() {
        for target in [2, usize::MAX] {
            assert_eq!(moved(&[e(1), e(1), e(2)], e(1), target), None);
        }
    }
    #[test]
    fn issue333_duplicate_other_ids_are_refused() {
        assert_eq!(moved(&[e(1), e(2), e(2)], e(1), 1), None);
    }
    #[test]
    fn issue333_invalid_and_unique_moves_preserve_the_permutation_contract() {
        assert_eq!(moved(&[], e(1), usize::MAX), None);
        assert_eq!(moved(&[e(1)], e(1), usize::MAX), None);
        assert_eq!(moved(&[e(1), e(2)], e(9), usize::MAX), None);
        let edges = [e(1), e(2), e(3), e(4)];
        for id in edges {
            for target in [0, 1, 2, 3, 4, usize::MAX] {
                let actual = moved(&edges, id, target).unwrap_or_else(|| edges.to_vec());
                assert_eq!(actual.len(), edges.len());
                assert_eq!(actual.iter().copied().collect::<std::collections::BTreeSet<_>>(), edges.into_iter().collect());
                assert_eq!(actual.iter().copied().filter(|e| *e != id).collect::<Vec<_>>(), edges.into_iter().filter(|e| *e != id).collect::<Vec<_>>());
                assert_eq!(actual.get(target.min(edges.len() - 1)), Some(&id));
            }
        }
    }
}
