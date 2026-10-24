//! Deterministic selected-node layout. No document mutation or UI dependencies.
//! Outer cell origins and spans use an 8-world-point lattice, deliberately, so converting
//! bodies to f32 does not reorder aligned rows with different positive decoration insets.
use crate::{Canvas, MAX_CHANGES, Position};
use nodecraft_types::{Error, ErrorKind, NodeId};
use std::collections::{BTreeMap, BTreeSet};

const LATTICE: f64 = 8.0;
const COORD: f64 = 10_000_000.0;
const METRIC: f64 = 1_000_000.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Grid,
    Layered,
}
/// Positive margins: body position = outer cell origin + (left, top).
#[derive(Clone, Copy, Debug)]
pub struct Extent {
    pub id: NodeId,
    pub width: f64,
    pub height: f64,
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
impl Extent {
    fn validate(self) -> Result<(), Error> {
        if ![self.width, self.height, self.left, self.top, self.right, self.bottom].into_iter().all(|n| n.is_finite() && (0.0..=METRIC).contains(&n))
            || self.width <= 0.0
            || self.height <= 0.0
            || self.width + self.left + self.right > METRIC
            || self.height + self.top + self.bottom > METRIC
        {
            return Err(invalid("layout metrics must be finite, positive and within geometry bounds"));
        }
        Ok(())
    }
    fn width(self) -> f64 {
        align(self.width + self.left + self.right)
    }
    fn height(self) -> f64 {
        align(self.height + self.top + self.bottom)
    }
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn align(n: f64) -> f64 {
    (n / LATTICE).ceil() * LATTICE
}
fn key(n: f64) -> i64 {
    (n / LATTICE).round() as i64
}
fn ranks(canvas: &Canvas) -> Result<BTreeMap<NodeId, usize>, Error> {
    let mut degree: BTreeMap<_, usize> = canvas.nodes().keys().map(|id| (*id, 0)).collect();
    let mut links: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        if !degree.contains_key(&edge.from.node) {
            return Err(invalid("layout edge endpoint is unavailable"));
        }
        let d = degree.get_mut(&edge.to.node).ok_or_else(|| invalid("layout edge endpoint is unavailable"))?;
        *d = d.checked_add(1).ok_or_else(|| invalid("layout degree overflow"))?;
        links.entry(edge.from.node).or_default().push(edge.to.node);
    }
    let mut ready: BTreeSet<_> = degree.iter().filter(|(_, n)| **n == 0).map(|(id, _)| *id).collect();
    let mut result: BTreeMap<NodeId, usize> = BTreeMap::new();
    while let Some(id) = ready.pop_first() {
        let rank = result.get(&id).copied().unwrap_or(0);
        result.insert(id, rank);
        for next in links.get(&id).into_iter().flatten() {
            let next_rank = rank.checked_add(1).ok_or_else(|| invalid("layout rank overflow"))?;
            result.entry(*next).and_modify(|r| *r = (*r).max(next_rank)).or_insert(next_rank);
            let d = degree.get_mut(next).ok_or_else(|| invalid("layout endpoint is unavailable"))?;
            *d = d.checked_sub(1).ok_or_else(|| invalid("layout degree underflow"))?;
            if *d == 0 {
                ready.insert(*next);
            }
        }
    }
    if degree.values().any(|n| *n != 0) {
        return Err(invalid("layout requires an acyclic canvas"));
    }
    Ok(result)
}
/// Bounded structural admission only: no topology traversal, arrangement or project clone.
/// Output-coordinate overflow remains an execution-time error.
pub fn validate_input(canvas: &Canvas, extents: &[Extent], mode: Mode, columns: Option<usize>, gap: f64) -> Result<(), Error> {
    // Canvas is also publicly Deserialize: callers need not have validated it first.
    // Reject hostile topology cardinalities before any allocation or traversal.
    if canvas.nodes().len() > crate::validate::MAX_NODES || canvas.edges().len() > crate::validate::MAX_EDGES {
        return Err(invalid("layout canvas exceeds graph topology limits"));
    }
    if extents.len() < 2 || extents.len() > MAX_CHANGES || extents.len() > canvas.nodes().len() {
        return Err(invalid("select between two and the maximum changeset nodes"));
    }
    if !gap.is_finite() || !(4.0..=4096.0).contains(&gap) {
        return Err(invalid("layout gap is outside bounds"));
    }
    if columns.is_some_and(|n| n == 0 || n > extents.len()) || (mode == Mode::Layered && columns.is_some()) {
        return Err(invalid("columns must be within the grid selection count"));
    }
    let mut seen = BTreeSet::new();
    for e in extents {
        e.validate()?;
        if !seen.insert(e.id) {
            return Err(invalid("duplicate layout node"));
        }
        let node = canvas.nodes().get(&e.id).ok_or_else(|| invalid("selected layout node is unavailable"))?;
        if ![node.pos.x, node.pos.y].into_iter().all(|n| n.is_finite() && f64::from(n).abs() <= COORD) {
            return Err(invalid("layout source position is outside bounds"));
        }
    }
    Ok(())
}
/// O((V+E) log V) layered topology, O(S log S) ordering, bounded by validated canvas
/// membership and MAX_CHANGES. Every proposed position is checked before returning any.
pub fn arrange(canvas: &Canvas, extents: &[Extent], mode: Mode, columns: Option<usize>, gap: f64) -> Result<Vec<(NodeId, Position)>, Error> {
    validate_input(canvas, extents, mode, columns, gap)?;
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::with_capacity(extents.len());
    let mut anchor = (f64::INFINITY, f64::INFINITY);
    for e in extents {
        e.validate()?;
        if !seen.insert(e.id) {
            return Err(invalid("duplicate layout node"));
        }
        let node = canvas.nodes().get(&e.id).ok_or_else(|| invalid("selected layout node is unavailable"))?;
        let (x, y) = (f64::from(node.pos.x), f64::from(node.pos.y));
        if !x.is_finite() || !y.is_finite() || x.abs() > COORD || y.abs() > COORD {
            return Err(invalid("layout source position is outside bounds"));
        }
        let (x, y) = (x - e.left, y - e.top);
        anchor.0 = anchor.0.min(x);
        anchor.1 = anchor.1.min(y);
        // Recover a previously emitted row origin only when the current body is its exact
        // f32 roundtrip. At legal coordinates f32 ULP <= 1, below the 8-point lattice;
        // tiny positive heights must not split an emitted row after inset cancellation.
        let canonical_y = key(y) as f64 * LATTICE;
        let row_y = if mode == Mode::Grid && (canonical_y + e.top) as f32 == node.pos.y { canonical_y } else { y };
        ordered.push((row_y, x, e.id, *e));
    }
    ordered.sort_by(|a, b| {
        if mode == Mode::Grid {
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2))
        } else {
            (key(a.0), key(a.1), a.2).cmp(&(key(b.0), key(b.1), b.2))
        }
    });
    if mode == Mode::Grid {
        // Positive vertical overlap is transitive: a tall card may bridge a row chain.
        // Touching intervals start separate rows. Resolve every component left-to-right.
        let mut start = 0;
        while start < ordered.len() {
            let first = ordered.get(start).ok_or_else(|| invalid("layout row unavailable"))?;
            let mut bottom = first.0 + first.3.height + first.3.top + first.3.bottom;
            let mut end = start + 1;
            while let Some(next) = ordered.get(end) {
                if next.0 >= bottom {
                    break;
                }
                bottom = bottom.max(next.0 + next.3.height + next.3.top + next.3.bottom);
                end += 1;
            }
            if let Some(row) = ordered.get_mut(start..end) {
                row.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)));
            }
            start = end;
        }
    }
    let anchor = (key(anchor.0) as f64 * LATTICE, key(anchor.1) as f64 * LATTICE);
    let gap = align(gap);
    let mut cells = Vec::with_capacity(extents.len());
    match mode {
        Mode::Grid => {
            let n = columns.unwrap_or_else(|| {
                let mut n = 1usize;
                while n.saturating_mul(n) < extents.len() {
                    n += 1;
                }
                n
            });
            let rows = extents.len().div_ceil(n);
            let mut widths = vec![0.0_f64; n];
            let mut heights = vec![0.0_f64; rows];
            for (i, (_, _, _, e)) in ordered.iter().enumerate() {
                if let Some(w) = widths.get_mut(i % n) {
                    *w = w.max(e.width());
                }
                if let Some(h) = heights.get_mut(i / n) {
                    *h = h.max(e.height());
                }
            }
            let offsets = |sizes: &[f64]| {
                let mut p = 0.0;
                sizes
                    .iter()
                    .map(|s| {
                        let old = p;
                        p += s + gap;
                        old
                    })
                    .collect::<Vec<_>>()
            };
            let xs = offsets(&widths);
            let ys = offsets(&heights);
            for (i, (_, _, _, e)) in ordered.into_iter().enumerate() {
                let x = xs.get(i % n).ok_or_else(|| invalid("layout column is unavailable"))?;
                let y = ys.get(i / n).ok_or_else(|| invalid("layout row is unavailable"))?;
                cells.push((e, anchor.0 + x, anchor.1 + y));
            }
        }
        Mode::Layered => {
            let ranks = ranks(canvas)?;
            let mut groups: BTreeMap<usize, Vec<Extent>> = BTreeMap::new();
            for (_, _, id, e) in ordered {
                let rank = ranks.get(&id).ok_or_else(|| invalid("layout rank is unavailable"))?;
                groups.entry(*rank).or_default().push(e);
            }
            let mut x = anchor.0;
            for group in groups.values() {
                let width = group.iter().map(|e| e.width()).fold(0.0_f64, f64::max);
                let mut y = anchor.1;
                for e in group {
                    cells.push((*e, x, y));
                    y += e.height() + gap;
                }
                x += width + gap;
            }
        }
    }
    let mut result = Vec::with_capacity(cells.len());
    for (e, x, y) in cells {
        let (x, y) = (x + e.left, y + e.top);
        if !x.is_finite() || !y.is_finite() || x.abs() > COORD || y.abs() > COORD {
            return Err(invalid("formatted selection would exceed canvas coordinate bounds"));
        }
        let pos = Position { x: x as f32, y: y as f32 };
        if canvas.nodes().get(&e.id).is_some_and(|n| n.pos != pos) {
            result.push((e.id, pos));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Change, Edge, Endpoint, Node, Port, Project};
    use nodecraft_types::{CanvasId, EdgeId, Modality, PortType, ProjectId};
    const C: CanvasId = CanvasId::from_bytes([1; 16]);
    fn id(n: u8) -> NodeId {
        NodeId::from_bytes([n; 16])
    }
    fn scene(n: u8, offset: f32) -> Canvas {
        let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Layout").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
        for i in 1..=n {
            let mut node = Node::new(id(i), "test.text");
            node.pos = Position { x: offset + f32::from(i) * 3.25, y: offset - f32::from(i) * 17.5 };
            let port = Port { name: "text".into(), port_type: PortType { modality: Modality::Text, many: true } };
            node.inputs.push(port.clone());
            node.outputs.push(port);
            Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
        }
        p.canvases().get(&C).unwrap().clone()
    }
    fn metrics(canvas: &Canvas) -> Vec<Extent> {
        canvas
            .nodes()
            .keys()
            .enumerate()
            .map(|(i, id)| Extent {
                id: *id,
                width: 320.25 + i as f64,
                height: 220.5 + i as f64 * 32.0,
                left: i as f64 * 0.25,
                top: 22.5 + i as f64 * 11.25,
                right: 51.25,
                bottom: 5.5,
            })
            .collect()
    }
    fn apply(canvas: &mut Canvas, result: &[(NodeId, Position)]) {
        for (id, pos) in result {
            canvas.nodes.get_mut(id).unwrap().pos = *pos;
        }
    }
    fn connect(c: &mut Canvas, n: u8, a: u8, b: u8) {
        let edge = Edge {
            id: EdgeId::from_bytes([n; 16]),
            from: Endpoint { node: id(a), port: "text".into() },
            to: Endpoint { node: id(b), port: "text".into() },
            order: 0,
        };
        c.edges.insert(edge.id, edge);
    }
    #[test]
    fn mixed_insets_fractional_metrics_extreme_coordinates_and_ties_reapply_exactly() {
        for offset in [0.0, -5000.0, 9_990_000.0, -9_990_000.0, 4.0, -4.0] {
            for mode in [Mode::Grid, Mode::Layered] {
                for cols in [None, Some(1), Some(3)] {
                    if mode == Mode::Layered && cols.is_some() {
                        continue;
                    }
                    let mut c = scene(7, offset);
                    let e = metrics(&c);
                    let result = arrange(&c, &e, mode, cols, 64.0).unwrap();
                    apply(&mut c, &result);
                    assert!(arrange(&c, &e, mode, cols, 64.0).unwrap().is_empty(), "{offset} {mode:?} {cols:?}");
                    for (i, a) in e.iter().enumerate() {
                        for b in e.iter().skip(i + 1) {
                            let p = c.nodes().get(&a.id).unwrap().pos;
                            let q = c.nodes().get(&b.id).unwrap().pos;
                            let (x, y) = (f64::from(p.x) - a.left, f64::from(p.y) - a.top);
                            let (u, v) = (f64::from(q.x) - b.left, f64::from(q.y) - b.top);
                            assert!(
                                x + a.width + a.left + a.right <= u + 1.0
                                    || u + b.width + b.left + b.right <= x + 1.0
                                    || y + a.height + a.top + a.bottom <= v + 1.0
                                    || v + b.height + b.top + b.bottom <= y + 1.0
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(key(4.0), 1);
        assert_eq!(key(-4.0), -1);
    }
    #[test]
    fn sub_ulp_heights_and_fractional_insets_keep_emitted_rows_exactly_idempotent() {
        for offset in [9_000_000.0, -9_000_000.0, 9_999_000.0] {
            for top in [0.1, 0.6, 1.4, 3.5, 7.9, 31.6] {
                for tall in [0.1, 100.0] {
                    for separate_rows in [false, true] {
                        for mirrored in [false, true] {
                            let mut c = scene(2, 0.0);
                            c.nodes.get_mut(&id(1)).unwrap().pos = Position { x: if mirrored { 100.0 } else { 0.0 }, y: offset };
                            c.nodes.get_mut(&id(2)).unwrap().pos =
                                Position { x: if mirrored { 0.0 } else { 100.0 }, y: offset + if separate_rows { 200.0 } else { 0.0 } };
                            let mut e = vec![
                                Extent { id: id(1), width: 1.0, height: 0.1, left: 0.0, top, right: 0.0, bottom: 0.0 },
                                Extent { id: id(2), width: 1.0, height: tall, left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 },
                            ];
                            let first = arrange(&c, &e, Mode::Grid, Some(2), 64.0).unwrap();
                            e.reverse();
                            assert_eq!(first, arrange(&c, &e, Mode::Grid, Some(2), 64.0).unwrap());
                            apply(&mut c, &first);
                            for _ in 0..3 {
                                assert!(arrange(&c, &e, Mode::Grid, Some(2), 64.0).unwrap().is_empty(), "{offset} {top} {tall} {separate_rows}");
                                e.reverse();
                            }
                            if separate_rows {
                                assert!(c.nodes[&id(1)].pos.x < c.nodes[&id(2)].pos.x);
                            }
                        }
                    }
                }
            }
        }
        // f32 ULP >= 8 is outside the accepted coordinate domain, never silently clamped.
        let mut c = scene(2, 0.0);
        c.nodes.get_mut(&id(1)).unwrap().pos.y = 100_000_000.0;
        assert!(arrange(&c, &metrics(&c), Mode::Grid, Some(2), 64.0).is_err());
    }
    #[test]
    fn grid_overlap_components_are_left_to_right_with_touching_rows_and_stable_ties() {
        // 1/2/3 form a transitive overlap chain, although 1 and 3 do not overlap.
        // 4 touches its end and starts another row. Equal x resolves by ID.
        let mut c = scene(5, 0.0);
        let mut e = metrics(&c);
        for (n, x, y, height) in [(1, 600.0, 0.0, 100.0), (2, 300.0, 90.0, 100.0), (3, 0.0, 180.0, 100.0), (4, 0.0, 280.0, 50.0), (5, 0.0, 280.0, 50.0)] {
            c.nodes.get_mut(&id(n)).unwrap().pos = Position { x, y };
            let m = e.iter_mut().find(|e| e.id == id(n)).unwrap();
            m.height = height;
            m.left = 0.0;
            m.top = 0.0;
            m.right = 0.0;
            m.bottom = 0.0;
        }
        let result = arrange(&c, &e, Mode::Grid, Some(3), 64.0).unwrap();
        let mut reversed = e.clone();
        reversed.reverse();
        assert_eq!(result, arrange(&c, &reversed, Mode::Grid, Some(3), 64.0).unwrap());
        apply(&mut c, &result);
        assert!(c.nodes[&id(3)].pos.x < c.nodes[&id(2)].pos.x);
        assert!(c.nodes[&id(2)].pos.x < c.nodes[&id(1)].pos.x);
        assert!(c.nodes[&id(4)].pos.y > c.nodes[&id(1)].pos.y);
        assert!(c.nodes[&id(4)].pos.x < c.nodes[&id(5)].pos.x);
        assert!(arrange(&c, &e, Mode::Grid, Some(3), 64.0).unwrap().is_empty());
    }
    #[test]
    fn layered_whole_dag_paths_via_unselected_nodes_and_stable_ties() {
        let mut c = scene(5, 0.0);
        connect(&mut c, 10, 1, 2);
        connect(&mut c, 11, 2, 3);
        connect(&mut c, 12, 1, 4);
        connect(&mut c, 13, 4, 3);
        let e: Vec<_> = metrics(&c).into_iter().filter(|e| e.id != id(2)).collect();
        let untouched = c.nodes().get(&id(2)).unwrap().clone();
        let changes = arrange(&c, &e, Mode::Layered, None, 64.0).unwrap();
        assert!(changes.iter().all(|(n, _)| *n != id(2)));
        apply(&mut c, &changes);
        assert!(c.nodes().get(&id(1)).unwrap().pos.x < c.nodes().get(&id(3)).unwrap().pos.x);
        assert_eq!(c.nodes().get(&id(2)).unwrap(), &untouched);
        assert!(arrange(&c, &e, Mode::Layered, None, 64.0).unwrap().is_empty());
        connect(&mut c, 14, 3, 1);
        assert!(arrange(&c, &e, Mode::Layered, None, 64.0).is_err());
    }
    #[test]
    fn hostile_inputs_and_late_range_failure_return_no_partial_layout() {
        let c = scene(3, 0.0);
        let e = metrics(&c);
        for gap in [f64::NAN, f64::INFINITY, 0.0, 4097.0] {
            assert!(arrange(&c, &e, Mode::Grid, None, gap).is_err());
        }
        for cols in [Some(0), Some(4)] {
            assert!(arrange(&c, &e, Mode::Grid, cols, 64.0).is_err());
        }
        assert!(arrange(&c, &e, Mode::Layered, Some(1), 64.0).is_err());
        for bad in [f64::NAN, -1.0, 1e100] {
            let mut e = e.clone();
            e.get_mut(2).unwrap().top = bad;
            assert!(arrange(&c, &e, Mode::Grid, None, 64.0).is_err());
        }
        let mut duplicate = e.clone();
        duplicate.get_mut(1).unwrap().id = id(1);
        assert!(arrange(&c, &duplicate, Mode::Grid, None, 64.0).is_err());
        let c = scene(3, 9_999_900.0);
        assert!(arrange(&c, &metrics(&c), Mode::Grid, None, 64.0).is_err());
    }
    #[test]
    fn maximum_selected_canvas_and_dense_forward_adjacency_remain_bounded() {
        let mut c = Canvas::new(C, "Maximum");
        let id_of = |n: usize| NodeId::from_bytes((n as u128).to_le_bytes());
        for n in 0..MAX_CHANGES {
            c.nodes.insert(id_of(n), Node::new(id_of(n), "test.text"));
        }
        let mut index = 0u128;
        for n in 0..MAX_CHANGES {
            for delta in 1..=5 {
                if n + delta < MAX_CHANGES {
                    let edge = Edge {
                        id: EdgeId::from_bytes(index.to_le_bytes()),
                        from: Endpoint { node: id_of(n), port: "out".into() },
                        to: Endpoint { node: id_of(n + delta), port: "in".into() },
                        order: 0,
                    };
                    c.edges.insert(edge.id, edge);
                    index += 1;
                }
            }
        }
        let e: Vec<_> = c.nodes.keys().map(|id| Extent { id: *id, width: 320.0, height: 220.0, left: 0.0, top: 23.0, right: 10.0, bottom: 10.0 }).collect();
        for mode in [Mode::Grid, Mode::Layered] {
            let mut c = c.clone();
            let changes = arrange(&c, &e, mode, None, 64.0).unwrap();
            assert!(changes.len() <= MAX_CHANGES);
            apply(&mut c, &changes);
            assert!(arrange(&c, &e, mode, None, 64.0).unwrap().is_empty());
        }
    }
    #[test]
    fn public_api_rejects_oversized_topology_with_a_small_selection() {
        let c = scene(2, 0.0);
        let e = metrics(&c);
        let mut nodes = c.clone();
        for n in 0..=crate::validate::MAX_NODES {
            let id = NodeId::from_bytes((n as u128).to_le_bytes());
            nodes.nodes.insert(id, Node::new(id, "test.text"));
        }
        let mut edges = c.clone();
        for n in 0..=crate::validate::MAX_EDGES {
            let edge = Edge {
                id: EdgeId::from_bytes((n as u128).to_le_bytes()),
                from: Endpoint { node: id(1), port: "text".into() },
                to: Endpoint { node: id(2), port: "text".into() },
                order: 0,
            };
            edges.edges.insert(edge.id, edge);
        }
        for mode in [Mode::Grid, Mode::Layered] {
            for canvas in [&nodes, &edges] {
                assert!(arrange(canvas, &e, mode, None, 64.0).is_err());
            }
        }
    }
    proptest::proptest! {
        #[test]
        fn bounded_dag_layout_is_idempotent(n in 2u8..40, offset in -1_000_000f32..1_000_000f32, layered in proptest::bool::ANY) {
            let mut c=scene(n,offset);for i in 1..n {connect(&mut c,100+i,i,i+1);}
            let e=metrics(&c);let mode=if layered {Mode::Layered} else {Mode::Grid};
            let changes=arrange(&c,&e,mode,None,64.0).unwrap();apply(&mut c,&changes);
            proptest::prop_assert!(arrange(&c,&e,mode,None,64.0).unwrap().is_empty());
            if layered {for i in 1..n {proptest::prop_assert!(c.nodes().get(&id(i)).unwrap().pos.x<c.nodes().get(&id(i+1)).unwrap().pos.x);}}
        }
    }
}
