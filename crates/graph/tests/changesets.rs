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
    let generation = Generation { id: GenerationId::from_bytes([1; 16]), outputs: vec![Value::Text("result".into())], input_hash: None };
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

fn arbitrary_change() -> impl Strategy<Value = Change> {
    // Deliberately tiny ID pools generate missing references, collisions, cycles and reuse.
    (0u8..26, 1u8..5, 1u8..5, 1u8..3, ".{0,16}").prop_map(|(op, n, other, c, text)| {
        let canvas = CanvasId::from_bytes([c; 16]);
        let id = node_id(n);
        let generation = GenerationId::from_bytes([n; 16]);
        match op {
            0 => Change::SetProjectTitle { title: text },
            1 => Change::AddCanvas { canvas: Canvas::new(canvas, text) },
            2 => Change::RemoveCanvas { id: canvas },
            3 => Change::SetCanvasTitle { canvas, title: text },
            4 => Change::AddNode { canvas, node: node(n) },
            5 => Change::RestoreNode { canvas, node: node(n), edges: vec![edge(n, n, other)] },
            6 => Change::RemoveNode { canvas, id },
            7 => Change::SetNode { canvas, node: node(n) },
            8 => Change::MoveNode { canvas, id, pos: Position { x: f32::from(other), y: 1.0 } },
            9 => Change::SetNodeTitle { canvas, id, title: text },
            10 => Change::SetParam { canvas, id, name: "prompt".into(), value: Some(Value::Text(text)) },
            11 => Change::SetParam { canvas, id, name: "prompt".into(), value: None },
            12 => Change::AddGeneration {
                canvas,
                id,
                index: usize::from(other - 1),
                generation: Generation { id: generation, outputs: vec![Value::Text(text)], input_hash: None },
            },
            13 => Change::RemoveGeneration { canvas, id, generation },
            14 => Change::SetActiveGeneration { canvas, id, generation: Some(generation) },
            15 => Change::SetActiveGeneration { canvas, id, generation: None },
            16 => Change::Connect { canvas, edge: edge(n, n, other) },
            17 => Change::Disconnect { canvas, id: edge_id(n) },
            18 => Change::SetEdge { canvas, edge: edge(n, other, n) },
            19 => Change::PutField { canvas, field: Field { id: FieldId::from_bytes([n; 16]), title: text } },
            20 => Change::RemoveField { canvas, id: FieldId::from_bytes([n; 16]) },
            21 => Change::PutTag { canvas, tag: Tag { id: TagId::from_bytes([n; 16]), title: text } },
            22 => Change::RemoveTag { canvas, id: TagId::from_bytes([n; 16]) },
            23 => Change::PutComment { canvas, comment: Comment { id: CommentId::from_bytes([n; 16]), text, pos: Position::default() } },
            _ => Change::RemoveComment { canvas, id: CommentId::from_bytes([n; 16]) },
        }
    })
}

proptest! {
    #[test]
    fn varied_batches_round_trip_or_leave_random_valid_start_unchanged(
        seed in proptest::collection::vec(arbitrary_change(), 0..40),
        edits in proptest::collection::vec(arbitrary_change(), 0..40),
    ) {
        let mut start = connected_project();
        for change in seed {
            // Successful random setup operations build varied valid starting documents.
            let _ = change.apply(&mut start);
        }
        start.validate().unwrap();
        let mut actual = start.clone();
        let changes = Changeset::new(edits);
        let validation = changes.validate(&start);
        match changes.apply(&mut actual) {
            Ok(inverse) => {
                prop_assert!(validation.is_ok());
                let expected = actual.clone();
                actual.validate().unwrap();
                let encoded = serde_json::to_string(&inverse).unwrap();
                let inverse: Changeset = serde_json::from_str(&encoded).unwrap();
                let redo = inverse.apply(&mut actual).unwrap();
                prop_assert_eq!(&actual, &start);
                redo.apply(&mut actual).unwrap();
                prop_assert_eq!(actual, expected);
            }
            Err(_) => {
                prop_assert!(validation.is_err());
                prop_assert_eq!(actual, start);
            }
        }
    }
}

#[test]
fn generation_edits_preserve_middle_entry_order_and_keep_undo_small() {
    let mut project = connected_project();
    let mut item = node(1);
    for n in 1..=3 {
        item.history.push(Generation { id: GenerationId::from_bytes([n; 16]), outputs: vec![Value::Text("old".repeat(10_000))], input_hash: None });
    }
    item.active = Some(GenerationId::from_bytes([2; 16]));
    Change::SetNode { canvas: canvas(), node: item }.apply(&mut project).unwrap();
    let before = project.clone();
    let added = Change::AddGeneration {
        canvas: canvas(),
        id: node_id(1),
        index: 1,
        generation: Generation { id: GenerationId::from_bytes([4; 16]), outputs: vec![Value::Text("new".into())], input_hash: None },
    };
    let undo = added.apply(&mut project).unwrap();
    assert!(serde_json::to_string(&undo).unwrap().len() < 300);
    undo.apply(&mut project).unwrap();
    assert_eq!(project, before);
    assert_round_trip(
        project.clone(),
        Changeset::new(vec![
            added,
            Change::SetActiveGeneration { canvas: canvas(), id: node_id(1), generation: Some(GenerationId::from_bytes([1; 16])) },
            Change::RemoveGeneration { canvas: canvas(), id: node_id(1), generation: GenerationId::from_bytes([2; 16]) },
        ]),
    );
    for change in [
        Change::RemoveGeneration { canvas: canvas(), id: node_id(1), generation: GenerationId::from_bytes([2; 16]) },
        Change::SetActiveGeneration { canvas: canvas(), id: node_id(2), generation: Some(GenerationId::from_bytes([1; 16])) },
        Change::AddGeneration {
            canvas: canvas(),
            id: node_id(2),
            index: 0,
            generation: Generation { id: GenerationId::from_bytes([1; 16]), outputs: vec![], input_hash: None },
        },
        Change::AddGeneration {
            canvas: canvas(),
            id: node_id(1),
            index: usize::MAX,
            generation: Generation { id: GenerationId::from_bytes([4; 16]), outputs: vec![], input_hash: None },
        },
    ] {
        assert!(change.apply(&mut project).is_err());
        assert_eq!(project, before);
    }
}

#[test]
fn batches_can_swap_input_orders_and_replace_referenced_annotations() {
    let mut project = connected_project();
    Change::Connect { canvas: canvas(), edge: edge(3, 1, 3) }.apply(&mut project).unwrap();
    let mut first = edge(2, 2, 3);
    first.order = 1;
    let mut second = edge(3, 1, 3);
    second.order = 2;
    assert_round_trip(
        project.clone(),
        Changeset::new(vec![Change::SetEdge { canvas: canvas(), edge: first }, Change::SetEdge { canvas: canvas(), edge: second }]),
    );
    let field = Field { id: FieldId::from_bytes([1; 16]), title: "Old".into() };
    let mut item = node(1);
    item.field = Some(field.id);
    Changeset::new(vec![Change::PutField { canvas: canvas(), field: field.clone() }, Change::SetNode { canvas: canvas(), node: item }])
        .apply(&mut project)
        .unwrap();
    assert_round_trip(
        project,
        Changeset::new(vec![
            Change::RemoveField { canvas: canvas(), id: field.id },
            Change::PutField { canvas: canvas(), field: Field { title: "Replacement".into(), ..field } },
        ]),
    );
}

#[test]
fn malformed_restore_and_dangling_edge_prefixes_are_atomic() {
    let mut project = connected_project();
    let before = project.clone();
    for changes in [
        vec![
            Change::RemoveNode { canvas: canvas(), id: node_id(2) },
            Change::RestoreNode { canvas: canvas(), node: node(2), edges: vec![edge(1, 1, 2), edge(1, 1, 2)] },
        ],
        vec![Change::Connect { canvas: canvas(), edge: edge(3, 3, 4) }, Change::AddNode { canvas: canvas(), node: node(4) }],
    ] {
        assert!(Changeset::new(changes).apply(&mut project).is_err());
        assert_eq!(project, before);
    }
}

#[test]
fn optional_fields_default_but_identity_and_kind_are_required() {
    let item: Node = serde_json::from_value(serde_json::json!({"id": node_id(1), "kind": "test.text"})).unwrap();
    assert_eq!(item, Node::new(node_id(1), "test.text"));
    let empty: Canvas = serde_json::from_value(serde_json::json!({"id": canvas()})).unwrap();
    assert_eq!(empty, Canvas::new(canvas(), ""));
    let empty: Project = serde_json::from_value(serde_json::json!({"id": ProjectId::from_bytes([0;16])})).unwrap();
    assert_eq!(empty, Project::new(ProjectId::from_bytes([0; 16]), "").unwrap());
    for invalid in [serde_json::json!({"kind":"test.text"}), serde_json::json!({"id":node_id(1)})] {
        assert!(serde_json::from_value::<Node>(invalid).is_err());
    }
}

#[test]
fn aggregate_parameters_and_history_are_bounded_together() {
    let mut project = connected_project();
    let before = project.clone();
    let mut item = node(1);
    for n in 0..4 {
        item.params.insert(format!("p{n}"), Value::Text("x".repeat(1024 * 1024)));
    }
    item.history.push(Generation { id: GenerationId::from_bytes([1; 16]), outputs: vec![Value::Text("x".repeat(1024 * 1024)); 4], input_hash: None });
    assert!(Change::SetNode { canvas: canvas(), node: item }.apply(&mut project).is_err());
    assert_eq!(project, before);
}

#[test]
fn types_and_media_hashes_share_bytes_and_canonical_hex() {
    for fixture in [b"".as_slice(), b"NodeCraft media fixture\0\xff", b"another fixture"] {
        let media = nodecraft_media::ContentHash::of(fixture);
        let typed = nodecraft_types::Blake3::from_bytes(media.0);
        assert_eq!(typed.as_bytes(), &media.0);
        assert_eq!(typed.to_string(), media.to_hex());
        assert_eq!(media.to_hex().parse::<nodecraft_types::Blake3>().unwrap(), typed);
        assert_eq!(nodecraft_media::ContentHash::from_hex(&typed.to_string()), Some(media));
        assert_eq!(serde_json::to_string(&typed).unwrap(), format!("\"{}\"", media.to_hex()));
    }
}

#[test]
#[ignore = "manual release performance measurement"]
fn structural_chain_apply_undo_redo_benchmark() {
    for count in [500u32, 1000, 2000, 4000] {
        let mut project = project();
        let before = project.clone();
        let mut changes = Vec::new();
        for n in 1..=count {
            let mut item = node(1);
            item.id = NodeId::from_bytes(u128::from(n).to_be_bytes());
            changes.push(Change::AddNode { canvas: canvas(), node: item });
        }
        for n in 2..=count {
            changes.push(Change::Connect {
                canvas: canvas(),
                edge: Edge {
                    id: EdgeId::from_bytes(u128::from(n).to_be_bytes()),
                    from: Endpoint { node: NodeId::from_bytes(u128::from(n - 1).to_be_bytes()), port: "out".into() },
                    to: Endpoint { node: NodeId::from_bytes(u128::from(n).to_be_bytes()), port: "in".into() },
                    order: 0,
                },
            });
        }
        let started = std::time::Instant::now();
        let undo = Changeset::new(changes).apply(&mut project).unwrap();
        let apply_time = started.elapsed();
        let expected = project.clone();
        let started = std::time::Instant::now();
        let redo = undo.apply(&mut project).unwrap();
        let undo_time = started.elapsed();
        assert_eq!(project, before);
        let started = std::time::Instant::now();
        redo.apply(&mut project).unwrap();
        let redo_time = started.elapsed();
        assert_eq!(project, expected);
        eprintln!("{count} nodes: apply {apply_time:?}, undo {undo_time:?}, redo {redo_time:?}");
    }
}
