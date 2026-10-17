//! Staleness (#75): a result records a hash of what it was made from, and a node whose inputs now
//! hash differently is stale. Stale nodes are not re-run (that costs money); they are marked, and
//! *Run with upstream* / *Run all stale* refresh them on request.
//!
//! The input hash covers the node's kind, its configuration (which shapes dynamic kinds' schema
//! and work), its visible parameters (hidden ones don't affect the result), and, per input port in
//! noodle order, what feeds it: an upstream source's content, or the identity of an upstream
//! node's active result. Positions, titles, tags and other nodes' unrelated edits don't change it.

use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::{Blake3, Error, ErrorKind, NodeId, ParamValues, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Version tag mixed into every hash, so a change to what is hashed makes old results "unknown"
/// rather than wrongly fresh. Bump it when the hashed fields change.
const DOMAIN: &[u8] = b"nodecraft.input-hash.v1";

/// Feeds bytes into the hasher with length prefixes, so field boundaries can't be confused.
struct Writer(blake3::Hasher);

impl Writer {
    fn bytes(&mut self, bytes: &[u8]) {
        self.0.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.0.update(bytes);
    }
    fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }
    /// A value through its canonical JSON (maps are ordered, numbers round-trip exactly).
    fn value(&mut self, value: &Value) -> Result<(), Error> {
        let json = serde_json::to_vec(value).map_err(|_| Error::new(ErrorKind::Internal, "a value couldn't be hashed"))?;
        self.bytes(&json);
        Ok(())
    }
    fn params(&mut self, params: &ParamValues) -> Result<(), Error> {
        self.bytes(&u64::try_from(params.len()).unwrap_or(u64::MAX).to_le_bytes());
        for (name, value) in params {
            self.text(name);
            self.value(value)?;
        }
        Ok(())
    }
}

/// The parameters that shape a node's result: its visible resolved values when its kind is
/// installed (hidden ones and defaults as the kind applies them), else what is stored. A node its
/// kind rejects has no meaningful hash (`Err`), so it is "unknown", never stale.
fn shaping_params(kinds: &Registry, node: &Node) -> Result<ParamValues, Error> {
    let Ok(kind) = kinds.get(&node.kind) else { return Ok(node.params.clone()) };
    let resolved = kind.validate_instance(node, false)?;
    Ok(resolved.visible_values().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// A node's own recipe: kind, configuration and shaping parameters.
fn write_node(w: &mut Writer, kinds: &Registry, node: &Node) -> Result<(), Error> {
    w.text(&node.kind);
    w.params(&node.config)?;
    w.params(&shaping_params(kinds, node)?)
}

/// Incoming feeds of every node, built once, with routers resolved to their sources (#72):
/// target → (port, position, upstream, upstream port).
type Incoming<'a> = BTreeMap<NodeId, Vec<crate::routing::Feed<'a>>>;

fn incoming(canvas: &Canvas) -> Result<Incoming<'_>, Error> {
    crate::routing::incoming(canvas)
}

fn is_source(kinds: &Registry, node: &Node) -> bool {
    kinds.get(&node.kind).is_ok_and(|kind| kind.descriptor().category == Category::Source)
}

/// The hash a result of `id` would record if it ran now.
pub fn input_hash(canvas: &Canvas, kinds: &Registry, id: NodeId) -> Result<Blake3, Error> {
    hash_with(canvas, kinds, &incoming(canvas)?, id)
}

fn hash_with(canvas: &Canvas, kinds: &Registry, incoming: &Incoming<'_>, id: NodeId) -> Result<Blake3, Error> {
    let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node is not on this canvas"))?;
    let mut w = Writer(blake3::Hasher::new());
    w.bytes(DOMAIN);
    write_node(&mut w, kinds, node)?;
    let mut feeds: BTreeMap<&str, Vec<(u32, NodeId, &str)>> = BTreeMap::new();
    for (port, order, from, from_port) in incoming.get(&id).into_iter().flatten() {
        feeds.entry(port).or_default().push((*order, *from, from_port));
    }
    for port in &node.inputs {
        w.text(&port.name);
        let mut list = feeds.remove(port.name.as_str()).unwrap_or_default();
        list.sort();
        w.bytes(&u64::try_from(list.len()).unwrap_or(u64::MAX).to_le_bytes());
        for (_, from, from_port) in list {
            w.text(from_port);
            match canvas.nodes().get(&from) {
                // A source contributes its content: its recipe (what it emits).
                Some(upstream) if is_source(kinds, upstream) => {
                    w.text("source");
                    write_node(&mut w, kinds, upstream)?;
                }
                // Anything else contributes the identity of the result it currently lends.
                Some(upstream) => match upstream.active {
                    Some(active) => {
                        w.text("result");
                        w.bytes(active.as_bytes());
                    }
                    None => w.text("empty"),
                },
                None => w.text("missing"),
            }
        }
    }
    Ok(Blake3::from_bytes(*w.0.finalize().as_bytes()))
}

/// Nodes whose active result is stale: its recorded input hash differs from the current one, or
/// a node upstream of it is stale (it was built on out-of-date data). Nodes without a result, or
/// whose result recorded no hash (made before hashing), are never stale themselves. The set is
/// transitive, as `planner::plan` expects.
pub fn stale(canvas: &Canvas, kinds: &Registry) -> BTreeSet<NodeId> {
    let has_result = |node: &Node| node.active.is_some_and(|active| node.history.iter().any(|g| g.id == active));
    // If routers can't be resolved, no hash can be checked: nothing is reported stale.
    let Ok(incoming) = incoming(canvas) else { return BTreeSet::new() };
    let mut stale: BTreeSet<NodeId> = canvas
        .nodes()
        .values()
        .filter(|node| {
            let recorded = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)).and_then(|g| g.input_hash);
            recorded.is_some_and(|hash| hash_with(canvas, kinds, &incoming, node.id).is_ok_and(|now| now != hash))
        })
        .map(|node| node.id)
        .collect();
    // Downstream of a stale node, every result is stale too (seen-set walk; graphs are acyclic
    // but the walk doesn't rely on it).
    let mut downstream: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        downstream.entry(edge.from.node).or_default().push(edge.to.node);
    }
    let mut seen: BTreeSet<NodeId> = stale.clone();
    let mut queue: Vec<NodeId> = stale.iter().copied().collect();
    while let Some(id) = queue.pop() {
        for next in downstream.get(&id).into_iter().flatten() {
            if seen.insert(*next) {
                if canvas.nodes().get(next).is_some_and(has_result) {
                    stale.insert(*next);
                }
                queue.push(*next);
            }
        }
    }
    stale
}

#[cfg(test)]
mod tests;
