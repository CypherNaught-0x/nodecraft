//! The cards a generation prompt can `@mention` (#505), for the composer's picker and hint.
use crate::card::{self, mention::Candidate};
use nodecraft_graph::Canvas;
use nodecraft_nodes::Registry;
use nodecraft_types::{CanvasId, NodeId};
use std::collections::BTreeMap;

/// The open canvas's [`candidates`] and why each blocked card's mentions can't feed, rebuilt only
/// when the document or the canvas changes rather than every frame.
#[derive(Default)]
pub(super) struct Cache {
    key: Option<(u64, CanvasId)>,
    pub cards: Vec<Candidate>,
    pub blocked: BTreeMap<NodeId, String>,
}

impl Cache {
    pub(super) fn refresh(&mut self, revision: u64, id: CanvasId, canvas: &Canvas, kinds: &Registry) {
        if self.key == Some((revision, id)) {
            return;
        }
        self.key = Some((revision, id));
        self.cards = candidates(canvas, kinds);
        // A routing error is the planner's to report; the hint then shows only what it reads.
        self.blocked = if self.cards.is_empty() {
            BTreeMap::new()
        } else {
            nodecraft_exec::routing::incoming(canvas, kinds).map(|incoming| incoming.mention_problems).unwrap_or_default()
        };
    }
}

/// Every card on `canvas` by title, text cards first in reading order (top to bottom, then left
/// to right); none when no card has a prompt to mention them from.
pub(super) fn candidates(canvas: &Canvas, kinds: &Registry) -> Vec<Candidate> {
    if !canvas.nodes().values().any(|node| nodecraft_nodes::generation::is_generation(&node.kind)) {
        return Vec::new();
    }
    let mut found: Vec<(bool, f32, f32, Candidate)> = canvas
        .nodes()
        .values()
        .map(|node| {
            let descriptor = kinds.get(&node.kind).ok().map(|kind| kind.descriptor());
            let text = nodecraft_exec::node_mentions::text_output(node).is_some();
            (!text, node.pos.y, node.pos.x, Candidate { id: node.id, title: card::title(node, descriptor), text })
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.total_cmp(&b.2)).then(a.3.id.cmp(&b.3.id)));
    found.into_iter().map(|(.., candidate)| candidate).collect()
}
