//! Routers (#72) are resolved away: a consumer fed through a router (or a chain of routers) is
//! treated as fed by the routers' own sources, in order. The planner and staleness both read
//! inputs through [`incoming`], so a router adds no step, no cost and no hash of its own.

use nodecraft_graph::Canvas;
use nodecraft_nodes::{Registry, element, router};
use nodecraft_types::{Error, ErrorKind, NodeId};
use std::collections::{BTreeMap, BTreeSet};

/// Most feeds one input port resolves to. Just past the planner's list limit, so an oversized
/// fan-in is still reported (not silently cut), while chains of routers fanning into each other
/// can't multiply into millions of feeds.
pub const MAX_FEEDS_PER_PORT: usize = crate::planner::MAX_LIST_ITEMS + 1;

/// Most source entries resolution may build in total (routers' resolved lists and consumers'
/// feeds together). Real graphs need a handful per consumer; a hostile file whose routers fan
/// into each other is refused instead of using unbounded time or memory.
pub const MAX_RESOLUTION_STEPS: usize = 4_000_000;

/// One resolved feed into a port: (target port, position, upstream node, upstream port). The
/// position orders feeds within the port.
pub type Feed<'a> = (&'a str, u32, NodeId, &'a str);

/// Resolved inputs and validation facts, shared by planning and hashing.
#[derive(Debug)]
pub struct Incoming<'a> {
    pub feeds: BTreeMap<NodeId, Vec<Feed<'a>>>,
    /// Original wired ports, indexed once even when resolution produces no feeds.
    pub wired: BTreeSet<(NodeId, &'a str)>,
    /// One deterministic invalid router per affected consumer port. Valid feeds on the same
    /// port must not hide this failure. Reasons are fixed-size and stored once per router.
    pub invalid: BTreeMap<(NodeId, &'a str), NodeId>,
    pub invalid_routers: BTreeMap<NodeId, &'static str>,
    /// Consumers whose prompt mentions a name several Elements share (#104).
    pub ambiguous: BTreeSet<NodeId>,
}

/// Whether `id` is a router.
pub fn is_router(canvas: &Canvas, id: NodeId) -> bool {
    canvas.nodes().get(&id).is_some_and(|node| node.kind == router::ID)
}

fn too_large() -> Error {
    Error::new(ErrorKind::InvalidInput, "routers are chained too deeply or too widely to resolve")
}

/// Incoming feeds of every node that isn't a router, with routers upstream of it resolved to their
/// sources. Positions are renumbered 0.. per port in the resolved order. `Err` when resolving
/// would take more than [`MAX_RESOLUTION_STEPS`] entries.
///
/// Each router's sources are resolved once, routers before the routers they feed (Kahn's order),
/// so the cost is linear in the entries built, not in the number of paths through routers.
pub fn incoming<'a>(canvas: &'a Canvas, kinds: &Registry) -> Result<Incoming<'a>, Error> {
    let mut wired = BTreeSet::new();
    let mut raw: BTreeMap<NodeId, Vec<Feed<'_>>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        wired.insert((edge.to.node, edge.to.port.as_str()));
        raw.entry(edge.to.node).or_default().push((edge.to.port.as_str(), edge.order, edge.from.node, edge.from.port.as_str()));
    }
    for feeds in raw.values_mut() {
        feeds.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
    }
    let routers: BTreeSet<NodeId> = canvas.nodes().values().filter(|n| n.kind == router::ID).map(|n| n.id).collect();
    let invalid_routers: BTreeMap<NodeId, &'static str> = routers
        .iter()
        .filter_map(|id| {
            let node = canvas.nodes().get(id)?;
            let reason = match kinds.get(&node.kind) {
                Err(_) => "router kind is not installed",
                Ok(kind) if kind.validate_instance(node, true).is_err() => "router configuration or stored ports do not match its installed kind",
                Ok(_) => return None,
            };
            Some((*id, reason))
        })
        .collect();
    let mut failures: BTreeMap<NodeId, NodeId> = BTreeMap::new();
    // Kahn's order over router → router edges. Routers on a cycle (not valid graphs) never become
    // ready and resolve to nothing.
    let mut waiting: BTreeMap<NodeId, usize> = routers.iter().map(|r| (*r, 0)).collect();
    let mut feeds_into: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for router_id in &routers {
        for (_, _, from, _) in raw.get(router_id).into_iter().flatten() {
            if routers.contains(from) {
                feeds_into.entry(*from).or_default().push(*router_id);
                if let Some(count) = waiting.get_mut(router_id) {
                    *count = count.saturating_add(1);
                }
            }
        }
    }
    let mut ready: Vec<NodeId> = waiting.iter().filter(|(_, n)| **n == 0).map(|(r, _)| *r).collect();
    let mut memo: BTreeMap<NodeId, Vec<(NodeId, &str)>> = BTreeMap::new();
    let mut steps = 0usize;
    while let Some(router_id) = ready.pop() {
        // Inspect every upstream router even after the feed cap: invalid tails cannot vanish.
        let failure = invalid_routers
            .contains_key(&router_id)
            .then_some(router_id)
            .or_else(|| raw.get(&router_id).into_iter().flatten().find_map(|(_, _, from, _)| failures.get(from).copied()));
        if let Some(failure) = failure {
            failures.insert(router_id, failure);
        }
        let mut list: Vec<(NodeId, &str)> = Vec::new();
        for (_, _, from, from_port) in raw.get(&router_id).into_iter().flatten() {
            if list.len() >= MAX_FEEDS_PER_PORT {
                break;
            }
            let room = MAX_FEEDS_PER_PORT.saturating_sub(list.len());
            if routers.contains(from) {
                list.extend(memo.get(from).into_iter().flatten().take(room).copied());
            } else {
                list.push((*from, from_port));
            }
        }
        steps = steps.saturating_add(list.len().max(1));
        if steps > MAX_RESOLUTION_STEPS {
            return Err(too_large());
        }
        memo.insert(router_id, list);
        for next in feeds_into.get(&router_id).into_iter().flatten() {
            if let Some(count) = waiting.get_mut(next) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push(*next);
                }
            }
        }
    }
    let mut resolved = BTreeMap::new();
    let mut invalid = BTreeMap::new();
    for (target, feeds) in &raw {
        // Routers never run, so nothing reads their own inputs.
        if routers.contains(target) {
            continue;
        }
        let mut out: Vec<Feed<'_>> = Vec::new();
        let mut current_port = None;
        let (mut position, mut in_port) = (0u32, 0usize);
        for (port, _, from, from_port) in feeds {
            if let Some(failure) = failures.get(from) {
                steps = steps.saturating_add(1);
                if steps > MAX_RESOLUTION_STEPS {
                    return Err(too_large());
                }
                invalid.entry((*target, *port)).or_insert(*failure);
            }
            if current_port != Some(*port) {
                current_port = Some(*port);
                position = 0;
                in_port = 0;
            }
            let sources: Vec<(NodeId, &str)> = if routers.contains(from) {
                memo.get(from).into_iter().flatten().take(MAX_FEEDS_PER_PORT.saturating_sub(in_port)).copied().collect()
            } else if in_port < MAX_FEEDS_PER_PORT {
                vec![(*from, *from_port)]
            } else {
                Vec::new()
            };
            steps = steps.saturating_add(sources.len().max(1));
            if steps > MAX_RESOLUTION_STEPS {
                return Err(too_large());
            }
            for (source, source_port) in sources {
                out.push((port, position, source, source_port));
                position = position.saturating_add(1);
                in_port = in_port.saturating_add(1);
            }
        }
        resolved.insert(*target, out);
    }
    // An Element's sheet card that hasn't run yet feeds nothing, so the Element uses its images
    // and planning it doesn't pull in the paid sheet run (#104).
    for (target, feeds) in resolved.iter_mut() {
        if canvas.nodes().get(target).is_some_and(|node| node.kind == element::ID) {
            feeds.retain(|feed| feed.0 != element::SHEET || canvas.nodes().get(&feed.2).is_some_and(|from| from.active.is_some()));
            if !feeds.iter().any(|feed| feed.0 == element::SHEET) {
                wired.remove(&(*target, element::SHEET));
            }
        }
    }
    // Elements a prompt mentions feed in after the wires (#104), unless a wire (perhaps through
    // routers) already brings that Element's references.
    let mentioned = crate::mentions::feeds(canvas, kinds);
    for (target, feeds) in mentioned.feeds {
        let out = resolved.entry(target).or_default();
        for (port, from, from_port) in feeds {
            if out.iter().any(|feed| feed.2 == from && feed.3 == from_port) {
                continue;
            }
            let in_port = out.iter().filter(|feed| feed.0 == port).count();
            if in_port >= MAX_FEEDS_PER_PORT {
                continue;
            }
            steps = steps.saturating_add(1);
            if steps > MAX_RESOLUTION_STEPS {
                return Err(too_large());
            }
            out.push((port, u32::try_from(in_port).unwrap_or(u32::MAX), from, from_port));
        }
    }
    Ok(Incoming { feeds: resolved, wired, invalid, invalid_routers, ambiguous: mentioned.ambiguous })
}

#[cfg(test)]
mod tests;
