//! Elements (#104) from the canvas: "Make Element from selection" in a card's context menu.
use super::NodeCraftApp;
use crate::ids;
use nodecraft_graph::{Canvas, Endpoint};
use nodecraft_nodes::element;
use nodecraft_types::{Modality, NodeId};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// How far right of the selection the new Element lands, in world units.
const GAP: f32 = 420.0;

/// The image outputs of the selected nodes that aren't Elements, in reading order (top to
/// bottom, then left to right), at most [`element::MAX_IMAGES`].
pub(super) fn sources(canvas: &Canvas, selected: &BTreeSet<NodeId>) -> Vec<Endpoint> {
    let mut found: Vec<(f32, f32, Endpoint)> = selected
        .iter()
        .filter_map(|id| canvas.nodes().get(id))
        .filter(|node| node.kind != element::ID)
        .filter_map(|node| {
            let port = node.outputs.iter().find(|port| port.port_type.modality == Modality::Image)?;
            Some((node.pos.y, node.pos.x, Endpoint { node: node.id, port: port.name.clone() }))
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.node.cmp(&b.2.node)));
    found.into_iter().take(element::MAX_IMAGES).map(|(_, _, endpoint)| endpoint).collect()
}

/// How many Elements on `canvas` go by each lowercased name; empty when there are none, so cards
/// skip the mention hint cheaply.
pub(super) fn names(canvas: &Canvas) -> BTreeMap<String, usize> {
    let mut names = BTreeMap::new();
    for name in canvas.nodes().values().filter_map(element::name) {
        let count: &mut usize = names.entry(name.to_ascii_lowercase()).or_default();
        *count = count.saturating_add(1);
    }
    names
}

/// `element`, or `element-2`, `element-3`… whichever no Element on `canvas` goes by.
fn free_name(canvas: &Canvas) -> Option<String> {
    let taken: BTreeSet<String> = canvas.nodes().values().filter_map(element::name).map(str::to_ascii_lowercase).collect();
    (1..=1000u32).map(|n| if n == 1 { "element".to_owned() } else { format!("element-{n}") }).find(|name| !taken.contains(name))
}

impl NodeCraftApp {
    /// Makes an Element from the selected images, wired in reading order, to the right of them,
    /// and selects it.
    pub(super) fn make_element(&mut self) {
        let document = self.session.document();
        let Some(canvas) = document.canvases().get(&self.canvas) else { return };
        let from = sources(canvas, self.selection.nodes());
        let picked: Vec<_> = from.iter().filter_map(|endpoint| canvas.nodes().get(&endpoint.node)).collect();
        let (Some(right), Some(top)) = (picked.iter().map(|n| n.pos.x).reduce(f32::max), picked.iter().map(|n| n.pos.y).reduce(f32::min)) else {
            self.notice = Some("Select image cards to make an Element from.".into());
            return;
        };
        let Some(name) = free_name(canvas) else {
            self.notice = Some("Too many Elements on this canvas; rename some first.".into());
            return;
        };
        let revision = self.session.revision();
        let id = ids::node_id(document, ("element", revision));
        let edges: Option<Vec<_>> = (0..from.len()).map(|n| ids::edge_id(document, ("element-wire", revision, n))).collect();
        let (Some(id), Some(edges)) = (id, edges) else {
            self.notice = Some("Couldn't allocate node ids; try again.".into());
            return;
        };
        let params = json!({
            "canvas": self.canvas,
            "id": id,
            "name": name,
            "from": from,
            "edges": edges,
            "pos": {"x": right + GAP, "y": top},
        });
        match self.session.dispatch("element.create", &params) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}
