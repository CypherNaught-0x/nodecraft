//! Deterministic fallback when a wire is released inside a card, clear of precise port hits.
use super::*;
use nodecraft_graph::Port;

pub(super) fn target<'a>(
    canvas: &'a Canvas,
    painted: &'a BTreeMap<NodeId, Rect>,
    inputs: &'a input_ports::Cache,
    selected: &Selection,
    wiring: &Wiring,
    pointer: Pos2,
    accepts: impl Fn(&Node, &Port) -> bool,
) -> Option<(&'a Node, &'a Port, Pos2)> {
    // Match the renderer: id order, with selected cards painted last. A refusing front
    // card must not expose a compatible card behind it.
    let (node, rect) =
        drop_targets(canvas, painted).filter(|(_, rect)| rect.contains(pointer)).max_by_key(|(node, _)| (selected.contains(node.id), node.id))?;
    let mut occupied: BTreeMap<&str, u32> = BTreeMap::new();
    for edge in canvas.edges().values().filter(|edge| edge.to.node == node.id && Some(edge.id) != wiring.moving) {
        occupied.entry(edge.to.port.as_str()).and_modify(|order| *order = (*order).max(edge.order)).or_insert(edge.order);
    }
    let ports = inputs.get(node);
    let mut nearest: Option<(f32, &Port, Pos2)> = None;
    for (index, port) in ports.iter().enumerate() {
        if !accepts(node, port) || port.port_type.many && occupied.get(port.name.as_str()) == Some(&u32::MAX) {
            continue;
        }
        let at = noodle::anchor(rect, Side::Input, index, ports.len());
        if port.port_type.many || !occupied.contains_key(port.name.as_str()) {
            return Some((node, port, at));
        }
        let distance = at.distance(pointer);
        if nearest.is_none_or(|(best, ..)| distance < best) {
            nearest = Some((distance, port, at));
        }
    }
    nearest.map(|(_, port, at)| (node, port, at))
}
