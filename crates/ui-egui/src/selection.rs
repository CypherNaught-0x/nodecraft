//! Canvas selection: shell view state, never written to the document or the undo history.
//!
//! The chain queries walk the canvas edges with a seen-set, so they terminate on any graph,
//! including a cyclic one loaded from a hostile file.
use egui::Rect;
use nodecraft_graph::{Canvas, Node};
use nodecraft_types::{EdgeId, NodeId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    nodes: BTreeSet<NodeId>,
    edges: BTreeSet<EdgeId>,
}

impl Selection {
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains(&id)
    }
    pub fn contains_edge(&self, id: EdgeId) -> bool {
        self.edges.contains(&id)
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }
    /// Selected nodes and edges together.
    pub fn len(&self) -> usize {
        self.nodes.len().saturating_add(self.edges.len())
    }
    pub fn nodes(&self) -> &BTreeSet<NodeId> {
        &self.nodes
    }
    pub fn edges(&self) -> &BTreeSet<EdgeId> {
        &self.edges
    }
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.edges.clear();
    }
    /// Plain click: this node and nothing else.
    pub fn only(&mut self, id: NodeId) {
        self.clear();
        self.nodes.insert(id);
    }
    /// Plain click on a noodle: this edge and nothing else.
    pub fn only_edge(&mut self, id: EdgeId) {
        self.clear();
        self.edges.insert(id);
    }
    pub fn toggle_edge(&mut self, id: EdgeId) {
        if !self.edges.remove(&id) {
            self.edges.insert(id);
        }
    }
    /// Shift-click: add the node, or remove it if it was already selected.
    pub fn toggle(&mut self, id: NodeId) {
        if !self.nodes.remove(&id) {
            self.nodes.insert(id);
        }
    }
    pub fn add(&mut self, id: NodeId) {
        self.nodes.insert(id);
    }
    /// Select exactly these nodes (chain and kind queries select nodes only).
    pub fn replace(&mut self, nodes: BTreeSet<NodeId>) {
        self.nodes = nodes;
        self.edges.clear();
    }
    /// Add to the selection (box select adds what it touches).
    pub fn extend(&mut self, nodes: impl IntoIterator<Item = NodeId>, edges: impl IntoIterator<Item = EdgeId>) {
        self.nodes.extend(nodes);
        self.edges.extend(edges);
    }
    /// Drop ids that no longer exist (after undo, a remove, or a canvas switch).
    pub fn retain_existing(&mut self, canvas: &Canvas) {
        self.nodes.retain(|id| canvas.nodes().contains_key(id));
        self.edges.retain(|id| canvas.edges().contains_key(id));
    }
}

/// `seeds` and every node they depend on, transitively.
pub fn upstream(canvas: &Canvas, seeds: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
    let mut links: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        links.entry(edge.to.node).or_default().push(edge.from.node);
    }
    reach(canvas, seeds, &links)
}

/// `seeds` and every node that depends on them, transitively.
pub fn downstream(canvas: &Canvas, seeds: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
    let mut links: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        links.entry(edge.from.node).or_default().push(edge.to.node);
    }
    reach(canvas, seeds, &links)
}

fn reach(canvas: &Canvas, seeds: &BTreeSet<NodeId>, links: &BTreeMap<NodeId, Vec<NodeId>>) -> BTreeSet<NodeId> {
    let mut seen: BTreeSet<NodeId> = seeds.iter().copied().filter(|id| canvas.nodes().contains_key(id)).collect();
    let mut stack: Vec<NodeId> = seen.iter().copied().collect();
    while let Some(id) = stack.pop() {
        for next in links.get(&id).into_iter().flatten() {
            if canvas.nodes().contains_key(next) && seen.insert(*next) {
                stack.push(*next);
            }
        }
    }
    seen
}

/// Every node of one kind on the canvas.
pub fn of_kind(canvas: &Canvas, kind: &str) -> BTreeSet<NodeId> {
    canvas.nodes().values().filter(|node| node.kind == kind).map(|node| node.id).collect()
}

/// Nodes whose card touches the world-space box (`rect_of` gives a card's world rect).
pub fn in_box(canvas: &Canvas, world: Rect, mut rect_of: impl FnMut(&Node) -> Rect) -> BTreeSet<NodeId> {
    canvas.nodes().values().filter(|node| rect_of(node).intersects(world)).map(|node| node.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Vec2};
    use nodecraft_graph::{Change, Edge, Endpoint, Port, Position, Project};
    use nodecraft_types::{CanvasId, EdgeId, Modality, PortType, ProjectId};

    const C: CanvasId = CanvasId::from_bytes([1; 16]);

    fn id(n: u8) -> NodeId {
        NodeId::from_bytes([n; 16])
    }
    fn set(ids: &[u8]) -> BTreeSet<NodeId> {
        ids.iter().map(|n| id(*n)).collect()
    }
    fn port(name: &str) -> Port {
        Port { name: name.into(), port_type: PortType { modality: Modality::Text, many: true } }
    }
    /// 1 → 2 → 3, 1 → 4, and an unconnected 5 of another kind.
    fn diamond() -> Project {
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Chain").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        for n in 1..=5 {
            let mut node = Node::new(id(n), if n == 5 { "test.other" } else { "test.chain" });
            node.pos = Position { x: f32::from(n) * 400.0, y: 0.0 };
            node.inputs = vec![port("in")];
            node.outputs = vec![port("out")];
            Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        }
        for (e, (from, to)) in [(1, 2), (2, 3), (1, 4)].into_iter().enumerate() {
            let edge = Edge {
                id: EdgeId::from_bytes([10 + e as u8; 16]),
                from: Endpoint { node: id(from), port: "out".into() },
                to: Endpoint { node: id(to), port: "in".into() },
                order: e as u32,
            };
            Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
        }
        project
    }

    #[test]
    fn chains_follow_edges_and_include_the_seeds() {
        let project = diamond();
        let canvas = project.canvases().get(&C).unwrap();
        assert_eq!(upstream(canvas, &set(&[3])), set(&[1, 2, 3]));
        assert_eq!(downstream(canvas, &set(&[1])), set(&[1, 2, 3, 4]));
        assert_eq!(downstream(canvas, &set(&[2])), set(&[2, 3]));
        assert_eq!(upstream(canvas, &set(&[5])), set(&[5]));
        assert_eq!(upstream(canvas, &set(&[3, 4])), set(&[1, 2, 3, 4]));
        // Ids that are not on the canvas are ignored rather than echoed back.
        assert_eq!(downstream(canvas, &set(&[99])), set(&[]));
    }

    #[test]
    fn kind_and_box_queries() {
        let project = diamond();
        let canvas = project.canvases().get(&C).unwrap();
        assert_eq!(of_kind(canvas, "test.chain"), set(&[1, 2, 3, 4]));
        assert_eq!(of_kind(canvas, "missing"), set(&[]));
        let rect_of = |node: &Node| Rect::from_min_size(Pos2::new(node.pos.x, node.pos.y), Vec2::new(300.0, 200.0));
        // Touching a card is enough; the gap between 2 and 3 selects nothing.
        assert_eq!(in_box(canvas, Rect::from_min_max(Pos2::new(750.0, 10.0), Pos2::new(1250.0, 20.0)), rect_of), set(&[2, 3]));
        assert_eq!(in_box(canvas, Rect::from_min_max(Pos2::new(1110.0, 10.0), Pos2::new(1190.0, 20.0)), rect_of), set(&[]));
    }

    #[test]
    fn toggle_only_and_retain() {
        let project = diamond();
        let canvas = project.canvases().get(&C).unwrap();
        let mut selection = Selection::default();
        selection.only(id(1));
        selection.toggle(id(2));
        assert_eq!(selection.nodes(), &set(&[1, 2]));
        selection.toggle(id(1));
        assert_eq!(selection.nodes(), &set(&[2]));
        selection.replace(set(&[2, 42]));
        selection.retain_existing(canvas);
        assert_eq!(selection.nodes(), &set(&[2]));
        selection.clear();
        assert!(selection.is_empty());
    }
}
