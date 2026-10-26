use nodecraft_graph::{Canvas, Change, Changeset, Generation, MAX_VALUE_DEPTH, MAX_VALUE_ITEMS, Node, Project};
use nodecraft_types::{CanvasId, GenerationId, Integer, NodeId, Number, ProjectId, Unsigned, Value};

fn setup() -> (Project, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Scalars").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }, Change::AddNode { canvas, node: Node::new(node, "test.scalars") }])
        .apply(&mut project)
        .unwrap();
    (project, canvas, node)
}

fn scalars() -> Vec<Value> {
    vec![
        Value::Bool(true),
        Value::Integer(Integer(i64::MIN)),
        Value::Integer(Integer(i64::MAX)),
        Value::Unsigned(Unsigned(u64::MAX)),
        Value::Number(Number::new(51.248178375505404).unwrap()),
        Value::Number(Number::new(f64::MAX).unwrap()),
    ]
}

#[test]
fn scalar_parameters_and_generation_outputs_survive_serialized_apply_undo_redo() {
    let (mut project, canvas, id) = setup();
    let before = project.clone();
    let generation = GenerationId::from_bytes([3; 16]);
    let mut edits: Vec<_> =
        scalars().into_iter().enumerate().map(|(index, value)| Change::SetParam { canvas, id, name: format!("p{index}"), value: Some(value) }).collect();
    edits.push(Change::AddGeneration {
        canvas,
        id,
        index: 0,
        generation: Generation {
            id: generation,
            outputs: vec![Value::List(scalars())],
            input_hash: None,
            items: Vec::new(),
            settings: Vec::new(),
            source_versions: Vec::new(),
            incomplete: None,
        },
    });
    edits.push(Change::SetActiveGeneration { canvas, id, generation: Some(generation) });
    let forward = Changeset::new(edits);
    let forward: Changeset = serde_json::from_str(&serde_json::to_string(&forward).unwrap()).unwrap();
    let undo = forward.apply(&mut project).unwrap();
    let expected = project.clone();
    project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(project, expected);
    let undo: Changeset = serde_json::from_str(&serde_json::to_string(&undo).unwrap()).unwrap();
    let redo = undo.apply(&mut project).unwrap();
    assert_eq!(project, before);
    let redo: Changeset = serde_json::from_str(&serde_json::to_string(&redo).unwrap()).unwrap();
    redo.apply(&mut project).unwrap();
    assert_eq!(project, expected);
    let restore = Change::RemoveNode { canvas, id }.apply(&mut project).unwrap();
    let restore: Changeset = serde_json::from_str(&serde_json::to_string(&restore).unwrap()).unwrap();
    restore.apply(&mut project).unwrap();
    assert_eq!(project, expected);
}

#[test]
fn malformed_scalar_payloads_cannot_enter_projects_or_changesets() {
    let (project, canvas, id) = setup();
    for raw in [
        r#"{"type":"bool","value":"true"}"#,
        r#"{"type":"integer","value":"9223372036854775808"}"#,
        r#"{"type":"unsigned","value":"-1"}"#,
        r#"{"type":"unsigned","value":18446744073709551615}"#,
        r#"{"type":"number","value":null}"#,
        r#"{"type":"number","value":1e400}"#,
        r#"{"type":"number","value":"NaN"}"#,
    ] {
        // Keep the raw numeric token: a JSON Value intermediary would normalize or reject it first.
        let mut item = Node::new(id, "test.scalars");
        item.params.insert("bad".into(), Value::Bool(false));
        let change = Changeset::new(vec![Change::SetNode { canvas, node: item }]);
        let json = serde_json::to_string(&change).unwrap().replace(r#"{"type":"bool","value":false}"#, raw);
        assert!(serde_json::from_str::<Changeset>(&json).is_err(), "{raw}");
        let mut saved = project.clone();
        Change::SetParam { canvas, id, name: "bad".into(), value: Some(Value::Bool(false)) }.apply(&mut saved).unwrap();
        let json = serde_json::to_string(&saved).unwrap().replace(r#"{"type":"bool","value":false}"#, raw);
        assert!(serde_json::from_str::<Project>(&json).is_err(), "{raw}");
    }
}

#[test]
fn scalar_values_obey_list_and_parameter_limits_atomically() {
    let (mut project, canvas, id) = setup();
    let mut item = Node::new(id, "test.scalars");
    for index in 0..256 {
        item.params.insert(format!("p{index}"), Value::Unsigned(Unsigned(u64::MAX)));
    }
    Change::SetNode { canvas, node: item }.apply(&mut project).unwrap();
    let before = project.clone();
    let mut deep = Value::Bool(true);
    for _ in 0..=MAX_VALUE_DEPTH {
        deep = Value::List(vec![deep]);
    }
    for (name, value) in [("overflow", Value::Integer(Integer(1))), ("p0", deep), ("p0", Value::List(vec![Value::Bool(true); MAX_VALUE_ITEMS]))] {
        let edits = Changeset::new(vec![
            Change::SetNodeTitle { canvas, id, title: "Uncommitted".into() },
            Change::SetParam { canvas, id, name: name.into(), value: Some(value) },
        ]);
        assert!(edits.apply(&mut project).is_err());
        assert_eq!(project, before);
    }
    Change::SetParam { canvas, id, name: "p0".into(), value: Some(Value::Bool(false)) }.apply(&mut project).unwrap();
}

#[test]
fn instance_config_is_defaulted_bounded_and_preserved_through_setnode_undo() {
    let (mut project, canvas, id) = setup();
    let before = project.clone();
    let mut node = Node::new(id, "test.scalars");
    node.config.insert("test.scalars.version".into(), Value::Unsigned(Unsigned(1)));
    node.config.insert("future.owner.v2.opaque".into(), Value::Text("preserve".into()));
    let undo = Change::SetNode { canvas, node: node.clone() }.apply(&mut project).unwrap();
    let encoded = serde_json::to_string(&project).unwrap();
    assert_eq!(serde_json::from_str::<Project>(&encoded).unwrap(), project);
    let redo = undo.apply(&mut project).unwrap();
    assert_eq!(project, before);
    redo.apply(&mut project).unwrap();
    assert_eq!(project.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap(), &node);
    let good = project.clone();
    let mut deep = Value::Bool(true);
    for _ in 0..=MAX_VALUE_DEPTH {
        deep = Value::List(vec![deep]);
    }
    node.config.insert("bad".into(), deep);
    assert!(Change::SetNode { canvas, node }.apply(&mut project).is_err());
    assert_eq!(project, good);
    let mut node = Node::new(id, "test.scalars");
    for index in 0..257 {
        node.config.insert(format!("p{index}"), Value::Bool(false));
    }
    assert!(Change::SetNode { canvas, node }.apply(&mut project).is_err());
    assert_eq!(project, good);
    let mut node = Node::new(id, "test.scalars");
    for index in 0..8 {
        node.config.insert(format!("p{index}"), Value::Text("x".repeat(1024 * 1024)));
    }
    assert!(Change::SetNode { canvas, node }.apply(&mut project).is_err());
    assert_eq!(project, good);
}
