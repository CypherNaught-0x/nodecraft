use crate::Session;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::{Registry, router};
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, NodeId, ProjectId};
use serde_json::{Value, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const HUB: NodeId = NodeId::from_bytes([9; 16]);

fn edge(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

/// Three text sources feeding a router's list input, in order 1, 2, 3.
fn session() -> Session {
    let kinds = Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Order").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let hub = kinds.get(router::ID).unwrap().instantiate_instance(HUB, Default::default(), &Default::default()).unwrap();
    Change::AddNode { canvas: C, node: hub }.apply(&mut project).unwrap();
    for n in 1..=3u8 {
        let id = NodeId::from_bytes([n; 16]);
        let node = kinds.get("source.text").unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        let wire =
            Edge { id: edge(n), from: Endpoint { node: id, port: "out".into() }, to: Endpoint { node: HUB, port: "in".into() }, order: u32::from(n) - 1 };
        Change::Connect { canvas: C, edge: wire }.apply(&mut project).unwrap();
    }
    Session::new(project).unwrap()
}

fn order(s: &Session) -> Vec<EdgeId> {
    let mut edges: Vec<_> = s.document().canvases().get(&C).unwrap().edges().values().filter(|e| e.to.node == HUB).cloned().collect();
    edges.sort_by_key(|e| e.order);
    edges.into_iter().map(|e| e.id).collect()
}

fn reorder(ids: &[u8]) -> Value {
    json!({"canvas": C, "to": {"node": HUB, "port": "in"}, "edges": ids.iter().map(|n| edge(*n)).collect::<Vec<_>>()})
}

#[test]
fn reorders_a_list_input_as_one_undo_step() {
    let mut s = session();
    s.dispatch("edge.reorder", &reorder(&[3, 1, 2])).unwrap();
    assert_eq!(order(&s), [edge(3), edge(1), edge(2)]);
    s.dispatch("edge.reorder", &reorder(&[2, 1, 3])).unwrap();
    assert_eq!(order(&s), [edge(2), edge(1), edge(3)], "a swap of two neighbours");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(order(&s), [edge(3), edge(1), edge(2)]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(order(&s), [edge(1), edge(2), edge(3)]);
}

#[test]
fn refuses_partial_duplicate_unchanged_or_single_input_orders() {
    let mut s = session();
    let before = s.document().clone();
    for (params, kind) in [
        (reorder(&[1, 2]), ErrorKind::Conflict),
        (reorder(&[1, 1, 2, 3]), ErrorKind::Conflict),
        (reorder(&[1, 2, 3, 7]), ErrorKind::Conflict),
        (reorder(&[1, 2, 3]), ErrorKind::Conflict),
        (json!({"canvas": C, "to": {"node": HUB, "port": "nope"}, "edges": []}), ErrorKind::NotFound),
        (json!({"canvas": C, "to": {"node": NodeId::from_bytes([5; 16]), "port": "in"}, "edges": []}), ErrorKind::NotFound),
    ] {
        let error = s.dispatch("edge.reorder", &params).unwrap_err();
        assert_eq!(error.kind, kind, "{params}: {}", error.message);
    }
    // Sources take no inputs.
    let single = json!({"canvas": C, "to": {"node": NodeId::from_bytes([1; 16]), "port": "in"}, "edges": []});
    assert_eq!(s.dispatch("edge.reorder", &single).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(s.document(), &before);
}
