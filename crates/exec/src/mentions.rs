//! `#name` mentions of Elements (#104). A generation node whose `prompt` parameter mentions an
//! Element on the same canvas is fed that Element's `reference` output on its image list input,
//! after its wired feeds, as if it were wired there. The planner and staleness both read inputs
//! through [`crate::routing::incoming`], which adds these feeds, so a mention runs, costs and goes
//! stale like a wire. The prompt itself is sent unchanged.
//!
//! Only generation kinds are fed, on the first image list input that takes more than one image
//! (not a single init image), and only while the prompt is typed in: a wired prompt replaces the
//! parameter, so its text mentions nothing.
//!
//! A `#token` that names no Element is plain text (`#ff0000`, `#1`), never an error. A name two
//! Elements share is ambiguous: the mentioning node is blocked until one is renamed.
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, Registry, element};
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
    /// An Element has this name but the node takes no list of reference images.
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

/// What resolving mentions on one canvas needs, built once.
pub struct Index<'a, 'k> {
    canvas: &'a Canvas,
    kinds: &'k Registry,
    /// Elements by lowercased name.
    names: BTreeMap<String, Vec<&'a Node>>,
    /// (consumer, Element) pairs wired directly from the Element's references.
    wired: BTreeSet<(NodeId, NodeId)>,
    /// Nodes whose prompt port is wired.
    wired_prompt: BTreeSet<NodeId>,
}

impl<'a, 'k> Index<'a, 'k> {
    pub fn new(canvas: &'a Canvas, kinds: &'k Registry) -> Self {
        let mut names: BTreeMap<String, Vec<&Node>> = BTreeMap::new();
        for node in canvas.nodes().values() {
            if let Some(name) = element::name(node) {
                names.entry(name.to_ascii_lowercase()).or_default().push(node);
            }
        }
        let mut wired = BTreeSet::new();
        let mut wired_prompt = BTreeSet::new();
        if !names.is_empty() {
            for edge in canvas.edges().values() {
                if edge.from.port == element::REFERENCE {
                    wired.insert((edge.to.node, edge.from.node));
                }
                if edge.to.port == PROMPT {
                    wired_prompt.insert(edge.to.node);
                }
            }
        }
        Self { canvas, kinds, names, wired, wired_prompt }
    }

    /// The typed-in prompt of a generation node, which is where mentions are read.
    fn prompt<'n>(&self, node: &'n Node) -> Option<&'n str> {
        if self.wired_prompt.contains(&node.id) {
            return None;
        }
        let generation = self.kinds.get(&node.kind).is_ok_and(|kind| kind.descriptor().category == Category::Generation);
        match node.params.get(PROMPT) {
            Some(Value::Text(text)) if generation => Some(text),
            _ => None,
        }
    }

    /// Where a mention feeds `node`: its first image list input that takes more than one image.
    fn target<'n>(&self, node: &'n Node) -> Option<&'n str> {
        let contract = self.kinds.get(&node.kind).ok()?.instance_contract(node).ok()?;
        node.inputs
            .iter()
            .filter(|port| port.port_type.modality == Modality::Image && port.port_type.many)
            .find(|port| contract.inputs.iter().find(|input| input.port == port.name).is_none_or(|input| input.max.is_none_or(|max| max > 1)))
            .map(|port| port.name.as_str())
    }

    fn resolve_one(&self, node: &Node, name: &str) -> Resolved {
        match self.names.get(&name.to_ascii_lowercase()).map(Vec::as_slice) {
            None | Some([]) => Resolved::Unknown,
            Some([found]) if reference(found).is_none() => Resolved::Unknown,
            Some([found]) if self.wired.contains(&(node.id, found.id)) => Resolved::Wired(found.id),
            Some([_]) if self.target(node).is_none() => Resolved::NoImageInput,
            Some([found]) => Resolved::Element(found.id),
            Some(_) => Resolved::Ambiguous,
        }
    }

    /// What `node`'s prompt mentions, in order, and what each resolves to. Empty for a node
    /// mentions don't apply to (not a generation, or its prompt is wired).
    pub fn of(&self, node: &Node) -> Vec<Mention> {
        let Some(text) = self.prompt(node) else { return Vec::new() };
        element::mentions(text).into_iter().map(|name| Mention { name: name.to_owned(), resolved: self.resolve_one(node, name) }).collect()
    }

    /// Every mention feed on the canvas.
    pub fn feeds(&self) -> Feeds<'a> {
        let mut out = Feeds::default();
        if self.names.is_empty() {
            return out;
        }
        let canvas = self.canvas;
        for node in canvas.nodes().values() {
            let Some(text) = self.prompt(node) else { continue };
            for name in element::mentions(text) {
                match self.resolve_one(node, name) {
                    Resolved::Element(id) => {
                        let (Some(port), Some(from)) = (self.target(node), canvas.nodes().get(&id).and_then(reference)) else { continue };
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
}

fn reference(node: &Node) -> Option<&str> {
    node.outputs.iter().find(|port| port.name == element::REFERENCE).map(|port| port.name.as_str())
}

/// What `node`'s prompt mentions; see [`Index::of`]. Build an [`Index`] to resolve many nodes.
pub fn of(canvas: &Canvas, kinds: &Registry, node: &Node) -> Vec<Mention> {
    Index::new(canvas, kinds).of(node)
}

/// Every mention feed on `canvas`.
pub fn feeds<'a>(canvas: &'a Canvas, kinds: &Registry) -> Feeds<'a> {
    Index::new(canvas, kinds).feeds()
}

#[cfg(test)]
mod tests;
