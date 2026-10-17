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
//! (memoized), streams values into the hasher instead of serializing them, and charges their
//! bytes, and a small cost per feed, against a shared budget ([`MAX_QUERY_BYTES`]); a canvas
//! past it is refused, not hashed for minutes.
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
    /// JSON (bounded by the node size limit). Each value and its text are charged to `meter`
    /// before they are hashed; `None` when the caller already charged them.
    fn value(&mut self, value: &Value, mut meter: Option<&mut Meter>) -> Result<(), Error> {
        let mut charge = |bytes: usize| meter.as_deref_mut().map_or(Ok(()), |m| m.charge(bytes));
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            charge(VALUE_COST)?;
            match value {
                Value::Text(text) => {
                    charge(text.len())?;
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
                    charge(json.len())?;
                    self.text("v");
                    self.bytes(&json);
                }
            }
        }
        Ok(())
    }
    /// A parameter whose bytes were charged already ([`stored_bytes`]).
    fn param(&mut self, name: &str, value: &Value) -> Result<(), Error> {
        self.text(name);
        self.value(value, None)
    }
    fn params(&mut self, params: &ParamValues) -> Result<(), Error> {
        self.len(params.len());
        for (name, value) in params {
            self.param(name, value)?;
        }
        Ok(())
    }
    fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

/// Approximate bytes of a node's stored parameters and configuration: charged once, before its
/// kind resolves (and copies) them and before they are hashed. (Defaults the kind fills in come
/// from its schema, which is small and bounded.)
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

/// Resolved feeds of every node, owned so a hasher can outlive the borrow of its canvas:
/// target → (port, position, upstream, upstream port).
type Feeds = BTreeMap<NodeId, Vec<(String, u32, NodeId, String)>>;

/// Input hashes over one canvas, reusable across queries: routers are resolved once, and each
/// node's recipe and each upstream output are digested once (memoized), all charged to one byte
/// budget. The canvas and kinds are passed to every call and must be the ones it was built with.
pub struct InputHasher {
    feeds: Feeds,
    /// Consumers whose inputs go through an invalid router, and the first such router.
    invalid: BTreeMap<NodeId, NodeId>,
    recipes: BTreeMap<NodeId, Recipe>,
    /// Upstream output digests by (node, port); `None` when it has no usable result there.
    outputs: BTreeMap<(NodeId, String), Option<[u8; 32]>>,
    meter: Meter,
}

impl InputHasher {
    /// A hasher for `canvas` with a byte budget of `budget` across all its queries.
    pub fn new(canvas: &Canvas, kinds: &Registry, budget: usize) -> Result<Self, Error> {
        let incoming = crate::routing::incoming(canvas, kinds)?;
        let mut meter = Meter { left: budget, hashed: 0 };
        let mut feeds: Feeds = BTreeMap::new();
        for (target, list) in &incoming.feeds {
            let mut owned = Vec::with_capacity(list.len());
            for (port, order, from, from_port) in list {
                // Copying the feed index is work too.
                meter.charge(VALUE_COST.saturating_add(port.len()).saturating_add(from_port.len()))?;
                owned.push(((*port).to_owned(), *order, *from, (*from_port).to_owned()));
            }
            feeds.insert(*target, owned);
        }
        let mut invalid = BTreeMap::new();
        for ((target, _), router) in &incoming.invalid {
            invalid.entry(*target).or_insert(*router);
        }
        Ok(Self { feeds, invalid, recipes: BTreeMap::new(), outputs: BTreeMap::new(), meter })
    }

    /// Bytes hashed so far.
    pub fn hashed(&self) -> usize {
        self.meter.hashed
    }

    /// Kind, configuration and shaping parameters: the visible resolved values when the kind is
    /// installed (hidden ones and defaults as the kind applies them), else what is stored.
    /// Outer `Err`: the query is over budget.
    fn recipe(&mut self, kinds: &Registry, node: &Node) -> Result<Recipe, Error> {
        if let Some(recipe) = self.recipes.get(&node.id) {
            return Ok(recipe.clone());
        }
        self.meter.charge(stored_bytes(node))?;
        let mut w = Writer::new();
        w.text(&node.kind);
        w.params(&node.config)?;
        let recipe = match kinds.get(&node.kind) {
            Err(_) => {
                w.params(&node.params)?;
                Ok(w.finish())
            }
            Ok(kind) => match kind.validate_instance(node, false) {
                Err(rejected) => Err(rejected),
                Ok(resolved) => {
                    let visible: Vec<_> = resolved.visible_values().collect();
                    w.len(visible.len());
                    for (name, value) in visible {
                        w.param(name, value)?;
                    }
                    Ok(w.finish())
                }
            },
        };
        self.recipes.insert(node.id, recipe.clone());
        Ok(recipe)
    }

    /// The content of `upstream`'s result on `port`: `produced` (a run's own outputs, in
    /// `upstream.outputs` order) when given, else its active result. `None` when it has none it
    /// can lend (no result, or one recorded for another port layout).
    fn output(&mut self, upstream: &Node, port: &str, produced: Option<&[Value]>) -> Result<Option<[u8; 32]>, Error> {
        if let Some(digest) = self.outputs.get(&(upstream.id, port.to_owned())) {
            return Ok(*digest);
        }
        let outputs = match produced {
            Some(values) => Some(values),
            None => upstream.active.and_then(|active| upstream.history.iter().find(|g| g.id == active)).map(|g| g.outputs.as_slice()),
        };
        // Results are positional; one recorded for a different port layout can't be used.
        let value = outputs
            .filter(|outputs| outputs.len() == upstream.outputs.len())
            .and_then(|outputs| upstream.outputs.iter().position(|p| p.name == port).and_then(|i| outputs.get(i)));
        let digest = match value {
            Some(value) => {
                let mut w = Writer::new();
                w.value(value, Some(&mut self.meter))?;
                Some(w.finish())
            }
            None => None,
        };
        self.outputs.insert((upstream.id, port.to_owned()), digest);
        Ok(digest)
    }

    /// The hash a result of `id` would record if it ran now, with `produced` standing in for the
    /// active results of the nodes it lists (a run's own outputs, positional). `Ok(None)`: it (or
    /// a source feeding it) is rejected by its kind, or its inputs go through an invalid router,
    /// so it has no hash. `Err`: the node isn't on the canvas, or the budget is spent.
    ///
    /// A node's output digest is memoized the first time it is read, so give a node's produced
    /// outputs before hashing anything downstream of it.
    pub fn hash(&mut self, canvas: &Canvas, kinds: &Registry, id: NodeId, produced: &BTreeMap<NodeId, Vec<Value>>) -> Result<Option<Blake3>, Error> {
        if self.invalid.contains_key(&id) {
            return Ok(None);
        }
        let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node is not on this canvas"))?;
        let mut w = Writer::new();
        match self.recipe(kinds, node)? {
            Ok(recipe) => w.digest(&recipe),
            Err(_) => return Ok(None),
        }
        let mut feeds: BTreeMap<String, Vec<(u32, NodeId, String)>> = BTreeMap::new();
        for (port, order, from, from_port) in self.feeds.get(&id).into_iter().flatten() {
            feeds.entry(port.clone()).or_default().push((*order, *from, from_port.clone()));
        }
        for port in &node.inputs {
            w.text(&port.name);
            let mut list = feeds.remove(port.name.as_str()).unwrap_or_default();
            list.sort();
            w.len(list.len());
            for (_, from, from_port) in list {
                // Each feed costs a little, so huge fan-ins count against the budget too.
                self.meter.charge(VALUE_COST.saturating_add(from_port.len()))?;
                w.text(&from_port);
                match canvas.nodes().get(&from) {
                    // A source contributes its recipe: what it emits.
                    Some(upstream) if is_source(kinds, upstream) => match self.recipe(kinds, upstream)? {
                        Ok(recipe) => {
                            w.text("source");
                            w.digest(&recipe);
                        }
                        Err(_) => return Ok(None),
                    },
                    // Anything else contributes the content of the result it lends.
                    Some(upstream) => match self.output(upstream, &from_port, produced.get(&from).map(Vec::as_slice))? {
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
        Ok(Some(Blake3::from_bytes(w.finish())))
    }
}

fn is_source(kinds: &Registry, node: &Node) -> bool {
    kinds.get(&node.kind).is_ok_and(|kind| kind.descriptor().category == Category::Source)
}

/// The hash a result of `id` would record if it ran now. `Err` when the node isn't on the canvas,
/// its kind rejects it (or a source feeding it), its routers can't be resolved, or the canvas is
/// too large to hash.
pub fn input_hash(canvas: &Canvas, kinds: &Registry, id: NodeId) -> Result<Blake3, Error> {
    let mut hasher = InputHasher::new(canvas, kinds, MAX_QUERY_BYTES)?;
    if hasher.invalid.contains_key(&id) {
        return Err(Error::new(ErrorKind::InvalidInput, "its inputs depend on an invalid router"));
    }
    if let Some(node) = canvas.nodes().get(&id)
        && let Some(Err(rejected)) = hasher.recipe(kinds, node).ok()
    {
        return Err(rejected);
    }
    hasher.hash(canvas, kinds, id, &BTreeMap::new())?.ok_or_else(|| Error::new(ErrorKind::InvalidInput, "a source feeding it is invalid"))
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
    let mut query = InputHasher::new(canvas, kinds, budget)?;
    let none = BTreeMap::new();
    let mut stale = BTreeSet::new();
    for node in canvas.nodes().values() {
        let recorded = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)).and_then(|g| g.input_hash);
        if let Some(recorded) = recorded
            && query.hash(canvas, kinds, node.id, &none)?.is_some_and(|now| now != recorded)
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
