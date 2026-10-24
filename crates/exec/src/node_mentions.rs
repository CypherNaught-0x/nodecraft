//! `@node` mentions (#505): a generation card whose typed prompt mentions another card
//! (`@[Title](id)`, `nodecraft_nodes::mention`) is fed that card's text output on
//! [`mention::PORT`], and the kind puts the text where the token is. The planner and staleness
//! read inputs through [`crate::routing::incoming`], which adds these feeds, so a mention runs,
//! orders and goes stale like a wire.
//!
//! A mention of a card that is gone, makes no single text (or several, as a batch does), or is
//! made from the mentioning card (a loop), and a prompt with more mentions than are fed, block the
//! mentioning card before anything is paid for. A wired prompt replaces the
//! typed one, so its mentions feed nothing, as for `#element` mentions.
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{batch, generation, mention, router};
use nodecraft_types::{Modality, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Why a mention feeds nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No card on this canvas has this id: it was deleted, or the prompt came from elsewhere.
    Missing,
    /// The card makes no single text (an image card, or a list of texts).
    NotText,
    /// The mentioned card is the mentioning card or is made from it.
    Loop,
    /// The card makes several texts: a batch, a step run once per item, or stored items.
    Several,
    /// The prompt mentions more cards, or holds more mentions, than are fed.
    TooMany,
}

/// One mention as the UI lists it: the title as written, the card, and the text port it reads
/// or why it can't.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention<'a> {
    pub title: &'a str,
    pub id: NodeId,
    pub resolved: Result<&'a str, Problem>,
}

/// The prompt mentions are read from: a generation card's typed prompt, unless a wire feeds it.
pub fn prompt<'n>(canvas: &Canvas, node: &'n Node) -> Option<&'n str> {
    typed(node).filter(|_| !canvas.edges().values().any(|edge| edge.to.node == node.id && edge.to.port == generation::PROMPT))
}

/// A generation card's typed prompt holding a mention, wired or not.
fn typed(node: &Node) -> Option<&str> {
    if !generation::is_generation(&node.kind) {
        return None;
    }
    let Some(Value::Text(text)) = node.params.get(generation::PROMPT) else { return None };
    text.contains("@[").then_some(text.as_str())
}

/// The output a mention of `node` reads: its first single text output. A router passes on
/// whatever reaches it and a batch or a folder gives several values, so none is read.
pub fn text_output(node: &Node) -> Option<&str> {
    if node.kind == router::ID || node.kind == batch::ID || node.kind == nodecraft_nodes::folder::ID {
        return None;
    }
    node.outputs.iter().find(|port| port.port_type.modality == Modality::Text && !port.port_type.many).map(|port| port.name.as_str())
}

/// What `id` resolves to as a mention by `consumer`, loops through other cards aside.
fn resolve(canvas: &Canvas, consumer: NodeId, id: NodeId) -> Result<&str, Problem> {
    if id == consumer {
        return Err(Problem::Loop);
    }
    let node = canvas.nodes().get(&id).ok_or(Problem::Missing)?;
    text_output(node).ok_or(Problem::NotText)
}

/// What `node`'s prompt mentions, in order of first mention. `loops` is
/// [`crate::routing::Incoming::mention_loops`].
pub fn of<'a>(canvas: &'a Canvas, node: &'a Node, loops: &BTreeSet<(NodeId, NodeId)>) -> Vec<Mention<'a>> {
    prompt(canvas, node).map_or_else(Vec::new, |text| mentions(canvas, node, text, loops))
}

fn mentions<'a>(canvas: &'a Canvas, node: &'a Node, text: &'a str, loops: &BTreeSet<(NodeId, NodeId)>) -> Vec<Mention<'a>> {
    let tokens = mention::tokens(text);
    mention::ids(text)
        .into_iter()
        .map(|id| {
            let title = tokens.iter().find(|token| token.id == id).map_or("", |token| token.title);
            let resolved = if loops.contains(&(node.id, id)) { Err(Problem::Loop) } else { resolve(canvas, node.id, id) };
            Mention { title, id, resolved }
        })
        .collect()
}

/// The title `node`'s prompt gives its mention of `id`, as written.
pub(crate) fn title_of(node: &Node, id: NodeId) -> &str {
    typed(node).and_then(|text| mention::tokens(text).into_iter().find(|token| token.id == id)).map_or("", |token| token.title)
}

/// Every consumer's mention feeds, (mentioned card, its text port) in prompt order, before the
/// loop check in [`crate::routing::incoming`]; and the mentions that can't feed.
#[derive(Debug, Default)]
pub(crate) struct Feeds<'a> {
    pub feeds: BTreeMap<NodeId, Vec<(NodeId, &'a str)>>,
    /// Per consumer, its first mention that can't feed: the title as written and why.
    pub problems: BTreeMap<NodeId, (&'a str, Problem)>,
}

pub(crate) fn feeds(canvas: &Canvas) -> Feeds<'_> {
    let mut out = Feeds::default();
    let wired: BTreeSet<NodeId> = canvas.edges().values().filter(|edge| edge.to.port == generation::PROMPT).map(|edge| edge.to.node).collect();
    for node in canvas.nodes().values() {
        let Some(text) = typed(node).filter(|_| !wired.contains(&node.id)) else { continue };
        if mention::too_many(text) {
            out.problems.insert(node.id, ("", Problem::TooMany));
        }
        for found in mentions(canvas, node, text, &BTreeSet::new()) {
            match found.resolved {
                Ok(port) => out.feeds.entry(node.id).or_default().push((found.id, port)),
                Err(problem) => {
                    out.problems.entry(node.id).or_insert((found.title, problem));
                }
            }
        }
    }
    out
}

/// Why `title`'s mention blocks a run, for the planner and the card.
pub fn reason(title: &str, problem: Problem) -> String {
    match problem {
        Problem::Missing => format!("its prompt mentions @{title}, which is no longer on this canvas; remove the mention or mention another card"),
        Problem::NotText => format!("its prompt mentions @{title}, which makes no single text; mention a text card"),
        Problem::Loop => format!("its prompt mentions @{title}, which is made from this card; that would be a loop"),
        Problem::Several => format!("its prompt mentions @{title}, which makes several texts; mention a card with one text"),
        Problem::TooMany => {
            format!("its prompt mentions more than {} cards or holds more than {} mentions; mention fewer", mention::MAX_MENTIONS, mention::MAX_TOKENS)
        }
    }
}

/// Each node of `next`'s strongly connected component: two nodes share one exactly when each is
/// downstream of the other. Tarjan's algorithm, iteratively, so a long chain can't overflow the
/// stack; each node and edge is visited once.
pub(crate) fn components(next: &BTreeMap<NodeId, BTreeSet<NodeId>>) -> BTreeMap<NodeId, usize> {
    struct Visit {
        index: usize,
        low: usize,
        on_stack: bool,
    }
    let mut visits: BTreeMap<NodeId, Visit> = BTreeMap::new();
    let mut component: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut stack: Vec<NodeId> = Vec::new();
    let mut count = 0usize;
    let mut labels = 0usize;
    let empty = BTreeSet::new();
    for root in next.keys() {
        if visits.contains_key(root) {
            continue;
        }
        // (node, its successors still to look at)
        let mut path = vec![(*root, next.get(root).unwrap_or(&empty).iter())];
        visits.insert(*root, Visit { index: count, low: count, on_stack: true });
        stack.push(*root);
        count = count.saturating_add(1);
        while let Some((id, successors)) = path.last_mut() {
            let id = *id;
            if let Some(after) = successors.next() {
                match visits.get(after) {
                    None => {
                        visits.insert(*after, Visit { index: count, low: count, on_stack: true });
                        stack.push(*after);
                        count = count.saturating_add(1);
                        path.push((*after, next.get(after).unwrap_or(&empty).iter()));
                    }
                    Some(seen) if seen.on_stack => {
                        let index = seen.index;
                        if let Some(visit) = visits.get_mut(&id) {
                            visit.low = visit.low.min(index);
                        }
                    }
                    Some(_) => {}
                }
                continue;
            }
            path.pop();
            let (index, low) = visits.get(&id).map_or((0, 0), |visit| (visit.index, visit.low));
            if let Some((parent, _)) = path.last()
                && let Some(visit) = visits.get_mut(parent)
            {
                visit.low = visit.low.min(low);
            }
            if low == index {
                let label = labels;
                labels = labels.saturating_add(1);
                while let Some(member) = stack.pop() {
                    if let Some(visit) = visits.get_mut(&member) {
                        visit.on_stack = false;
                    }
                    component.insert(member, label);
                    if member == id {
                        break;
                    }
                }
            }
        }
    }
    component
}

#[cfg(test)]
mod tests;
