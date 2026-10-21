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
//! effective payloads (including omitted/hidden defaults), names and a small cost per feed
//! against a shared admission budget ([`MAX_QUERY_BYTES`]). Text counts exactly; scalar/media
//! JSON reserves conservative upper bounds, so a query may refuse before its exact JSON bytes
//! reach the cap. A canvas past the budget is refused before parameter resolution copies it.
//! Dynamic instance/validation hooks own their internal allocations. Descriptor defaults are
//! admitted before authoritative validation; additional/transformed effective values returned by
//! custom validators are admitted before hashing. Native hooks may allocate before returning.
//! Rejected recipes retain a sanitized reason of at most 256 bytes, charged once (128 + reason
//! bytes). Detailed stale reports reserve another 576 bytes before routing or recipe hooks, even
//! when no unknown result is found. Standalone diagnostics pay those bytes before construction.
//! Reports can refuse earlier; the 256 MiB ceiling and legacy v2 hash budgets are unchanged.
//!
//! **Recipe versions:** the hash starts with a domain tag ([`DOMAIN`]). The stored hash carries no
//! version, so after a change to the recipe an old result can't be told from one whose inputs
//! changed: it is reported **stale** (re-running refreshes it). Deliberately conservative: a
//! result is never shown as fresh when we can't vouch for it.

use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, Registry, mention};
use nodecraft_types::{Blake3, Error, ErrorKind, NodeId, ParamValues, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

/// The node `config` key holding whether it reuses identical results (#91): a boolean.
pub const CACHE_CONFIG: &str = "nodecraft.cache";

/// Domain tag mixed into every hash. Bump it when what is hashed changes; results stamped under
/// an older tag are then reported stale (see the module docs).
pub const DOMAIN: &[u8] = b"nodecraft.input-hash.v2";

/// Admission budget for recipes, resolved values and upstream outputs, each once.
/// Scalar/media JSON uses conservative encoded-size reservations, not exact bytes hashed.
pub const MAX_QUERY_BYTES: usize = 256 * 1024 * 1024;

/// A checked result whose current input hash is unknown, with its actual failing node.
/// Reasons are the same bounded public-safe messages as [`input_hash`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashDiagnostic {
    pub node: NodeId,
    pub cause: NodeId,
    pub error: Error,
}

/// Known stale results plus the first local invalidity, in canvas NodeId order.
/// `unknown_total` counts checked results with recorded hashes whose current hash is unavailable,
/// including multiple results sharing one rejected upstream node; it is not a unique-node count.
/// A stale set alone is not proof that every other node is fresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleReport {
    pub nodes: BTreeSet<NodeId>,
    pub unknown_total: usize,
    pub unknown: Option<HashDiagnostic>,
}

/// Fixed cost charged per value on top of its payload, so huge lists of tiny values count too.
const VALUE_COST: usize = 16;

fn too_large() -> Error {
    Error::new(ErrorKind::InvalidInput, "the canvas holds too much data to check for stale results; split it into smaller canvases")
}

/// Bytes left in a query's budget, and admission bytes charged so far.
struct Meter {
    left: usize,
    hashed: usize,
    refused: bool,
}

impl Meter {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        if self.refused {
            return Err(too_large());
        }
        let Some(left) = self.left.checked_sub(bytes) else {
            self.refused = true;
            return Err(too_large());
        };
        self.left = left;
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
    /// A value, streamed with the v2 spelling unchanged. Charge the complete borrowed
    /// payload before serialization; scalar/media JSON reserves a conservative upper bound.
    fn value(&mut self, value: &Value, meter: Option<&mut Meter>) -> Result<(), Error> {
        if let Some(meter) = meter {
            charge_value(meter, value)?;
        }
        let mut stack = vec![std::slice::from_ref(value).iter()];
        while let Some(items) = stack.last_mut() {
            let Some(value) = items.next() else {
                stack.pop();
                continue;
            };
            match value {
                Value::Text(text) => {
                    self.text("t");
                    self.text(text);
                }
                Value::List(items) => {
                    self.text("l");
                    self.len(items.len());
                    stack.push(items.iter());
                }
                other => {
                    let json = serde_json::to_vec(other).map_err(|_| Error::new(ErrorKind::Internal, "a value couldn't be hashed"))?;
                    self.text("v");
                    self.bytes(&json);
                }
            }
        }
        Ok(())
    }
    /// A parameter whose stored or omitted-default payload was admitted already.
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
    fn generation_contract(&mut self, contract: &nodecraft_nodes::NodeContract, meter: &mut Meter) -> Result<(), Error> {
        // instance_contract is a trusted native hook, like validate_instance above. Its
        // returned owner is charged here; serialization streams twice with bounded scratch,
        // first for exact encoded admission then hashing, never a full encoded copy.
        let mut retained = std::mem::size_of::<nodecraft_nodes::NodeContract>();
        let mut retain = |bytes: usize| -> Result<(), Error> {
            retained = retained.checked_add(bytes).ok_or_else(too_large)?;
            Ok(())
        };
        retain(contract.kind.len())?;
        retain(contract.owned_groups.len().checked_mul(std::mem::size_of::<nodecraft_types::Contract>()).ok_or_else(too_large)?)?;
        retain(contract.inputs.len().checked_mul(std::mem::size_of::<nodecraft_nodes::InputContract>()).ok_or_else(too_large)?)?;
        for input in &contract.inputs {
            retain(input.port.len())?;
            retain(input.role.len())?;
        }
        retain(contract.outputs.len().checked_mul(std::mem::size_of::<nodecraft_nodes::OutputContract>()).ok_or_else(too_large)?)?;
        for output in &contract.outputs {
            retain(output.port.len())?;
            retain(output.role.len())?;
        }
        retain(contract.modes.len().checked_mul(std::mem::size_of::<nodecraft_nodes::ModeContract>()).ok_or_else(too_large)?)?;
        for mode in &contract.modes {
            retain(mode.mode.as_str().len())?;
            retain(mode.inputs.len().checked_mul(std::mem::size_of::<nodecraft_nodes::PortCount>()).ok_or_else(too_large)?)?;
            for input in &mode.inputs {
                retain(input.port.len())?;
            }
        }
        for group in contract.parameter_groups() {
            retained = retained.checked_add(crate::admission::measure::contract(group).map_err(|_| too_large())?).ok_or_else(too_large)?;
        }
        meter.charge(retained)?;
        struct Sink<'a> {
            hash: Option<&'a mut blake3::Hasher>,
            bytes: usize,
        }
        impl std::io::Write for Sink<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes = self
                    .bytes
                    .checked_add(bytes.len())
                    .filter(|n| *n <= MAX_QUERY_BYTES)
                    .ok_or_else(|| std::io::Error::other("contract hash exceeds admission"))?;
                if let Some(hash) = &mut self.hash {
                    hash.update(bytes);
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Sink { hash: None, bytes: 0 };
        serde_json::to_writer(&mut count, contract).map_err(|_| too_large())?;
        meter.charge(count.bytes)?;
        self.text("generation.parameter-contract.v1");
        self.len(count.bytes);
        let mut hash = Sink { hash: Some(&mut self.0), bytes: 0 };
        serde_json::to_writer(&mut hash, contract).map_err(|_| too_large())?;
        Ok(())
    }
    fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

/// Preflight borrowed values before a proportional clone/JSON allocation. Text payloads
/// count exactly; scalar JSON reserves 128 bytes (tag/envelope plus at most a finite number
/// or 20 decimal digits). Media reserves 512 bytes for the tag, digest, metadata keys and
/// six maximal integers and the image header facts, plus six bytes per MIME byte for worst-case JSON escaping.
/// Mime is independently capped at 1024 bytes. These upper bounds deliberately overcharge
/// short encodings. Keep the traversal stack proportional to depth, not list length.
fn charge_value(meter: &mut Meter, value: &Value) -> Result<(), Error> {
    let mut stack = vec![std::slice::from_ref(value).iter()];
    while let Some(items) = stack.last_mut() {
        let Some(value) = items.next() else {
            stack.pop();
            continue;
        };
        meter.charge(VALUE_COST)?;
        match value {
            Value::Text(text) => meter.charge(text.len())?,
            Value::List(items) => stack.push(items.iter()),
            Value::Media(asset) => meter.charge(512usize.saturating_add(asset.mime.as_str().len().saturating_mul(6)))?,
            _ => meter.charge(128)?,
        }
    }
    Ok(())
}

/// Stored payload is admitted once before the kind's instance hook. It covers hidden
/// overrides too: schema resolution retains/copies them even though they aren't hashed.
fn charge_stored(meter: &mut Meter, node: &Node) -> Result<(), Error> {
    meter.charge(node.kind.len())?;
    for port in &node.inputs {
        meter.charge(port.name.len())?;
    }
    for (name, value) in node.params.iter().chain(node.config.iter()) {
        meter.charge(name.len())?;
        charge_value(meter, value)?;
    }
    Ok(())
}

/// A node's recipe digest, or why its kind rejects it (then it has no meaningful hash: unknown).
type Recipe = Result<[u8; 32], CachedRejection>;

const MAX_REJECTION_REASON_BYTES: usize = 256;
const REJECTION_PREFIX_BYTES: usize = MAX_REJECTION_REASON_BYTES - 3; // UTF-8 ellipsis
const CACHE_REJECTION_OVERHEAD: usize = 128;
const MAX_DIAGNOSTIC_BYTES: usize = 512;
const DIAGNOSTIC_OUTPUT_OVERHEAD: usize = 64;

struct CachedRejection {
    kind: ErrorKind,
    reason: String,
}
/// How many batches feeding batches staleness follows before calling a result unknown.
const MAX_BATCH_DEPTH: usize = 32;
#[derive(Clone, Copy)]
enum InvalidHash {
    Router(NodeId),
    Target(NodeId),
    Source(NodeId),
}
enum HashResult {
    Ready(Blake3),
    Invalid(InvalidHash),
}
fn diagnostic_char(ch: char) -> char {
    if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
        ' '
    } else {
        ch
    }
}
// Counting and copying each inspect at most 254 scalars, irrespective of native hook message
// length. No original Error/message clone or full-input formatting is needed.
fn reason_shape(text: &str) -> (usize, bool) {
    let mut bytes = 0usize;
    for ch in text.chars().map(diagnostic_char) {
        if bytes.saturating_add(ch.len_utf8()) > REJECTION_PREFIX_BYTES {
            return (bytes, true);
        }
        bytes += ch.len_utf8();
    }
    (bytes, false)
}

/// Resolved feeds of every node, owned so a hasher can outlive the borrow of its canvas:
/// target → (port, position, upstream, upstream port).
type Feeds = BTreeMap<NodeId, Vec<(String, u32, NodeId, String)>>;

// Created only by the report constructor after charging its own meter; consumed once.
struct DiagnosticCredit;

/// Input hashes over one canvas, reusable across queries: routers are resolved once, and each
/// node's recipe and each upstream output are digested once (memoized), all charged to one byte
/// budget. Once an admission exceeds that shared budget, subsequent hashes return the budget
/// error; node-local invalidity does not end the query. The canvas and kinds are passed to every
/// call and must be the ones it was built with.
pub struct InputHasher {
    feeds: Feeds,
    /// Consumers whose inputs go through an invalid router, and the first such router.
    invalid: BTreeMap<NodeId, NodeId>,
    recipes: BTreeMap<NodeId, Recipe>,
    /// Upstream output digests by (node, port); `None` when it has no usable result there.
    outputs: BTreeMap<(NodeId, String), Option<[u8; 32]>>,
    /// Batch hashes (#92): `None` while one is being hashed, so a cycle of batches ends.
    batches: BTreeMap<NodeId, Option<Result<Blake3, InvalidHash>>>,
    /// Batches being hashed right now; a chain deeper than [`MAX_BATCH_DEPTH`] is unknown.
    batch_depth: usize,
    /// Whether a node's result is fanned out by batches (#519), by node.
    fanned: BTreeMap<NodeId, bool>,
    meter: Meter,
    diagnostic_credit: Option<DiagnosticCredit>,
    #[cfg(test)]
    diagnostic_cache_bytes: usize,
    #[cfg(test)]
    diagnostic_output_bytes: usize,
}

impl InputHasher {
    /// A hasher for `canvas` with a byte budget of `budget` across all its queries.
    pub fn new(canvas: &Canvas, kinds: &Registry, budget: usize) -> Result<Self, Error> {
        Self::with_meter(canvas, kinds, Meter { left: budget, hashed: 0, refused: false }, None)
    }

    fn for_report(canvas: &Canvas, kinds: &Registry, budget: usize) -> Result<Self, Error> {
        let mut meter = Meter { left: budget, hashed: 0, refused: false };
        // Routing itself invokes router validation hooks, so reserve before constructing it.
        meter.charge(MAX_DIAGNOSTIC_BYTES + DIAGNOSTIC_OUTPUT_OVERHEAD)?;
        Self::with_meter(canvas, kinds, meter, Some(DiagnosticCredit))
    }

    fn with_meter(canvas: &Canvas, kinds: &Registry, mut meter: Meter, diagnostic_credit: Option<DiagnosticCredit>) -> Result<Self, Error> {
        let incoming = crate::routing::incoming(canvas, kinds)?;
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
        Ok(Self {
            feeds,
            invalid,
            recipes: BTreeMap::new(),
            outputs: BTreeMap::new(),
            batches: BTreeMap::new(),
            batch_depth: 0,
            fanned: BTreeMap::new(),
            meter,
            diagnostic_credit,
            #[cfg(test)]
            diagnostic_cache_bytes: 0,
            #[cfg(test)]
            diagnostic_output_bytes: 0,
        })
    }

    /// Admission bytes charged so far, including values, feed index and retained/returned diagnostics.
    pub fn hashed(&self) -> usize {
        self.meter.hashed
    }

    /// Kind, configuration and shaping parameters: the visible resolved values when the kind is
    /// installed (hidden ones and defaults as the kind applies them), else what is stored.
    /// Outer `Err`: the query is over budget.
    fn recipe(&mut self, kinds: &Registry, node: &Node) -> Result<Option<[u8; 32]>, Error> {
        if let Some(recipe) = self.recipes.get(&node.id) {
            return Ok(recipe.as_ref().ok().copied());
        }
        charge_stored(&mut self.meter, node)?;
        let mut w = Writer::new();
        w.text(&node.kind);
        // Whether a node reuses cached results (#91) doesn't change what it makes: leave the
        // switch out, so turning it on or off never makes a result stale.
        if node.config.contains_key(CACHE_CONFIG) {
            let mut config = node.config.clone();
            config.remove(CACHE_CONFIG);
            w.params(&config)?;
        } else {
            w.params(&node.config)?;
        }
        let recipe = match kinds.get(&node.kind) {
            Err(_) => {
                w.params(&node.params)?;
                Ok(w.finish())
            }
            Ok(kind) => match kind.resolve_instance(node) {
                Err(rejected) => Err(rejected),
                Ok(descriptor) => {
                    if node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
                        Err(Error::new(ErrorKind::InvalidInput, "node ports do not match its instance descriptor"))
                    } else {
                        // An instance hook owns its bounded schema construction; this admission
                        // precedes resolution's clones of defaults and supplied values. Hidden
                        // defaults count too because resolution retains them in the values map.
                        let mut admitted_defaults = BTreeMap::new();
                        for spec in &descriptor.params.params {
                            self.meter.charge(spec.name.len())?; // visibility-set keys
                            if !node.params.contains_key(&spec.name)
                                && let Some(value) = &spec.default
                            {
                                self.meter.charge(spec.name.len())?; // new values-map key
                                charge_value(&mut self.meter, value)?;
                            }
                            admitted_defaults.insert(spec.name.as_str(), spec.default.as_ref());
                        }
                        // Compatibility: validate_instance is authoritative and overridable.
                        // The default hook resolves the instance again, only after successful
                        // preflight. Native custom hooks own allocations made before returning.
                        match kind.validate_instance(node, false) {
                            Err(rejected) => Err(rejected),
                            Ok(resolved) => {
                                if nodecraft_nodes::generation::is_generation(&node.kind) {
                                    let mut contract = kind.instance_contract(node)?;
                                    // Whether a list port fans out or collects is hashed per
                                    // feed instead (#519): only a batch behind it changes what
                                    // the node makes, so declaring it never makes results stale.
                                    for input in &mut contract.inputs {
                                        input.fans_out = false;
                                    }
                                    w.generation_contract(&contract, &mut self.meter)?;
                                }
                                for (name, value) in &resolved.values {
                                    let admitted = node.params.get(name).or_else(|| admitted_defaults.get(name.as_str()).copied().flatten());
                                    if admitted.is_none() {
                                        self.meter.charge(name.len())?;
                                    }
                                    // Compare only the corresponding already-admitted value:
                                    // its validated depth/size bounds equality's traversal. Never
                                    // recursively inspect an arbitrary hook payload on its own.
                                    if admitted != Some(value) {
                                        charge_value(&mut self.meter, value)?;
                                    }
                                }
                                for name in &resolved.visible {
                                    if !admitted_defaults.contains_key(name.as_str()) {
                                        self.meter.charge(name.len())?;
                                    }
                                }
                                // What is sent, less what the contract declares `identity: false`.
                                let hashed: Vec<_> = resolved.identity_values().collect();
                                w.len(hashed.len());
                                for (name, value) in hashed {
                                    w.param(name, value)?;
                                }
                                Ok(w.finish())
                            }
                        }
                    }
                }
            },
        };
        let recipe = match recipe {
            Ok(digest) => Ok(digest),
            Err(rejected) => Err(self.cache_rejection(rejected)?),
        };
        let digest = recipe.as_ref().ok().copied();
        self.recipes.insert(node.id, recipe);
        Ok(digest)
    }

    fn cache_rejection(&mut self, rejected: Error) -> Result<CachedRejection, Error> {
        let text = if rejected.message.is_empty() { "the node kind rejected this node" } else { &rejected.message };
        let (prefix, truncated) = reason_shape(text);
        let bytes = prefix.saturating_add(if truncated { 3 } else { 0 });
        // New ownership is admitted before allocating/copying even a bounded prefix. Native
        // hooks own the original returned Error; we retain only this bounded public-safe reason.
        self.meter.charge(CACHE_REJECTION_OVERHEAD.saturating_add(bytes))?;
        let mut reason = String::with_capacity(bytes);
        for ch in text.chars().map(diagnostic_char) {
            if reason.len().saturating_add(ch.len_utf8()) > prefix {
                break;
            }
            reason.push(ch);
        }
        if truncated {
            reason.push('…');
        }
        #[cfg(test)]
        {
            self.diagnostic_cache_bytes += reason.len();
        }
        Ok(CachedRejection { kind: rejected.kind, reason })
    }

    fn diagnostic(&mut self, invalid: InvalidHash) -> Result<Error, Error> {
        let rejection = match invalid {
            InvalidHash::Router(_) => None,
            InvalidHash::Target(node) | InvalidHash::Source(node) => Some(
                self.recipes
                    .get(&node)
                    .and_then(|r| r.as_ref().err())
                    .ok_or_else(|| Error::new(ErrorKind::Internal, "a rejected hash has no cached reason"))?,
            ),
        };
        if self.meter.refused {
            return Err(too_large());
        }
        if self.diagnostic_credit.take().is_none() {
            self.meter.charge(MAX_DIAGNOSTIC_BYTES + DIAGNOSTIC_OUTPUT_OVERHEAD)?;
        }
        // Fixed role text + canonical UUID + at most 256 reason bytes fits below 512. Write
        // directly into the admitted String, never an intermediate formatted native Error.
        let mut message = String::with_capacity(MAX_DIAGNOSTIC_BYTES);
        let kind = match invalid {
            InvalidHash::Router(router) => {
                write!(&mut message, "inputs depend on invalid router {router}; check its configuration and ports")
                    .map_err(|_| Error::new(ErrorKind::Internal, "a hash diagnostic could not be formatted"))?;
                ErrorKind::InvalidInput
            }
            InvalidHash::Target(_) => {
                let Some(rejection) = rejection else { return Err(Error::new(ErrorKind::Internal, "a target rejection has no reason")) };
                message.push_str(&rejection.reason);
                rejection.kind
            }
            InvalidHash::Source(source) => {
                let Some(rejection) = rejection else { return Err(Error::new(ErrorKind::Internal, "a source rejection has no reason")) };
                write!(&mut message, "upstream source {source} is invalid: ")
                    .map_err(|_| Error::new(ErrorKind::Internal, "a hash diagnostic could not be formatted"))?;
                message.push_str(&rejection.reason);
                rejection.kind
            }
        };
        #[cfg(test)]
        {
            self.diagnostic_output_bytes += message.len();
        }
        Ok(Error::new(kind, message))
    }

    fn reported(&mut self, node: NodeId, invalid: InvalidHash) -> Result<HashDiagnostic, Error> {
        let cause = match invalid {
            InvalidHash::Router(id) | InvalidHash::Target(id) | InvalidHash::Source(id) => id,
        };
        // Move the admitted Error: two fixed IDs and Error fit its reserved 64-byte overhead.
        Ok(HashDiagnostic { node, cause, error: self.diagnostic(invalid)? })
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
        self.hash_detailed(canvas, kinds, id, produced).map(|result| match result {
            HashResult::Ready(digest) => Some(digest),
            HashResult::Invalid(_) => None,
        })
    }

    fn hash_detailed(&mut self, canvas: &Canvas, kinds: &Registry, id: NodeId, produced: &BTreeMap<NodeId, Vec<Value>>) -> Result<HashResult, Error> {
        // A shared admission failure ends this query, including memoized lookups. A local
        // rejection stays unknown when its bounded record fits; metadata admission can fail too.
        // A successful exact-budget query may still reuse caches.
        if self.meter.refused {
            return Err(too_large());
        }
        if let Some(router) = self.invalid.get(&id) {
            return Ok(HashResult::Invalid(InvalidHash::Router(*router)));
        }
        let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "node is not on this canvas"))?;
        let mut w = Writer::new();
        match self.recipe(kinds, node)? {
            Some(recipe) => w.digest(&recipe),
            None => return Ok(HashResult::Invalid(InvalidHash::Target(node.id))),
        }
        let mut feeds: BTreeMap<String, Vec<(u32, NodeId, String)>> = BTreeMap::new();
        for (port, order, from, from_port) in self.feeds.get(&id).into_iter().flatten() {
            feeds.entry(port.clone()).or_default().push((*order, *from, from_port.clone()));
        }
        let fans_out = fans_out_ports(kinds, node);
        // Cards the prompt `@mentions` feed a port the node doesn't have (#505); only hashed when
        // there are some, so nodes without mentions keep their hashes.
        let mentions = feeds.contains_key(mention::PORT).then_some(mention::PORT);
        for name in node.inputs.iter().map(|port| port.name.as_str()).chain(mentions) {
            w.text(name);
            let mut list = feeds.remove(name).unwrap_or_default();
            list.sort();
            w.len(list.len());
            // A list port that runs once per item (#519) makes something else than one that
            // collects them: mark the ports a batch fans out.
            if fans_out.contains(name) {
                let mut fanned = false;
                for (_, from, _) in &list {
                    fanned |= self.fans(canvas, kinds, *from)?;
                }
                if fanned {
                    w.text("fan");
                }
            }
            for (_, from, from_port) in list {
                // Each feed costs a little, so huge fan-ins count against the budget too.
                self.meter.charge(VALUE_COST.saturating_add(from_port.len()))?;
                w.text(&from_port);
                match canvas.nodes().get(&from) {
                    // A batch (#92) never runs: it contributes its items' recipe, which is its
                    // own settings and whatever feeds it.
                    Some(upstream) if upstream.kind == nodecraft_nodes::batch::ID => match self.batches.get(&from) {
                        Some(Some(Ok(digest))) => {
                            w.text("batch");
                            w.digest(digest.as_bytes());
                        }
                        Some(Some(Err(invalid))) => return Ok(HashResult::Invalid(*invalid)),
                        // Batches feeding each other in a loop: there's nothing to hash.
                        Some(None) => w.text("cycle"),
                        // Batches feeding batches recurse: bound the depth so a hostile file
                        // can't exhaust the stack (the planner refuses such chains anyway).
                        None if self.batch_depth >= MAX_BATCH_DEPTH => {
                            self.batches.insert(from, Some(Err(InvalidHash::Target(from))));
                            return Ok(HashResult::Invalid(InvalidHash::Target(from)));
                        }
                        None => {
                            self.batches.insert(from, None);
                            self.batch_depth = self.batch_depth.saturating_add(1);
                            let hashed = self.hash_detailed(canvas, kinds, from, produced);
                            self.batch_depth = self.batch_depth.saturating_sub(1);
                            let result = match hashed? {
                                HashResult::Ready(digest) => Ok(digest),
                                HashResult::Invalid(invalid) => Err(invalid),
                            };
                            self.batches.insert(from, Some(result));
                            match result {
                                Ok(digest) => {
                                    w.text("batch");
                                    w.digest(digest.as_bytes());
                                }
                                Err(invalid) => return Ok(HashResult::Invalid(invalid)),
                            }
                        }
                    },
                    // A source contributes its recipe: what it emits.
                    Some(upstream) if is_source(kinds, upstream) => match self.recipe(kinds, upstream)? {
                        Some(recipe) => {
                            w.text("source");
                            w.digest(&recipe);
                        }
                        None => return Ok(HashResult::Invalid(InvalidHash::Source(upstream.id))),
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
        Ok(HashResult::Ready(Blake3::from_bytes(w.finish())))
    }
}

impl InputHasher {
    /// Whether `id`'s result is fanned out by batches (#92): it is a batch, or a batch's items
    /// reach it through a single port or one that fans out. Iterative, with a seen-set, so a
    /// deep or cyclic graph is bounded by its size.
    fn fans(&mut self, canvas: &Canvas, kinds: &Registry, id: NodeId) -> Result<bool, Error> {
        if let Some(known) = self.fanned.get(&id) {
            return Ok(*known);
        }
        // A node is pushed once to expand it, then again with its upstream to decide it.
        let mut stack: Vec<(NodeId, Option<Vec<NodeId>>)> = vec![(id, None)];
        let mut seen = BTreeSet::new();
        while let Some((at, upstream)) = stack.pop() {
            if self.fanned.contains_key(&at) {
                continue;
            }
            if let Some(upstream) = upstream {
                // Provisional on a cycle: a node there sees itself as not fanned (the planner
                // refuses cycles, so no run depends on it).
                let fans = upstream.iter().any(|from| self.fanned.get(from).copied().unwrap_or(false));
                self.fanned.insert(at, fans);
                continue;
            }
            if !seen.insert(at) {
                continue;
            }
            let Some(node) = canvas.nodes().get(&at) else {
                self.fanned.insert(at, false);
                continue;
            };
            if node.kind == nodecraft_nodes::batch::ID {
                self.fanned.insert(at, true);
                continue;
            }
            let fans_out = fans_out_ports(kinds, node);
            let carries = |port: &str| fans_out.contains(port) || node.inputs.iter().any(|p| p.name == port && !p.port_type.many);
            let upstream: Vec<NodeId> = self.feeds.get(&at).into_iter().flatten().filter(|(port, ..)| carries(port)).map(|(_, _, from, _)| *from).collect();
            self.meter.charge(VALUE_COST.saturating_mul(upstream.len().saturating_add(1)))?;
            let next: Vec<(NodeId, Option<Vec<NodeId>>)> = upstream.iter().filter(|from| !seen.contains(*from)).map(|from| (*from, None)).collect();
            stack.push((at, Some(upstream)));
            stack.extend(next);
        }
        Ok(self.fanned.get(&id).copied().unwrap_or(false))
    }
}

/// The list ports of `node` that run it once per batch item rather than collecting (#515).
/// A Generate card's depend on its model; other kinds declare theirs statically, so no
/// instance hook runs for them here.
fn fans_out_ports(kinds: &Registry, node: &Node) -> BTreeSet<String> {
    let Ok(kind) = kinds.get(&node.kind) else { return BTreeSet::new() };
    let contract = if nodecraft_nodes::generation::is_generation(&node.kind) { kind.instance_contract(node) } else { Ok(kind.contract(kind.descriptor())) };
    match contract {
        Ok(contract) => contract.inputs.into_iter().filter(|input| input.fans_out).map(|input| input.port).collect(),
        Err(_) => BTreeSet::new(),
    }
}

fn is_source(kinds: &Registry, node: &Node) -> bool {
    kinds.get(&node.kind).is_ok_and(|kind| kind.descriptor().category == Category::Source)
}

/// The hash a result of `id` would record if it ran now. `Err` when the node isn't on the canvas,
/// its kind rejects it (or a source feeding it), its routers can't be resolved, or the canvas is
/// too large to hash. Local failures retain a bounded public-safe reason; upstream source/router
/// errors identify their node, without rerunning kind hooks. Diagnostic copies share the query budget.
pub fn input_hash(canvas: &Canvas, kinds: &Registry, id: NodeId) -> Result<Blake3, Error> {
    input_hash_within(canvas, kinds, id, MAX_QUERY_BYTES)
}

fn input_hash_within(canvas: &Canvas, kinds: &Registry, id: NodeId, budget: usize) -> Result<Blake3, Error> {
    let mut hasher = InputHasher::new(canvas, kinds, budget)?;
    match hasher.hash_detailed(canvas, kinds, id, &BTreeMap::new())? {
        HashResult::Ready(digest) => Ok(digest),
        HashResult::Invalid(invalid) => Err(hasher.diagnostic(invalid)?),
    }
}

/// Nodes whose active result is stale: its recorded input hash differs from the current one, or
/// a node upstream of it is stale (it was built on out-of-date data), or it is partial (#480). Nodes without a result, or
/// whose result recorded no hash (made before hashing), or that their kind rejects, are never
/// stale themselves. The set is transitive, as `planner::plan` expects. `Err` when routers can't
/// be resolved or the canvas is too large to check ([`MAX_QUERY_BYTES`]).
pub fn stale(canvas: &Canvas, kinds: &Registry) -> Result<BTreeSet<NodeId>, Error> {
    stale_within(canvas, kinds, MAX_QUERY_BYTES).map(|(stale, _)| stale)
}

/// [`stale`] with one bounded diagnostic and a count of locally invalid checked results.
/// Uses the same hasher/cache and never repeats hooks to recover a reason. One 576-byte output
/// is reserved before routing or hashing, even without an unknown result. Formatting consumes it
/// without another charge; refusal returns an error, not a partial report.
pub fn stale_report(canvas: &Canvas, kinds: &Registry) -> Result<StaleReport, Error> {
    query_stale(canvas, kinds, MAX_QUERY_BYTES, true).map(|(report, _)| report)
}

/// [`stale`] with its own admission budget; also returns charged bytes (including reservations).
fn stale_within(canvas: &Canvas, kinds: &Registry, budget: usize) -> Result<(BTreeSet<NodeId>, usize), Error> {
    query_stale(canvas, kinds, budget, false).map(|(report, charged)| (report.nodes, charged))
}

fn query_stale(canvas: &Canvas, kinds: &Registry, budget: usize, details: bool) -> Result<(StaleReport, usize), Error> {
    let has_result = |node: &Node| node.active.is_some_and(|active| node.history.iter().any(|g| g.id == active));
    let mut query = if details { InputHasher::for_report(canvas, kinds, budget)? } else { InputHasher::new(canvas, kinds, budget)? };
    let none = BTreeMap::new();
    let mut stale = BTreeSet::new();
    let mut unknown_total = 0usize;
    let mut first_unknown = None;
    for node in canvas.nodes().values() {
        let active = node.active.and_then(|active| node.history.iter().find(|g| g.id == active));
        // A partial result (#480) waits to be completed, whatever its inputs.
        if active.is_some_and(nodecraft_graph::Generation::is_partial) {
            stale.insert(node.id);
            continue;
        }
        if let Some(recorded) = active.and_then(|g| g.input_hash) {
            match query.hash_detailed(canvas, kinds, node.id, &none)? {
                HashResult::Ready(now) if now != recorded => {
                    stale.insert(node.id);
                }
                HashResult::Invalid(invalid) if details => {
                    unknown_total = unknown_total.saturating_add(1);
                    first_unknown.get_or_insert((node.id, invalid));
                }
                _ => {}
            }
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
    let unknown = first_unknown.map(|(node, invalid)| query.reported(node, invalid)).transpose()?;
    Ok((StaleReport { nodes: stale, unknown_total, unknown }, query.meter.hashed))
}

#[cfg(test)]
mod tests;
