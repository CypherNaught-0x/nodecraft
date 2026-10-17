//! Staleness (#75): a result records a hash of what it was made from, and a node whose inputs now
//! hash differently is stale. Stale nodes are not re-run (that costs money); they are marked, and
//! *Run with upstream* / *Run all stale* refresh them on request.
//!
//! The input hash covers the node's kind, its configuration (which shapes dynamic kinds' schema
//! and work), its visible parameters (hidden ones don't affect the result), and, per input port in
//! noodle order, what feeds it: an upstream source's recipe (what it emits), or the content of the
//! upstream result's output on that port. Content, not result ids: a result can be replaced under
//! the same id (`SetNode`, or removed and re-added), and an identical re-run upstream keeps
//! downstream results fresh. Positions, titles, tags and unrelated edits don't change the hash.
//!
//! **Bounded work:** one query digests each node's recipe and each upstream output once
//! (memoized), streams values into the hasher without serializing them, and charges every byte
//! against a shared budget ([`MAX_QUERY_BYTES`]); a canvas past it is refused, not hashed for
//! minutes.
//!
//! **Recipe versions:** the hash starts with a domain tag ([`DOMAIN`]). The stored hash carries no
//! version, so after a change to the recipe an old result can't be told from one whose inputs
//! changed: it is reported **stale** (re-running refreshes it). Deliberately conservative: a
//! result is never shown as fresh when we can't vouch for it.

use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::{Blake3, Error, ErrorKind, NodeId, ParamValues, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Domain tag mixed into every hash. Bump it when what is hashed changes; results stamped under
/// an older tag are then reported stale (see the module docs).
pub const DOMAIN: &[u8] = b"nodecraft.input-hash.v2";

/// Most bytes one staleness query may hash (recipes, parameters and upstream outputs, each once).
pub const MAX_QUERY_BYTES: usize = 256 * 1024 * 1024;

/// Fixed cost charged per value on top of its payload, so huge lists of tiny values count too.
const VALUE_COST: usize = 16;

fn too_large() -> Error {
    Error::new(ErrorKind::InvalidInput, "the canvas holds too much data to check for stale results; split it into smaller canvases")
}

/// Bytes left in a query's budget, and bytes hashed so far.
struct Meter {
    left: usize,
    hashed: usize,
}

impl Meter {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.left = self.left.checked_sub(bytes).ok_or_else(too_large)?;
        self.hashed = self.hashed.saturating_add(bytes);
        Ok(())
    }
}

/// Feeds bytes into the hasher with length prefixes, so field boundaries can't be confused.
struct Writer(blake3::Hasher);

impl Writer {
    fn new() -> Self {
        let mut w = Self(blake3::Hasher::new());
        w.bytes(DOMAIN);
        w
    }
    fn len(&mut self, len: usize) {
        self.0.update(&u64::try_from(len).unwrap_or(u64::MAX).to_le_bytes());
    }
    fn bytes(&mut self, bytes: &[u8]) {
        self.len(bytes.len());
        self.0.update(bytes);
    }
    fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }
    fn digest(&mut self, digest: &[u8; 32]) {
        self.0.update(digest);
    }
    /// A value, streamed (iteratively: lists nest): a tag, then text as length-prefixed bytes,
    /// lists as their length then items, and scalars and media through their short canonical
    /// JSON. Every byte is charged to `meter` before it is hashed.
    fn value(&mut self, value: &Value, meter: &mut Meter) -> Result<(), Error> {
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            meter.charge(VALUE_COST)?;
            match value {
                Value::Text(text) => {
                    meter.charge(text.len())?;
                    self.text("t");
                    self.text(text);
                }
                Value::List(items) => {
                    self.text("l");
                    self.len(items.len());
                    stack.extend(items.iter().rev());
                }
                other => {
                    let json = serde_json::to_vec(other).map_err(|_| Error::new(ErrorKind::Internal, "a value couldn't be hashed"))?;
                    meter.charge(json.len())?;
                    self.text("v");
                    self.bytes(&json);
                }
            }
        }
        Ok(())
    }
    fn param(&mut self, name: &str, value: &Value, meter: &mut Meter) -> Result<(), Error> {
        meter.charge(name.len())?;
        self.text(name);
        self.value(value, meter)
    }
    fn params(&mut self, params: &ParamValues, meter: &mut Meter) -> Result<(), Error> {
        self.len(params.len());
        for (name, value) in params {
            self.param(name, value, meter)?;
        }
        Ok(())
    }
    fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

/// Approximate bytes of a node's stored parameters and configuration: charged before its kind
/// resolves (and copies) them.
fn stored_bytes(node: &Node) -> usize {
    node.params.iter().chain(node.config.iter()).fold(0usize, |sum, (name, value)| {
        let mut total = sum.saturating_add(name.len());
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            total = total.saturating_add(VALUE_COST).saturating_add(match value {
                Value::Text(text) => text.len(),
                Value::List(items) => {
                    stack.extend(items.iter());
                    0
                }
                _ => 0,
            });
        }
        total
    })
}

/// A node's recipe digest, or why its kind rejects it (then it has no meaningful hash: unknown).
type Recipe = Result<[u8; 32], Error>;

/// One query's shared state: the resolved inputs, memoized digests and the byte budget.
struct Query<'a> {
    canvas: &'a Canvas,
    kinds: &'a Registry,
    incoming: crate::routing::Incoming<'a>,
    recipes: BTreeMap<NodeId, Recipe>,
    outputs: BTreeMap<(NodeId, &'a str), [u8; 32]>,
    meter: Meter,
}

impl<'a> Query<'a> {
    fn new(canvas: &'a Canvas, kinds: &'a Registry, budget: usize) -> Result<Self, Error> {
        Ok(Self {
            canvas,
            kinds,
            incoming: crate::routing::incoming(canvas, kinds)?,
            recipes: BTreeMap::new(),
            outputs: BTreeMap::new(),
            meter: Meter { left: budget, hashed: 0 },
        })
    }

    /// Kind, configuration and shaping parameters: the visible resolved values when the kind is
    /// installed (hidden ones and defaults as the kind applies them), else what is stored.
    /// Outer `Err`: the query is over budget.
    fn recipe(&mut self, node: &Node) -> Result<Recipe, Error> {
        if let Some(recipe) = self.recipes.get(&node.id) {
            return Ok(recipe.clone());
        }
        self.meter.charge(stored_bytes(node))?;
        let mut w = Writer::new();
        w.text(&node.kind);
        w.params(&node.config, &mut self.meter)?;
        let recipe = match self.kinds.get(&node.kind) {
            Err(_) => {
                w.params(&node.params, &mut self.meter)?;
                Ok(w.finish())
            }
            Ok(kind) => match kind.validate_instance(node, false) {
                Err(rejected) => Err(rejected),
                Ok(resolved) => {
                    let visible: Vec<_> = resolved.visible_values().collect();
                    w.len(visible.len());
                    for (name, value) in visible {
                        w.param(name, value, &mut self.meter)?;
                    }
                    Ok(w.finish())
                }
            },
        };
        self.recipes.insert(node.id, recipe.clone());
        Ok(recipe)
    }

    /// The content of `upstream`'s active result on `port`, or `None` when it has none it can
    /// lend (no result, or one recorded for another port layout).
    fn output(&mut self, upstream: &'a Node, port: &'a str) -> Result<Option<[u8; 32]>, Error> {
        if let Some(digest) = self.outputs.get(&(upstream.id, port)) {
            return Ok(Some(*digest));
        }
        let Some(active) = upstream.active.and_then(|active| upstream.history.iter().find(|g| g.id == active)) else { return Ok(None) };
        // Results are positional; one recorded for a different port layout can't be used.
        if active.outputs.len() != upstream.outputs.len() {
            return Ok(None);
        }
        let Some(value) = upstream.outputs.iter().position(|p| p.name == port).and_then(|i| active.outputs.get(i)) else { return Ok(None) };
        let mut w = Writer::new();
        w.value(value, &mut self.meter)?;
        let digest = w.finish();
        self.outputs.insert((upstream.id, port), digest);
        Ok(Some(digest))
    }

    /// The hash a result of `id` would record if it ran now. Outer `Err`: the node isn't on the
    /// canvas, or the query is over budget. Inner `Err`: it (or a source feeding it) is rejected
    /// by its kind, so it has no hash.
    fn hash(&mut self, id: NodeId) -> Result<Result<Blake3, Error>, Error> {
        let canvas = self.canvas;
        // Inputs through an invalid router have no meaningful hash: unknown.
        if let Some((_, router)) = self.incoming.invalid.range((id, "")..).next().filter(|((target, _), _)| *target == id) {
            return Ok(Err(Error::new(ErrorKind::InvalidInput, format!("inputs depend on invalid router {router}"))));
        }
        let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node is not on this canvas"))?;
        let mut w = Writer::new();
        match self.recipe(node)? {
            Ok(recipe) => w.digest(&recipe),
            Err(rejected) => return Ok(Err(rejected)),
        }
        let mut feeds: BTreeMap<&str, Vec<(u32, NodeId, &str)>> = BTreeMap::new();
        for (port, order, from, from_port) in self.incoming.feeds.get(&id).into_iter().flatten() {
            feeds.entry(*port).or_default().push((*order, *from, *from_port));
        }
        for port in &node.inputs {
            w.text(&port.name);
            let mut list = feeds.remove(port.name.as_str()).unwrap_or_default();
            list.sort();
            w.len(list.len());
            for (_, from, from_port) in list {
                w.text(from_port);
                match canvas.nodes().get(&from) {
                    // A source contributes its recipe: what it emits.
                    Some(upstream) if is_source(self.kinds, upstream) => match self.recipe(upstream)? {
                        Ok(recipe) => {
                            w.text("source");
                            w.digest(&recipe);
                        }
                        Err(rejected) => return Ok(Err(rejected)),
                    },
                    // Anything else contributes the content of the result it currently lends.
                    Some(upstream) => match self.output(upstream, from_port)? {
                        Some(digest) => {
                            w.text("result");
                            w.digest(&digest);
                        }
                        None => w.text("empty"),
                    },
                    None => w.text("missing"),
                }
            }
        }
        Ok(Ok(Blake3::from_bytes(w.finish())))
    }
}

fn is_source(kinds: &Registry, node: &Node) -> bool {
    kinds.get(&node.kind).is_ok_and(|kind| kind.descriptor().category == Category::Source)
}

/// The hash a result of `id` would record if it ran now. `Err` when the node isn't on the canvas,
/// its kind rejects it (or a source feeding it), its routers can't be resolved, or the canvas is
/// too large to hash.
pub fn input_hash(canvas: &Canvas, kinds: &Registry, id: NodeId) -> Result<Blake3, Error> {
    Query::new(canvas, kinds, MAX_QUERY_BYTES)?.hash(id)?
}

/// Nodes whose active result is stale: its recorded input hash differs from the current one, or
/// a node upstream of it is stale (it was built on out-of-date data). Nodes without a result, or
/// whose result recorded no hash (made before hashing), or that their kind rejects, are never
/// stale themselves. The set is transitive, as `planner::plan` expects. `Err` when routers can't
/// be resolved or the canvas is too large to check ([`MAX_QUERY_BYTES`]).
pub fn stale(canvas: &Canvas, kinds: &Registry) -> Result<BTreeSet<NodeId>, Error> {
    stale_within(canvas, kinds, MAX_QUERY_BYTES).map(|(stale, _)| stale)
}

/// [`stale`] with its own byte budget; also returns the bytes hashed.
fn stale_within(canvas: &Canvas, kinds: &Registry, budget: usize) -> Result<(BTreeSet<NodeId>, usize), Error> {
    let has_result = |node: &Node| node.active.is_some_and(|active| node.history.iter().any(|g| g.id == active));
    let mut query = Query::new(canvas, kinds, budget)?;
    let mut stale = BTreeSet::new();
    for node in canvas.nodes().values() {
        let recorded = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)).and_then(|g| g.input_hash);
        if let Some(recorded) = recorded
            && query.hash(node.id)?.is_ok_and(|now| now != recorded)
        {
            stale.insert(node.id);
        }
    }
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
    Ok((stale, query.meter.hashed))
}

#[cfg(test)]
mod tests;
