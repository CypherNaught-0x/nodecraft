//! Opt-in result caching (#91): a node about to run whose inputs (kind, settings, input values;
//! the input hash, #75) match a result already made reuses that result instead of computing or
//! paying again.
//!
//! - **Where results come from.** The node's own history first (a setting changed back selects
//!   the result it had then); otherwise another node of the same kind on the canvas whose result
//!   has the same input hash (copied as a new generation). Nothing outside the project is looked at.
//! - **Who reuses.** A node's `config["nodecraft.cache"]` decides; without it, every kind that
//!   doesn't call a model reuses, and kinds that call a model don't, because a model with the same inputs may be wanted
//!   again for a different take. The switch is left out of the input hash, so turning it on or off
//!   never makes a result stale.
//! - **Results only.** A hit changes the document like any result does (one undo step, or the
//!   edit's own step when live recompute applies it), and costs nothing.
use nodecraft_exec::staleness::{CACHE_CONFIG, InputHasher, MAX_QUERY_BYTES};
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Project};
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::{Blake3, CanvasId, Error, GenerationId, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Whether `node` reuses identical results.
pub fn enabled(node: &Node, kinds: &Registry) -> bool {
    match node.config.get(CACHE_CONFIG) {
        Some(Value::Bool(on)) => *on,
        _ => kinds.get(&node.kind).is_ok_and(|k| k.descriptor().category != Category::Generation),
    }
}

/// A result `node` can reuse.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// One of its own results: select it.
    Own { node: NodeId, generation: GenerationId },
    /// Another node's: copy it as a new result of this node.
    Copy { node: NodeId, generation: Generation },
}

impl Hit {
    pub fn node(&self) -> NodeId {
        match self {
            Hit::Own { node, .. } | Hit::Copy { node, .. } => *node,
        }
    }
}

/// Most results indexed on a canvas per round.
const MAX_SCANNED: usize = 100_000;
/// Most hits one call returns (what one step can record).
pub const MAX_HITS: usize = crate::run_cmds::MAX_RESULTS;

/// A fresh random generation id.
pub(crate) fn fresh_id() -> Result<GenerationId, Error> {
    let bytes = crate::execution_identity::nonce()?;
    let mut id = [0u8; 16];
    id.copy_from_slice(bytes.get(..16).unwrap_or(&[0; 16]));
    Ok(GenerationId::from_bytes(id))
}

/// The hits for `nodes` on `canvas_id` of `project` (those with caching on whose active result
/// isn't already current), each node at most once and at most [`MAX_HITS`]. Chains are
/// followed: a hit's node is treated as holding its reused result when the nodes it feeds are
/// looked at, so one call settles a whole chain. A copy its node couldn't record (too large) is
/// skipped, never returned.
pub fn hits(
    project: &Project,
    canvas_id: CanvasId,
    kinds: &Registry,
    nodes: impl IntoIterator<Item = NodeId>,
    mut fresh: impl FnMut() -> Result<GenerationId, Error>,
) -> Result<Vec<Hit>, Error> {
    let Some(canvas) = project.canvases().get(&canvas_id) else { return Ok(Vec::new()) };
    let mut wanted: BTreeSet<NodeId> = nodes.into_iter().filter(|id| canvas.nodes().get(id).is_some_and(|n| enabled(n, kinds))).collect();
    let mut scratch = project.clone();
    let mut found: Vec<Hit> = Vec::new();
    while !wanted.is_empty() && found.len() < MAX_HITS {
        let Some(canvas) = scratch.canvases().get(&canvas_id) else { break };
        // Every result on the canvas by kind and input hash, once per round.
        let mut index: BTreeMap<(&str, Blake3), (NodeId, &Generation)> = BTreeMap::new();
        for node in canvas.nodes().values() {
            for generation in &node.history {
                if index.len() >= MAX_SCANNED {
                    break;
                }
                if let Some(hash) = generation.input_hash {
                    index.entry((node.kind.as_str(), hash)).or_insert((node.id, generation));
                }
            }
        }
        let mut hasher = InputHasher::new(canvas, kinds, MAX_QUERY_BYTES)?;
        let mut round = Vec::new();
        let mut done = Vec::new();
        for id in &wanted {
            let Some(node) = canvas.nodes().get(id) else {
                done.push(*id);
                continue;
            };
            let Ok(Some(current)) = hasher.hash(canvas, kinds, *id, &BTreeMap::new()) else { continue };
            let active = node.active.and_then(|a| node.history.iter().find(|g| g.id == a));
            // Current now; a hit upstream in this round may still make it reusable next round.
            if active.is_some_and(|g| g.input_hash == Some(current)) {
                continue;
            }
            if let Some(own) = node.history.iter().find(|g| g.input_hash == Some(current)) {
                round.push(Hit::Own { node: *id, generation: own.id });
            } else if let Some((_, other)) = index.get(&(node.kind.as_str(), current)).filter(|(owner, _)| owner != id) {
                let generation = Generation { id: fresh()?, outputs: other.outputs.clone(), input_hash: Some(current), items: Vec::new() };
                if nodecraft_exec::run::record_changes(canvas_id, node, generation.clone()).is_ok() {
                    round.push(Hit::Copy { node: *id, generation });
                }
            }
            if found.len().saturating_add(round.len()) >= MAX_HITS {
                break;
            }
        }
        for id in done {
            wanted.remove(&id);
        }
        if round.is_empty() {
            break;
        }
        Changeset::new(changes(canvas_id, canvas, &round)?).apply(&mut scratch)?;
        for hit in &round {
            wanted.remove(&hit.node());
        }
        found.extend(round);
    }
    found.truncate(MAX_HITS);
    Ok(found)
}

/// The changes applying `hits` on `canvas`.
pub fn changes(canvas_id: CanvasId, canvas: &Canvas, hits: &[Hit]) -> Result<Vec<Change>, Error> {
    let mut changes = Vec::new();
    for hit in hits {
        match hit {
            Hit::Own { node, generation } => changes.push(Change::SetActiveGeneration { canvas: canvas_id, id: *node, generation: Some(*generation) }),
            Hit::Copy { node, generation } => {
                let current = canvas.nodes().get(node).ok_or_else(|| Error::new(nodecraft_types::ErrorKind::NotFound, "a cached result's node is gone"))?;
                changes.extend(nodecraft_exec::run::record_changes(canvas_id, current, generation.clone())?);
            }
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests;
