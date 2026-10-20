//! `#name` mentions of Elements (#104). A node whose `prompt` parameter mentions an Element on
//! the same canvas is fed that Element's `reference` output on its first image list input, after
//! its wired feeds, as if it were wired there. The planner and staleness both read inputs through
//! [`crate::routing::incoming`], which adds these feeds, so a mention runs, costs and goes stale
//! like a wire. The prompt itself is sent unchanged.
//!
//! A `#token` that names no Element is plain text (`#ff0000`, `#1`), never an error. A name two
//! Elements share is ambiguous: the mentioning node is blocked until one is renamed.
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::element;
use nodecraft_types::{Modality, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The parameter mentions are read from.
pub const PROMPT: &str = "prompt";

/// One mention, as the UI lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// The name as the prompt writes it.
    pub name: String,
    pub resolved: Resolved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// It feeds this Element's references.
    Element(NodeId),
    /// No Element has this name: the text stays as written.
    Unknown,
    /// Several Elements have this name.
    Ambiguous,
    /// An Element has this name but the node takes no list of images.
    NoImageInput,
    /// The node already has this Element wired in.
    Wired(NodeId),
}

/// Implicit feeds: per consumer, (target port, Element, its `reference` port), in mention order.
#[derive(Debug, Default)]
pub struct Feeds<'a> {
    pub feeds: BTreeMap<NodeId, Vec<(&'a str, NodeId, &'a str)>>,
    /// Consumers mentioning a name several Elements share.
    pub ambiguous: BTreeSet<NodeId>,
}

/// The canvas's Elements by lowercased name.
fn by_name(canvas: &Canvas) -> BTreeMap<String, Vec<&Node>> {
    let mut names: BTreeMap<String, Vec<&Node>> = BTreeMap::new();
    for node in canvas.nodes().values() {
        if let Some(name) = element::name(node) {
            names.entry(name.to_ascii_lowercase()).or_default().push(node);
        }
    }
    names
}

fn prompt(node: &Node) -> Option<&str> {
    match node.params.get(PROMPT) {
        Some(Value::Text(text)) if node.kind != element::ID => Some(text),
        _ => None,
    }
}

/// Where a mention feeds `node`: its first image list input.
fn target(node: &Node) -> Option<&str> {
    node.inputs.iter().find(|port| port.port_type.modality == Modality::Image && port.port_type.many).map(|port| port.name.as_str())
}

fn reference(node: &Node) -> Option<&str> {
    node.outputs.iter().find(|port| port.name == element::REFERENCE).map(|port| port.name.as_str())
}

/// Whether `consumer` already has `from`'s references wired in.
fn wired(canvas: &Canvas, consumer: NodeId, from: NodeId) -> bool {
    canvas.edges().values().any(|edge| edge.to.node == consumer && edge.from.node == from && edge.from.port == element::REFERENCE)
}

fn resolve_one(canvas: &Canvas, names: &BTreeMap<String, Vec<&Node>>, node: &Node, name: &str) -> Resolved {
    match names.get(&name.to_ascii_lowercase()).map(Vec::as_slice) {
        None | Some([]) => Resolved::Unknown,
        Some([found]) if reference(found).is_none() => Resolved::Unknown,
        Some([found]) if wired(canvas, node.id, found.id) => Resolved::Wired(found.id),
        Some([_]) if target(node).is_none() => Resolved::NoImageInput,
        Some([found]) => Resolved::Element(found.id),
        Some(_) => Resolved::Ambiguous,
    }
}

/// What `node`'s prompt mentions, in order, and what each resolves to.
pub fn of(canvas: &Canvas, node: &Node) -> Vec<Mention> {
    let Some(text) = prompt(node) else { return Vec::new() };
    let names = by_name(canvas);
    element::mentions(text).into_iter().map(|name| Mention { name: name.to_owned(), resolved: resolve_one(canvas, &names, node, name) }).collect()
}

/// Every mention feed on `canvas`.
pub fn feeds(canvas: &Canvas) -> Feeds<'_> {
    let names = by_name(canvas);
    let mut out = Feeds::default();
    if names.is_empty() {
        return out;
    }
    for node in canvas.nodes().values() {
        let Some(text) = prompt(node) else { continue };
        for name in element::mentions(text) {
            match resolve_one(canvas, &names, node, name) {
                Resolved::Element(id) => {
                    let (Some(port), Some(from)) = (target(node), canvas.nodes().get(&id).and_then(reference)) else { continue };
                    out.feeds.entry(node.id).or_default().push((port, id, from));
                }
                Resolved::Ambiguous => {
                    out.ambiguous.insert(node.id);
                }
                Resolved::Unknown | Resolved::NoImageInput | Resolved::Wired(_) => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
