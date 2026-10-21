//! `Canvas::same_but_layout` (#549): moving and grouping keep the canvas the same for work
//! derived from it; anything a run reads makes it differ.
use nodecraft_graph::{Canvas, Change, Changeset, Comment, Edge, Endpoint, Field, Generation, Node, NodeFieldAssignment, Port, Position, Project};
use nodecraft_types::{CanvasId, CommentId, EdgeId, FieldId, GenerationId, Modality, NodeId, PortType, ProjectId, Value};

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

/// Comments are notes people leave each other and the agent: no plan or run reads one, so
/// whatever happens to one leaves the work derived from the canvas standing (#556).
#[test]
fn a_comment_is_layout() {
    let id = CommentId::from_bytes([6; 16]);
    let comment = Comment::new(id, "Check the crop against the reference", Position { x: 120.0, y: -60.0 });
    assert!(same(Change::PutComment { canvas: CANVAS, comment: comment.clone() }));
    let mut before = project();
    Changeset::new(vec![Change::PutComment { canvas: CANVAS, comment }]).apply(&mut before).unwrap();
    for change in [
        Change::SetCommentPin { canvas: CANVAS, id, node: Some(A), pos: Position { x: 8.0, y: 12.0 } },
        Change::SetCommentText { canvas: CANVAS, id, text: "Checked: the crop holds.".into() },
        Change::SetCommentResolved { canvas: CANVAS, id, resolved: true },
        Change::RemoveComment { canvas: CANVAS, id },
    ] {
        let mut now = before.clone();
        Changeset::new(vec![change]).apply(&mut now).unwrap();
        assert!(before.canvases().get(&CANVAS).unwrap().same_but_layout(now.canvases().get(&CANVAS).unwrap()), "a comment is not work");
    }
}

/// A result is the most expensive thing a canvas holds, and which one is shown changes what
/// runs downstream: either difference is a real change (#556).
#[test]
fn a_result_is_not_layout() {
    let generation = Generation::new(GenerationId::from_bytes([7; 16]), vec![Value::Text("a quiet forest at dawn".into())]);
    assert!(!same(Change::AddGeneration { canvas: CANVAS, id: A, index: 0, generation: generation.clone() }), "a node with a result isn't one without");
    let mut before = project();
    Changeset::new(vec![Change::AddGeneration { canvas: CANVAS, id: A, index: 0, generation: generation.clone() }]).apply(&mut before).unwrap();
    let mut now = before.clone();
    Changeset::new(vec![Change::SetActiveGeneration { canvas: CANVAS, id: A, generation: Some(generation.id) }]).apply(&mut now).unwrap();
    assert!(!before.canvases().get(&CANVAS).unwrap().same_but_layout(now.canvases().get(&CANVAS).unwrap()), "which result is shown is not layout");
}
