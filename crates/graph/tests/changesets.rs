use nodecraft_graph::{
    Canvas, Change, Changeset, Comment, Edge, Endpoint, Field, Generation, MAX_CHANGES, MAX_VALUE_DEPTH, MAX_VALUE_ITEMS, Node, Port, Position, Project, Tag,
};
use nodecraft_types::{CanvasId, CommentId, EdgeId, FieldId, GenerationId, Modality, NodeId, PortType, ProjectId, TagId, Value};
use proptest::prelude::*;

fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
fn node_id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn edge_id(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

fn node(n: u8) -> Node {
    let mut node = Node::new(node_id(n), "test.text");
    node.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: true } });
    node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node
}

fn edge(n: u8, from: u8, to: u8) -> Edge {
    Edge {
        id: edge_id(n),
        from: Endpoint { node: node_id(from), port: "out".into() },
        to: Endpoint { node: node_id(to), port: "in".into() },
        order: u32::from(from),
    }
}

fn project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut project).unwrap();
    project
}

fn connected_project() -> Project {
    let mut project = project();
    Changeset::new(vec![
        Change::AddNode { canvas: canvas(), node: node(1) },
        Change::AddNode { canvas: canvas(), node: node(2) },
        Change::AddNode { canvas: canvas(), node: node(3) },
        Change::Connect { canvas: canvas(), edge: edge(1, 1, 2) },
        Change::Connect { canvas: canvas(), edge: edge(2, 2, 3) },
    ])
    .apply(&mut project)
    .unwrap();
    project
}

fn assert_round_trip(mut project: Project, changes: Changeset) {
    let before = project.clone();
    changes.validate(&project).unwrap();
    assert_eq!(project, before);
    let inverse = changes.apply(&mut project).unwrap();
    let after = project.clone();
    project.validate().unwrap();
    let encoded = serde_json::to_string(&inverse).unwrap();
    let inverse: Changeset = serde_json::from_str(&encoded).unwrap();
    let redo = inverse.apply(&mut project).unwrap();
    assert_eq!(project, before);
    redo.apply(&mut project).unwrap();
    assert_eq!(project, after);
    assert_eq!(serde_json::from_str::<Project>(&serde_json::to_string(&project).unwrap()).unwrap(), project);
}

proptest! {
    #[test]
    fn random_changesets_apply_undo_redo_exactly(
        count in 2u8..16,
        links in proptest::collection::vec(any::<bool>(), 1..32),
        title in ".{0,60}",
        edits in proptest::collection::vec((0u8..16, -10000i16..10000, -10000i16..10000, ".{0,40}"), 0..25),
    ) {
        let mut changes = Vec::new();
        for n in 1..=count { changes.push(Change::AddNode { canvas: canvas(), node: node(n) }); }
        for n in 1..count {
            if links.get(usize::from(n) % links.len()).copied().unwrap_or(false) {
                changes.push(Change::Connect { canvas: canvas(), edge: edge(n, n, n + 1) });
            }
        }
        for (n, x, y, text) in edits {
            let id = node_id(1 + n % count);
            changes.push(Change::MoveNode { canvas: canvas(), id, pos: Position { x: f32::from(x), y: f32::from(y) } });
            changes.push(Change::SetParam { canvas: canvas(), id, name: "prompt".into(), value: Some(Value::List(vec![Value::Text(text)])) });
        }
        changes.push(Change::SetProjectTitle { title });
        changes.push(Change::RemoveNode { canvas: canvas(), id: node_id(count / 2) });
        assert_round_trip(project(), Changeset::new(changes));
    }

    #[test]
    fn invalid_random_suffix_rolls_back_all_prior_edits(title in ".{0,100}", x in -1000i16..1000) {
        let mut project = connected_project();
        let before = project.clone();
        let changes = Changeset::new(vec![
            Change::SetProjectTitle { title },
            Change::MoveNode { canvas: canvas(), id: node_id(1), pos: Position { x: f32::from(x), y: 0.0 } },
            Change::AddNode { canvas: canvas(), node: node(2) },
        ]);
        prop_assert!(changes.validate(&project).is_err());
        prop_assert!(changes.apply(&mut project).is_err());
        prop_assert_eq!(project, before);
    }
}

#[test]
fn removing_a_node_restores_all_edges_in_one_inverse_operation() {
    let mut project = connected_project();
    let before = project.clone();
    let inverse = Change::RemoveNode { canvas: canvas(), id: node_id(2) }.apply(&mut project).unwrap();
    assert!(project.canvases().get(&canvas()).unwrap().edges().is_empty());
    assert_eq!(inverse.changes.len(), 1);
    inverse.apply(&mut project).unwrap();
    assert_eq!(project, before);
}

#[test]
fn all_edit_types_and_annotation_stubs_are_reversible() {
    let field = Field { id: FieldId::from_bytes([1; 16]), title: "Group".into() };
    let tag = Tag { id: TagId::from_bytes([1; 16]), title: "Tag".into() };
    let comment = Comment { id: CommentId::from_bytes([1; 16]), text: "Note".into(), pos: Position::default() };
    let mut replacement = node(1);
    replacement.field = Some(field.id);
    replacement.tag = Some(tag.id);
    let generation = Generation { id: GenerationId::from_bytes([1; 16]), outputs: vec![Value::Text("result".into())] };
    replacement.active = Some(generation.id);
    replacement.history.push(generation);
    let mut rerouted = edge(1, 1, 3);
    rerouted.order = 99;
    assert_round_trip(
        connected_project(),
        Changeset::new(vec![
            Change::SetCanvasTitle { canvas: canvas(), title: "Renamed".into() },
            Change::PutField { canvas: canvas(), field: field.clone() },
            Change::PutTag { canvas: canvas(), tag: tag.clone() },
            Change::PutComment { canvas: canvas(), comment: comment.clone() },
            Change::PutComment { canvas: canvas(), comment: Comment { text: "Edited".into(), ..comment.clone() } },
            Change::SetNode { canvas: canvas(), node: replacement },
            Change::SetNodeTitle { canvas: canvas(), id: node_id(2), title: "Renamed node".into() },
            Change::SetParam { canvas: canvas(), id: node_id(2), name: "prompt".into(), value: Some(Value::Text("first".into())) },
            Change::SetParam { canvas: canvas(), id: node_id(2), name: "prompt".into(), value: None },
            Change::SetEdge { canvas: canvas(), edge: rerouted },
            Change::Disconnect { canvas: canvas(), id: edge_id(2) },
            Change::SetNode { canvas: canvas(), node: node(1) },
            Change::RemoveField { canvas: canvas(), id: field.id },
            Change::RemoveTag { canvas: canvas(), id: tag.id },
            Change::RemoveComment { canvas: canvas(), id: comment.id },
            Change::RemoveCanvas { id: canvas() },
        ]),
    );
    assert_round_trip(connected_project(), Changeset::default());
}

#[test]
fn invalid_connections_and_replacements_leave_document_untouched() {
    let original = connected_project();
    let mut wrong_port = edge(3, 1, 3);
    wrong_port.from.port = "missing".into();
    let mut wrong_type = node(1);
    wrong_type.outputs.get_mut(0).unwrap().port_type.modality = Modality::Image;
    let mut duplicate_order = edge(3, 1, 3);
    duplicate_order.order = 2;
    for change in [
        Change::Connect { canvas: canvas(), edge: edge(3, 3, 1) },
        Change::Connect { canvas: canvas(), edge: edge(3, 4, 1) },
        Change::Connect { canvas: canvas(), edge: edge(3, 1, 1) },
        Change::Connect { canvas: canvas(), edge: edge(1, 1, 3) },
        Change::Connect { canvas: canvas(), edge: wrong_port },
        Change::Connect { canvas: canvas(), edge: duplicate_order },
        Change::SetNode { canvas: canvas(), node: wrong_type },
        Change::RemoveNode { canvas: canvas(), id: node_id(99) },
        Change::RemoveCanvas { id: CanvasId::from_bytes([99; 16]) },
    ] {
        let mut project = original.clone();
        assert!(change.validate(&project).is_err(), "{change:?}");
        assert!(change.apply(&mut project).is_err(), "{change:?}");
        assert_eq!(project, original);
    }
}

#[test]
fn single_input_cardinality_and_cross_canvas_ids_are_checked() {
    let mut project = project();
    let mut target = node(2);
    target.inputs.get_mut(0).unwrap().port_type.many = false;
    let mut connection = edge(1, 1, 2);
    connection.order = 0;
    Changeset::new(vec![
        Change::AddNode { canvas: canvas(), node: node(1) },
        Change::AddNode { canvas: canvas(), node: target },
        Change::Connect { canvas: canvas(), edge: connection },
    ])
    .apply(&mut project)
    .unwrap();
    let before = project.clone();
    let mut extra = edge(2, 1, 2);
    extra.order = 0;
    assert!(Change::Connect { canvas: canvas(), edge: extra }.apply(&mut project).is_err());
    assert_eq!(project, before);
    let other = CanvasId::from_bytes([2; 16]);
    let changes = Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(other, "Other") }, Change::AddNode { canvas: other, node: node(1) }]);
    assert!(changes.apply(&mut project).is_err());
    assert_eq!(project, before);
}

#[test]
fn hostile_programmatic_values_are_rejected_before_clone() {
    let mut project = connected_project();
    let before = project.clone();
    let mut deep = Value::Text("leaf".into());
    for _ in 0..=MAX_VALUE_DEPTH {
        deep = Value::List(vec![deep]);
    }
    for value in [deep, Value::List(vec![Value::Text(String::new()); MAX_VALUE_ITEMS]), Value::Text("x".repeat(1024 * 1024 + 1))] {
        let changes = Changeset::new(vec![Change::SetParam { canvas: canvas(), id: node_id(1), name: "prompt".into(), value: Some(value) }]);
        assert!(changes.apply(&mut project).is_err());
        assert_eq!(project, before);
    }
    for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
        assert!(Change::MoveNode { canvas: canvas(), id: node_id(1), pos: Position { x, y: 0.0 } }.apply(&mut project).is_err());
    }
    assert!(Changeset::new(vec![Change::SetProjectTitle { title: "x".into() }; MAX_CHANGES + 1]).apply(&mut project).is_err());
    assert_eq!(project, before);
}

#[test]
fn project_deserialization_validates_keys_references_and_cycles() {
    let original = connected_project();
    let base = serde_json::to_value(&original).unwrap();
    let mut bad_key = base.clone();
    bad_key["canvases"][canvas().to_string()]["nodes"][node_id(1).to_string()]["id"] = serde_json::json!(node_id(99));
    assert!(serde_json::from_value::<Project>(bad_key).is_err());
    let mut dangling = base.clone();
    dangling["canvases"][canvas().to_string()]["nodes"][node_id(1).to_string()]["field"] = serde_json::json!(FieldId::from_bytes([9; 16]));
    assert!(serde_json::from_value::<Project>(dangling).is_err());
    let mut cycle = base.clone();
    cycle["canvases"][canvas().to_string()]["edges"][edge_id(3).to_string()] = serde_json::to_value(edge(3, 3, 1)).unwrap();
    assert!(serde_json::from_value::<Project>(cycle).is_err());
    let mut active = base;
    active["canvases"][canvas().to_string()]["nodes"][node_id(1).to_string()]["active"] = serde_json::json!(GenerationId::from_bytes([9; 16]));
    assert!(serde_json::from_value::<Project>(active).is_err());
}

#[test]
fn thousand_node_chain_supports_bulk_moves_and_undo() {
    let mut nodes = std::collections::BTreeMap::new();
    let mut edges = std::collections::BTreeMap::new();
    let mut changes = Vec::new();
    for n in 1u32..=1000 {
        let id = NodeId::from_bytes(u128::from(n).to_be_bytes());
        let mut item = node(1);
        item.id = id;
        nodes.insert(id, item);
        changes.push(Change::MoveNode { canvas: canvas(), id, pos: Position { x: n as f32, y: 42.0 } });
        if n > 1 {
            let id = EdgeId::from_bytes(u128::from(n).to_be_bytes());
            edges.insert(
                id,
                Edge {
                    id,
                    from: Endpoint { node: NodeId::from_bytes(u128::from(n - 1).to_be_bytes()), port: "out".into() },
                    to: Endpoint { node: NodeId::from_bytes(u128::from(n).to_be_bytes()), port: "in".into() },
                    order: 0,
                },
            );
        }
    }
    let snapshot: Canvas = serde_json::from_value(serde_json::json!({
        "id": canvas(), "title":"Long chain", "nodes":nodes, "edges":edges,
        "fields":{}, "tags":{}, "comments":{}
    }))
    .unwrap();
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Large").unwrap();
    Change::AddCanvas { canvas: snapshot }.apply(&mut project).unwrap();
    let before = project.clone();
    let started = std::time::Instant::now();
    let undo = Changeset::new(changes).apply(&mut project).unwrap();
    eprintln!("1,000-node/999-edge bulk move: {:?}", started.elapsed());
    assert!(project.canvases().get(&canvas()).unwrap().nodes().values().all(|node| node.pos.y == 42.0));
    let moved = project.clone();
    let redo = undo.apply(&mut project).unwrap();
    assert_eq!(project, before);
    redo.apply(&mut project).unwrap();
    assert_eq!(project, moved);
}

#[test]
fn scalar_fast_path_still_enforces_parameter_count_and_atomicity() {
    let mut project = project();
    let mut item = node(1);
    for n in 0..256 {
        item.params.insert(format!("p{n}"), Value::Text("v".into()));
    }
    Change::AddNode { canvas: canvas(), node: item }.apply(&mut project).unwrap();
    let before = project.clone();
    let edits = Changeset::new(vec![
        Change::SetNodeTitle { canvas: canvas(), id: node_id(1), title: "Uncommitted".into() },
        Change::SetParam { canvas: canvas(), id: node_id(1), name: "overflow".into(), value: Some(Value::Text("v".into())) },
    ]);
    assert!(edits.apply(&mut project).is_err());
    assert_eq!(project, before);
}
