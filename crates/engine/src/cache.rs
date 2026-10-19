//! Opt-in result caching (#91): a node about to run whose inputs (kind, settings, input values;
//! the input hash, #75) match a result already made reuses that result instead of computing or
//! paying again.
//!
//! - **Where results come from.** The node's own history first (a setting changed back selects
//!   the result it had then); otherwise another node of the same kind on the canvas whose result
//!   has the same input hash (copied as a new generation). Nothing outside the project is looked at.
//! - **Who reuses.** A node's `config["cache"]` decides; without it, local kinds (actions)
//!   reuse and kinds that call a model don't, because a model with the same inputs may be wanted
//!   again for a different take. The switch is left out of the input hash, so turning it on or off
//!   never makes a result stale.
//! - **Results only.** A hit changes the document like any result does (one undo step, or the
//!   edit's own step when live recompute applies it), and costs nothing.
use nodecraft_exec::staleness::{CACHE_CONFIG, input_hash};
use nodecraft_graph::{Canvas, Change, Generation, Node};
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::{Blake3, CanvasId, Error, GenerationId, NodeId, Value};

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

/// Most results looked through on the canvas for one node.
const MAX_SCANNED: usize = 100_000;

/// The hits for `nodes` (those with caching on whose active result isn't already current).
/// `fresh_id` names copied results.
pub fn hits(
    canvas: &Canvas,
    kinds: &Registry,
    nodes: impl IntoIterator<Item = NodeId>,
    mut fresh_id: impl FnMut() -> Result<GenerationId, Error>,
) -> Result<Vec<Hit>, Error> {
    let mut found = Vec::new();
    for id in nodes {
        let Some(node) = canvas.nodes().get(&id) else { continue };
        if !enabled(node, kinds) {
            continue;
        }
        let Ok(current) = input_hash(canvas, kinds, id) else { continue };
        let active = node.active.and_then(|a| node.history.iter().find(|g| g.id == a));
        if active.is_some_and(|g| g.input_hash == Some(current)) {
            continue;
        }
        if let Some(own) = node.history.iter().find(|g| g.input_hash == Some(current)) {
            found.push(Hit::Own { node: id, generation: own.id });
            continue;
        }
        if let Some(other) = elsewhere(canvas, node, current) {
            found.push(Hit::Copy { node: id, generation: Generation { id: fresh_id()?, outputs: other.outputs.clone(), input_hash: Some(current) } });
        }
    }
    Ok(found)
}

/// A result of another node of `node`'s kind with input hash `current`.
fn elsewhere<'a>(canvas: &'a Canvas, node: &Node, current: Blake3) -> Option<&'a Generation> {
    canvas
        .nodes()
        .values()
        .filter(|other| other.id != node.id && other.kind == node.kind)
        .flat_map(|other| other.history.iter())
        .take(MAX_SCANNED)
        .find(|g| g.input_hash == Some(current))
}

/// The changes applying `hits` on `canvas`.
pub fn changes(canvas_id: CanvasId, canvas: &Canvas, hits: &[Hit]) -> Result<Vec<Change>, Error> {
    let mut changes = Vec::new();
    for hit in hits {
        match hit {
            Hit::Own { node, generation } => changes.push(Change::SetActiveGeneration { canvas: canvas_id, id: *node, generation: Some(*generation) }),
            Hit::Copy { node, generation } => {
                let Some(current) = canvas.nodes().get(node) else { continue };
                changes.extend(nodecraft_exec::run::record_changes(canvas_id, current, generation.clone())?);
            }
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests;
