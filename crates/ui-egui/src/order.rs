//! Input order badges (#64): the noodles into a list input carry numbered badges near their
//! input end, and dragging one onto another moves it there (`edge.reorder`, one undo step).
use nodecraft_graph::{Canvas, Endpoint};
use nodecraft_types::{EdgeId, NodeId};
use std::collections::BTreeMap;

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

/// `edges` with `moved` taken out and put at `target`, or `None` when nothing changes.
pub fn moved(edges: &[EdgeId], moved: EdgeId, target: usize) -> Option<Vec<EdgeId>> {
    let from = edges.iter().position(|id| *id == moved)?;
    let target = target.min(edges.len().saturating_sub(1));
    if from == target {
        return None;
    }
    let mut order: Vec<EdgeId> = edges.iter().copied().filter(|id| *id != moved).collect();
    order.insert(target, moved);
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
}
