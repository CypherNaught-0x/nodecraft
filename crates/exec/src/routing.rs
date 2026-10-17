//! Routers (#72) are resolved away: a consumer fed through a router (or a chain of routers) is
//! treated as fed by the routers' own sources, in order. The planner and staleness both read
//! inputs through [`incoming`], so a router adds no step, no cost and no hash of its own.

use nodecraft_graph::Canvas;
use nodecraft_nodes::router;
use nodecraft_types::NodeId;
use std::collections::{BTreeMap, BTreeSet};

/// Most feeds one input port resolves to. Just past the planner's list limit, so an oversized
/// fan-in is still reported (not silently cut), while chains of routers fanning into each other
/// can't multiply into millions of feeds.
pub const MAX_FEEDS_PER_PORT: usize = crate::planner::MAX_LIST_ITEMS + 1;

/// One resolved feed into a port: (target port, position, upstream node, upstream port). The
/// position orders feeds within the port.
pub type Feed<'a> = (&'a str, u32, NodeId, &'a str);

/// Whether `id` is a router.
pub fn is_router(canvas: &Canvas, id: NodeId) -> bool {
    canvas.nodes().get(&id).is_some_and(|node| node.kind == router::ID)
}

/// Incoming feeds of every node, with routers upstream of it resolved to their sources (a router's
/// own entry lists the sources behind it). Positions are renumbered 0.. per port in the resolved
/// order.
pub fn incoming(canvas: &Canvas) -> BTreeMap<NodeId, Vec<Feed<'_>>> {
    let mut raw: BTreeMap<NodeId, Vec<Feed<'_>>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        raw.entry(edge.to.node).or_default().push((edge.to.port.as_str(), edge.order, edge.from.node, edge.from.port.as_str()));
    }
    for feeds in raw.values_mut() {
        feeds.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
    }
    let mut resolved = BTreeMap::new();
    for (target, feeds) in &raw {
        let mut out: Vec<Feed<'_>> = Vec::new();
        let mut current_port = None;
        let mut position = 0u32;
        for (port, _, from, from_port) in feeds {
            if current_port != Some(*port) {
                current_port = Some(*port);
                position = 0;
            }
            let mut sources = Vec::new();
            expand(canvas, &raw, *from, from_port, &mut BTreeSet::new(), &mut sources);
            let already = out.iter().filter(|(p, ..)| p == port).count();
            for (source, source_port) in sources.into_iter().take(MAX_FEEDS_PER_PORT.saturating_sub(already)) {
                out.push((port, position, source, source_port));
                position = position.saturating_add(1);
            }
        }
        resolved.insert(*target, out);
    }
    resolved
}

/// The sources behind `(from, from_port)`: itself unless it is a router, else the router's own
/// inputs in order, recursively. `visiting` breaks cycles; output is capped per port.
fn expand<'a>(
    canvas: &Canvas,
    raw: &BTreeMap<NodeId, Vec<Feed<'a>>>,
    from: NodeId,
    from_port: &'a str,
    visiting: &mut BTreeSet<NodeId>,
    out: &mut Vec<(NodeId, &'a str)>,
) {
    if out.len() >= MAX_FEEDS_PER_PORT {
        return;
    }
    if !is_router(canvas, from) {
        out.push((from, from_port));
        return;
    }
    if !visiting.insert(from) {
        return;
    }
    for (_, _, inner, inner_port) in raw.get(&from).into_iter().flatten() {
        expand(canvas, raw, *inner, inner_port, visiting, out);
        if out.len() >= MAX_FEEDS_PER_PORT {
            break;
        }
    }
    visiting.remove(&from);
}

#[cfg(test)]
mod tests;
