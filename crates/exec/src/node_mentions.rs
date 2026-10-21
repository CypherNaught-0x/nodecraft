//! `@node` mentions (#505): a generation card whose typed prompt mentions another card
//! (`@[Title](id)`, `nodecraft_nodes::mention`) is fed that card's text output on
//! [`mention::PORT`], and the kind puts the text where the token is. The planner and staleness
//! read inputs through [`crate::routing::incoming`], which adds these feeds, so a mention runs,
//! orders and goes stale like a wire.
//!
//! A mention of a card that is gone, makes no single text, or is made from the mentioning card
//! (a loop) blocks the mentioning card before anything is paid for. A wired prompt replaces the
//! typed one, so its mentions feed nothing, as for `#element` mentions.
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{generation, mention};
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
    if !generation::is_generation(&node.kind) {
        return None;
    }
    let Some(Value::Text(text)) = node.params.get(generation::PROMPT) else { return None };
    if !text.contains("@[") || canvas.edges().values().any(|edge| edge.to.node == node.id && edge.to.port == generation::PROMPT) {
        return None;
    }
    Some(text)
}

/// The output a mention of `node` reads: its first single text output.
pub fn text_output(node: &Node) -> Option<&str> {
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
    let Some(text) = prompt(canvas, node) else { return Vec::new() };
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
    for node in canvas.nodes().values() {
        for found in of(canvas, node, &BTreeSet::new()) {
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
    }
}

/// Whether `to` is downstream of `from` through `next`, counting each visit against `budget`.
/// `None` when the budget runs out.
pub(crate) fn reaches(next: &BTreeMap<NodeId, BTreeSet<NodeId>>, from: NodeId, to: NodeId, budget: &mut usize) -> Option<bool> {
    let mut seen = BTreeSet::from([from]);
    let mut stack = vec![from];
    while let Some(id) = stack.pop() {
        for after in next.get(&id).into_iter().flatten() {
            if *after == to {
                return Some(true);
            }
            if seen.insert(*after) {
                *budget = budget.checked_sub(1)?;
                stack.push(*after);
            }
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests;
