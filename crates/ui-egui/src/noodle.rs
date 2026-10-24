//! Noodles: the bezier connections between an output port and an input port.
//!
//! Pure geometry and graph lookups, shared by painting and hit testing. All positions are world
//! coordinates unless a name says otherwise.
use egui::{Pos2, Rect, Vec2};
use nodecraft_graph::{Canvas, Endpoint, Node, Project};
use nodecraft_types::{EdgeId, PortType};

/// Line segments per noodle for hit testing and box selection.
pub const SAMPLES: usize = 24;
/// Smallest horizontal pull of a control point, so short or backward noodles still curve.
pub const MIN_PULL: f32 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Input,
    Output,
}

/// Where port `index` of `count` sits on a card edge: inputs on the left, outputs on the right,
/// spread evenly; a single port sits at the middle of the edge.
pub fn anchor(card: Rect, side: Side, index: usize, count: usize) -> Pos2 {
    let x = match side {
        Side::Input => card.left(),
        Side::Output => card.right(),
    };
    let slots = count.max(1).saturating_add(1) as f32;
    let slot = (index.min(count.saturating_sub(1)) + 1) as f32;
    Pos2::new(x, card.top() + card.height() * slot / slots)
}

/// The anchor of a named port on `node`, given its card rect.
pub fn port_anchor(node: &Node, card: Rect, side: Side, port: &str) -> Option<Pos2> {
    let ports = match side {
        Side::Input => &node.inputs,
        Side::Output => &node.outputs,
    };
    ports.iter().position(|p| p.name == port).map(|index| anchor(card, side, index, ports.len()))
}

/// Control points of the cubic bezier from an output anchor to an input anchor.
pub fn bezier(from: Pos2, to: Pos2) -> [Pos2; 4] {
    let pull = ((to.x - from.x).abs() * 0.5).max(MIN_PULL);
    [from, from + Vec2::new(pull, 0.0), to - Vec2::new(pull, 0.0), to]
}

/// The point a fraction `t` (0 to 1) along the curve.
pub fn point(curve: &[Pos2; 4], t: f32) -> Pos2 {
    let [a, b, c, d] = *curve;
    let u = 1.0 - t;
    let v = a.to_vec2() * (u * u * u) + b.to_vec2() * (3.0 * u * u * t) + c.to_vec2() * (3.0 * u * t * t) + d.to_vec2() * (t * t * t);
    v.to_pos2()
}

/// Evenly spaced points along the curve, both ends included.
pub fn samples(curve: &[Pos2; 4]) -> impl Iterator<Item = Pos2> + '_ {
    (0..=SAMPLES).map(move |i| point(curve, i as f32 / SAMPLES as f32))
}

/// Distance from `p` to the curve, measured on its sampled polyline.
pub fn distance(curve: &[Pos2; 4], p: Pos2) -> f32 {
    let points: Vec<Pos2> = samples(curve).collect();
    points.windows(2).filter_map(|w| Some(segment_distance(*w.first()?, *w.get(1)?, p))).fold(f32::INFINITY, f32::min)
}

fn segment_distance(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    let ab = b - a;
    let len = ab.length_sq();
    let t = if len > 0.0 { ((p - a).dot(ab) / len).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t).distance(p)
}

/// Whether the curve's sampled polyline passes through `rect`.
pub fn touches(curve: &[Pos2; 4], rect: Rect) -> bool {
    let points: Vec<Pos2> = samples(curve).collect();
    points.windows(2).any(|w| matches!(w, [a, b] if segment_hits_rect(*a, *b, rect)))
}

/// Liang–Barsky: whether segment `a`–`b` has any point inside `rect`.
fn segment_hits_rect(a: Pos2, b: Pos2, rect: Rect) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-d.x, a.x - rect.min.x), (d.x, rect.max.x - a.x), (-d.y, a.y - rect.min.y), (d.y, rect.max.y - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    t0 <= t1
}

/// A modality as it reads after an article: "a text", "an image".
pub fn modality_phrase(modality: nodecraft_types::Modality) -> &'static str {
    use nodecraft_types::Modality;
    match modality {
        Modality::Text => "a text",
        Modality::Image => "an image",
        Modality::Video => "a video",
        Modality::Audio => "an audio",
        Modality::Mesh => "a 3D",
        Modality::Document => "a document",
        Modality::Style => "a style",
    }
}

/// The same typing rule the document enforces: equal modality, and a list output only into a
/// list input.
pub fn compatible(output: PortType, input: PortType) -> bool {
    output.modality == input.modality && (!output.many || input.many)
}

/// Edges currently feeding `to`, in input order.
pub fn into_port<'a>(canvas: &'a Canvas, to: &Endpoint) -> Vec<&'a nodecraft_graph::Edge> {
    let mut edges: Vec<_> = canvas.edges().values().filter(|e| e.to == *to).collect();
    edges.sort_by_key(|e| e.order);
    edges
}

/// The order for a new edge into a list input: after the last one other than `except` (an edge
/// being rerouted), or `None` if exhausted.
pub fn next_order(canvas: &Canvas, to: &Endpoint, except: Option<EdgeId>) -> Option<u32> {
    into_port(canvas, to).into_iter().rev().find(|e| Some(e.id) != except).map_or(Some(0), |e| e.order.checked_add(1))
}

/// A new edge id, unique across the whole project (see [`crate::ids`]).
pub fn new_edge_id(project: &Project, from: &Endpoint, to: &Endpoint, salt: u64) -> Option<EdgeId> {
    crate::ids::edge_id(project, (from.node, &from.port, to.node, &to.port, salt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Change, Edge, Port};
    use nodecraft_types::{CanvasId, Modality, NodeId, ProjectId};

    const C: CanvasId = CanvasId::from_bytes([1; 16]);

    fn text(many: bool) -> PortType {
        PortType { modality: Modality::Text, many }
    }
    fn end(n: u8, port: &str) -> Endpoint {
        Endpoint { node: NodeId::from_bytes([n; 16]), port: port.into() }
    }
    fn project() -> Project {
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Wires").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        for n in 1..=3 {
            let mut node = Node::new(NodeId::from_bytes([n; 16]), "test");
            node.inputs = vec![Port { name: "list".into(), port_type: text(true) }];
            node.outputs = vec![Port { name: "out".into(), port_type: text(false) }];
            Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        }
        for (e, from, to, order) in [(10, 1, 2, 0), (11, 3, 2, 5)] {
            let edge = Edge { id: EdgeId::from_bytes([e; 16]), from: end(from, "out"), to: end(to, "list"), order };
            Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
        }
        project
    }

    #[test]
    fn anchors_spread_ports_and_center_a_single_one() {
        let card = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(100.0, 90.0));
        assert_eq!(anchor(card, Side::Output, 0, 1), Pos2::new(100.0, 45.0));
        assert_eq!(anchor(card, Side::Input, 0, 2), Pos2::new(0.0, 30.0));
        assert_eq!(anchor(card, Side::Input, 1, 2), Pos2::new(0.0, 60.0));
        // Out-of-range indices clamp instead of leaving the card.
        assert_eq!(anchor(card, Side::Input, 9, 2), Pos2::new(0.0, 60.0));
        assert_eq!(anchor(card, Side::Input, 0, 0), Pos2::new(0.0, 45.0));
    }

    #[test]
    fn curve_hits_and_box_touches() {
        let curve = bezier(Pos2::new(0.0, 0.0), Pos2::new(200.0, 100.0));
        assert!(distance(&curve, Pos2::new(100.0, 50.0)) < 1.0);
        assert!(distance(&curve, Pos2::new(100.0, -40.0)) > 30.0);
        assert!(touches(&curve, Rect::from_center_size(Pos2::new(100.0, 50.0), Vec2::splat(10.0))));
        // A tiny box between two samples still catches the segment through it.
        let between = point(&curve, 0.52);
        assert!(samples(&curve).all(|p| p.distance(between) > 1.0));
        assert!(touches(&curve, Rect::from_center_size(between, Vec2::splat(1.0))));
        assert!(!touches(&curve, Rect::from_center_size(Pos2::new(100.0, 200.0), Vec2::splat(10.0))));
        // Backward noodles keep a minimum pull so they loop rather than collapse.
        let back = bezier(Pos2::new(100.0, 0.0), Pos2::new(0.0, 0.0));
        assert_eq!(back[1], Pos2::new(160.0, 0.0));
    }

    #[test]
    fn typing_order_cycles_and_ids() {
        assert!(compatible(text(false), text(true)));
        assert!(compatible(text(true), text(true)));
        assert!(!compatible(text(true), text(false)));
        assert!(!compatible(text(false), PortType { modality: Modality::Image, many: false }));
        let project = project();
        let canvas = project.canvases().get(&C).unwrap();
        assert_eq!(into_port(canvas, &end(2, "list")).len(), 2);
        assert_eq!(next_order(canvas, &end(2, "list"), None), Some(6));
        assert_eq!(next_order(canvas, &end(3, "list"), None), Some(0));
        assert_eq!(modality_phrase(Modality::Image), "an image");
        let a = new_edge_id(&project, &end(1, "out"), &end(3, "list"), 0).unwrap();
        let b = new_edge_id(&project, &end(1, "out"), &end(3, "list"), 1).unwrap();
        assert_ne!(a, b);
        assert!(!canvas.edges().contains_key(&a));
    }
}
