//! Inlining technique nodes (#94) before planning. Each technique node on a canvas is replaced,
//! for planning only, by a copy of the version it pins: the version's nodes under ids derived
//! from the instance, its inputs wired to whatever feeds the instance, and its exposed
//! parameters set to the instance's values. The instance node itself stays as it is, so its
//! staleness is that of the real canvas, and its own step gives the copied outputs back as its
//! results ([`nodecraft_nodes::technique`]).
//!
//! The working canvas is never stored: the run records only the instance's results, and the
//! copied nodes' own results are dropped ([`crate::run`]). A version that can't be inlined (it is
//! missing, removed or changed under the instance) blocks only runs of the instance.

use crate::planner::{Blocker, Reason};
use nodecraft_graph::{Canvas, Edge, Endpoint, Node};
use nodecraft_nodes::{Registry, technique};
use nodecraft_types::{CanvasId, EdgeId, Error, NodeId};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// How deeply techniques may nest inside one another.
pub const MAX_DEPTH: usize = 8;
/// Most nodes inlining may add to one canvas, across all its techniques.
pub const MAX_INLINED_NODES: usize = 2000;
/// Most wires inlining may add to one canvas. A fed input multiplies its wires by its feeds, so
/// nested techniques could otherwise grow them exponentially.
pub const MAX_INLINED_EDGES: usize = 10_000;

/// Where one output of a technique node comes from: `port` of the inlined `node`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collect {
    pub output: String,
    pub node: NodeId,
    pub port: String,
}

/// A canvas with its technique nodes inlined.
#[derive(Clone, Debug)]
pub struct Expanded<'a> {
    pub canvas: Cow<'a, Canvas>,
    /// For each inlined technique node, where each of its outputs comes from, in output order.
    pub collects: BTreeMap<NodeId, Vec<Collect>>,
    /// Each inlined node, with the technique node on the real canvas it runs for.
    pub inner: BTreeMap<NodeId, NodeId>,
    /// Technique nodes (on the real canvas) whose version can't be inlined, and why.
    pub blockers: Vec<Blocker>,
}

impl<'a> Expanded<'a> {
    /// A canvas with no technique nodes to inline.
    pub fn plain(canvas: &'a Canvas) -> Self {
        Self { canvas: Cow::Borrowed(canvas), collects: BTreeMap::new(), inner: BTreeMap::new(), blockers: Vec::new() }
    }

    /// The technique node on the real canvas that `id` runs for: itself when it is on it.
    pub fn owner(&self, id: NodeId) -> NodeId {
        self.inner.get(&id).copied().unwrap_or(id)
    }
}

/// The id of `inner` (a node or edge of a version) inlined for the technique node `instance`.
fn derive(instance: NodeId, inner: &[u8; 16], salt: u32) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new_derive_key("nodecraft technique inline 2026-10-20");
    hasher.update(instance.as_bytes());
    hasher.update(inner);
    hasher.update(&salt.to_le_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(hasher.finalize().as_bytes().get(..16).unwrap_or(&[0; 16]));
    id
}

/// One technique node waiting to be inlined.
struct Pending {
    id: NodeId,
    owner: NodeId,
    depth: usize,
}

/// Inline every technique node on `canvas`, with the project's `canvases` holding the versions.
/// Errors only when the inlined canvas itself is malformed; a version that can't be inlined is a
/// blocker on its instance.
pub fn expand<'a>(canvases: &BTreeMap<CanvasId, Canvas>, canvas: &'a Canvas, kinds: &Registry) -> Result<Expanded<'a>, Error> {
    let mut queue: VecDeque<Pending> =
        canvas.nodes().values().filter(|node| node.kind == technique::ID).map(|node| Pending { id: node.id, owner: node.id, depth: 0 }).collect();
    if queue.is_empty() {
        return Ok(Expanded::plain(canvas));
    }
    let mut added: BTreeMap<NodeId, Node> = BTreeMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut collects = BTreeMap::new();
    let mut inner = BTreeMap::new();
    let mut blockers: Vec<Blocker> = Vec::new();
    let mut blocked = BTreeSet::new();
    let mut edge_ids: BTreeSet<[u8; 16]> = BTreeSet::new();
    // Wires into each node of the real canvas, indexed once.
    let mut incoming: BTreeMap<NodeId, Vec<&Edge>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        incoming.entry(edge.to.node).or_default().push(edge);
    }
    let mut block = |blocked: &mut BTreeSet<NodeId>, owner: NodeId, message: &str| {
        if blocked.insert(owner) {
            blockers.push(Blocker { node: owner, port: None, upstream: None, reason: Reason::PublicInvalid(message.to_owned()) });
        }
    };
    while let Some(Pending { id, owner, depth }) = queue.pop_front() {
        let Some(node) = canvas.nodes().get(&id).or_else(|| added.get(&id)) else { continue };
        if blocked.contains(&owner) {
            continue;
        }
        let interface = match technique::interface(node) {
            Ok(interface) => interface,
            Err(error) => {
                block(&mut blocked, owner, &error.message);
                continue;
            }
        };
        let Some(version) = canvases.get(&interface.canvas) else {
            block(&mut blocked, owner, "its technique version isn't in this project");
            continue;
        };
        let Some(info) = version.technique_info().filter(|info| info.id == interface.technique && info.version == interface.version) else {
            block(&mut blocked, owner, "its technique version isn't in this project");
            continue;
        };
        if info.retired {
            block(&mut blocked, owner, "its technique version was removed; upgrade it to another version");
            continue;
        }
        if !technique::interface_of(version, kinds).is_ok_and(|now| now.matches(&interface)) {
            block(&mut blocked, owner, "its technique version no longer matches it; upgrade it");
            continue;
        }
        if depth >= MAX_DEPTH {
            block(&mut blocked, owner, "its techniques nest too deeply");
            continue;
        }
        let nodes_after = added.len().saturating_add(version.nodes().len());
        if nodes_after > MAX_INLINED_NODES || nodes_after.saturating_add(canvas.nodes().len()) > nodecraft_graph::MAX_NODES {
            block(&mut blocked, owner, "its techniques hold too many nodes to run");
            continue;
        }
        let map = |inner: NodeId| NodeId::from_bytes(derive(id, inner.as_bytes(), 0));
        let mut copies = Vec::with_capacity(version.nodes().len());
        let mut taken = false;
        for original in version.nodes().values() {
            let mut copy = original.clone();
            copy.id = map(original.id);
            copy.tag = None;
            copy.field = None;
            // An input node has no inputs: its result is the version's example, used when
            // nothing feeds the instance. Everything else runs again.
            if !info.inputs.iter().any(|input| input.node == original.id) {
                copy.history.clear();
                copy.active = None;
            }
            taken |= canvas.nodes().contains_key(&copy.id) || added.contains_key(&copy.id);
            copies.push(copy);
        }
        if taken {
            block(&mut blocked, owner, "an id its technique needs is already taken on this canvas");
            continue;
        }
        for (exposed, spec) in info.params.iter().zip(&interface.params) {
            if let (Some(value), Some(copy)) = (node.params.get(&spec.name), copies.iter_mut().find(|copy| copy.id == map(exposed.node))) {
                copy.params.insert(exposed.param.clone(), value.clone());
            }
        }
        // What feeds each exposed input: wires into the instance's port, in input order.
        // Only a nested copy is fed by inlined wires.
        let nested = if inner.contains_key(&id) { edges.as_slice() } else { &[] };
        let into_instance = incoming.get(&id).into_iter().flatten().copied().chain(nested.iter().filter(|edge| edge.to.node == id));
        let mut feeds: BTreeMap<(NodeId, &str), Vec<(u32, Endpoint)>> = BTreeMap::new();
        for edge in into_instance {
            if let Some(index) = interface.inputs.iter().position(|input| input.port == edge.to.port)
                && let Some(exposed) = info.inputs.get(index)
            {
                feeds.entry((exposed.node, exposed.port.as_str())).or_default().push((edge.order, edge.from.clone()));
            }
        }
        for list in feeds.values_mut() {
            list.sort_by_key(|(order, _)| *order);
        }
        // Counted before any is made: a fed input's wires multiply by its feeds.
        let adding =
            version.edges().values().map(|edge| feeds.get(&(edge.from.node, edge.from.port.as_str())).map_or(1, Vec::len)).fold(0usize, usize::saturating_add);
        let edges_after = edges.len().saturating_add(adding);
        if edges_after > MAX_INLINED_EDGES || edges_after.saturating_add(canvas.edges().len()) > nodecraft_graph::MAX_EDGES {
            block(&mut blocked, owner, "its techniques hold too many wires to run");
            continue;
        }
        // The version's wires, those leaving a fed input replaced by one wire per feed; then
        // each input's wires numbered again in order.
        let mut wires: Vec<(Endpoint, u32, usize, Endpoint, [u8; 16])> = Vec::new();
        for edge in version.edges().values() {
            let to = Endpoint { node: map(edge.to.node), port: edge.to.port.clone() };
            match feeds.get(&(edge.from.node, edge.from.port.as_str())) {
                Some(list) => {
                    for (n, (_, from)) in list.iter().enumerate() {
                        let salt = u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1);
                        wires.push((to.clone(), edge.order, n, from.clone(), derive(id, edge.id.as_bytes(), salt)));
                    }
                }
                None => {
                    wires.push((to, edge.order, 0, Endpoint { node: map(edge.from.node), port: edge.from.port.clone() }, derive(id, edge.id.as_bytes(), 0)))
                }
            }
        }
        wires.sort_by(|a, b| (&a.0.node, &a.0.port, a.1, a.2).cmp(&(&b.0.node, &b.0.port, b.1, b.2)));
        let fresh: BTreeSet<[u8; 16]> = wires.iter().map(|wire| wire.4).collect();
        if fresh.len() != wires.len() || fresh.iter().any(|id| canvas.edges().contains_key(&EdgeId::from_bytes(*id)) || edge_ids.contains(id)) {
            block(&mut blocked, owner, "an id its technique needs is already taken on this canvas");
            continue;
        }
        edge_ids.extend(fresh);
        let mut previous: Option<Endpoint> = None;
        let mut order = 0u32;
        for (to, _, _, from, edge_id) in wires {
            order = if previous.as_ref() == Some(&to) { order.saturating_add(1) } else { 0 };
            previous = Some(to.clone());
            edges.push(Edge { id: EdgeId::from_bytes(edge_id), from, to, order });
        }
        let outputs = info
            .outputs
            .iter()
            .zip(&interface.outputs)
            .map(|(exposed, output)| Collect { output: output.port.clone(), node: map(exposed.node), port: exposed.port.clone() })
            .collect();
        collects.insert(id, outputs);
        for copy in copies {
            inner.insert(copy.id, owner);
            if copy.kind == technique::ID {
                queue.push_back(Pending { id: copy.id, owner, depth: depth.saturating_add(1) });
            }
            added.insert(copy.id, copy);
        }
    }
    // A blocked owner's nested copies are dropped as a whole, so nothing half-inlined runs.
    let dropped: BTreeSet<NodeId> = inner.iter().filter(|(_, owner)| blocked.contains(*owner)).map(|(id, _)| *id).collect();
    let nodes: Vec<Node> = added.into_values().filter(|node| !dropped.contains(&node.id)).collect();
    let edges: Vec<Edge> = edges.into_iter().filter(|edge| !dropped.contains(&edge.to.node) && !dropped.contains(&edge.from.node)).collect();
    collects.retain(|id, _| !dropped.contains(id) && !blocked.contains(id));
    inner.retain(|id, _| !dropped.contains(id));
    let canvas = canvas.inlined(nodes, edges)?;
    Ok(Expanded { canvas: Cow::Owned(canvas), collects, inner, blockers })
}

#[cfg(test)]
mod tests;
