//! `Canvas::same_but_layout` (#549): moving and grouping keep the canvas the same for work
//! derived from it; anything a run reads makes it differ.
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Field, Node, NodeFieldAssignment, Port, Position, Project};
use nodecraft_types::{CanvasId, EdgeId, FieldId, Modality, NodeId, PortType, ProjectId, Value};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([2; 16]);
const B: NodeId = NodeId::from_bytes([3; 16]);

fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([0; 16]), "Layout").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }.apply(&mut p).unwrap();
    let node = |id| {
        let mut node = Node::new(id, "test.node");
        let text = PortType { modality: Modality::Text, many: false };
        node.inputs = vec![Port { name: "in".into(), port_type: text }];
        node.outputs = vec![Port { name: "out".into(), port_type: text }];
        node
    };
    Changeset::new(vec![Change::AddNode { canvas: CANVAS, node: node(A) }, Change::AddNode { canvas: CANVAS, node: node(B) }]).apply(&mut p).unwrap();
    p
}

fn after(change: Change) -> (Project, Project) {
    let before = project();
    let mut now = before.clone();
    Changeset::new(vec![change]).apply(&mut now).unwrap();
    (before, now)
}

fn same(change: Change) -> bool {
    let (before, now) = after(change);
    before.canvases().get(&CANVAS).unwrap().same_but_layout(now.canvases().get(&CANVAS).unwrap())
}

#[test]
fn moving_and_grouping_are_layout() {
    assert!(same(Change::MoveNode { canvas: CANVAS, id: A, pos: Position { x: 400.0, y: -20.0 } }));
    let field = FieldId::from_bytes([4; 16]);
    assert!(same(Change::PutField { canvas: CANVAS, field: Field::new(field, "Group") }));
    let mut before = project();
    Changeset::new(vec![Change::PutField { canvas: CANVAS, field: Field::new(field, "Group") }]).apply(&mut before).unwrap();
    let mut grouped = before.clone();
    let assignments = vec![NodeFieldAssignment { node: A, field: Some(field) }];
    Changeset::new(vec![Change::SetNodeFields { canvas: CANVAS, assignments }]).apply(&mut grouped).unwrap();
    assert!(before.canvases().get(&CANVAS).unwrap().same_but_layout(grouped.canvases().get(&CANVAS).unwrap()));
}

#[test]
fn what_a_run_reads_is_not_layout() {
    assert!(!same(Change::SetParam { canvas: CANVAS, id: A, name: "text".into(), value: Some(Value::Text("dawn".into())) }));
    assert!(!same(Change::SetNodeTitle { canvas: CANVAS, id: A, title: "Prompt".into() }));
    let edge = Edge { id: EdgeId::from_bytes([5; 16]), from: Endpoint { node: A, port: "out".into() }, to: Endpoint { node: B, port: "in".into() }, order: 0 };
    assert!(!same(Change::Connect { canvas: CANVAS, edge }));
    assert!(!same(Change::RemoveNode { canvas: CANVAS, id: B }));
}
