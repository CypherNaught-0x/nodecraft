mod common;
use common::*;
use nodecraft_engine::{
    Session,
    commands::{CommandSpec, Context, Effect, Registry, Resources},
};
use nodecraft_graph::{Change, Changeset, Comment, Position};
use nodecraft_types::{CommentId, Error, ErrorKind};
use serde_json::{Value, json};

fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}
fn service(context: &Context<'_>, _: &Value) -> Result<Effect, Error> {
    Ok(Effect::None { result: json!(context.resources.require::<String>()?) })
}
fn denied(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Err(Error::new(ErrorKind::Conflict, "custom precondition"))
}
fn move_params(id: u8, x: i32, gesture: &str) -> Value {
    json!({"canvas":canvas(),"id":node_id(id),"pos":{"x":x,"y":0},"gesture":gesture})
}
fn registry() -> Registry {
    let mut registry = Registry::builtins().unwrap();
    registry
        .register(CommandSpec {
            id: "test.service_1",
            label: "Service",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({"type":"null"}),
            params_doc: "null",
            enabled,
            run: service,
        })
        .unwrap();
    registry
}

#[test]
fn resources_and_non_document_effects_preserve_history_events_and_effective_gesture() {
    let mut resources = Resources::default();
    resources.insert(String::from("host result"));
    let mut session = Session::with_registry(project(), registry()).unwrap().with_resources(resources);
    session.dispatch("node.move", &move_params(1, 10, "a")).unwrap();
    let revision = session.revision();
    let events = session.events_since(0).unwrap();
    assert_eq!(session.dispatch("test.service_1", &Value::Null).unwrap(), json!("host result"));
    session.dispatch("node.move", &move_params(1, 10, "a")).unwrap();
    // A redundant move of another node is not an effective document edit either.
    session.dispatch("node.move", &move_params(2, 0, "b")).unwrap();
    assert_eq!(session.revision(), revision);
    assert_eq!(session.events_since(0).unwrap(), events);
    session.dispatch("node.move", &move_params(1, 20, "a")).unwrap();
    session.dispatch("edit.undo", &Value::Null).unwrap();
    assert_eq!(session.document(), &project());
    assert!(!session.can_undo());
    let before = session.document().clone();
    let revision = session.revision();
    session.dispatch("test.service_1", &Value::Null).unwrap();
    assert!(session.can_redo());
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    session.dispatch("edit.redo", &Value::Null).unwrap();
    assert_eq!(session.document().canvases().get(&canvas()).unwrap().nodes().get(&node_id(1)).unwrap().pos.x, 20.0);
    let mut no_host = Session::with_registry(project(), registry()).unwrap();
    assert_eq!(no_host.dispatch("test.service_1", &Value::Null).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn custom_enabled_preconditions_are_honored_during_dispatch() {
    let mut registry = registry();
    let spec = *registry.get("test.service_1").unwrap();
    registry.register(CommandSpec { id: "test.denied", enabled: denied, ..spec }).unwrap();
    let mut session = Session::with_registry(project(), registry).unwrap();
    assert_eq!(session.dispatch("test.denied", &Value::Null).unwrap_err().message, "custom precondition");
    assert_eq!(session.revision(), 0);
}

#[test]
fn safe_decode_errors_identify_field_and_category_without_echoing_values_or_keys() {
    let session = Session::new(project()).unwrap();
    for (params, path, category) in [
        (json!({"canvas":canvas(),"id":node_id(1),"pos":{"x":"secret-key-value","y":0}}), "params.pos.x", "wrong type"),
        (json!({"canvas":canvas(),"pos":{"x":0,"y":0}}), "params.id", "missing field"),
        (json!({"canvas":canvas(),"id":node_id(1),"pos":{"x":0,"y":0},"secret-key-value":true}), "params", "unknown field"),
    ] {
        let error = session.enabled("node.move", &params).unwrap_err();
        assert!(error.message.contains(path), "{}", error.message);
        assert!(error.message.contains(category), "{}", error.message);
        assert!(!error.message.contains("secret-key-value"));
    }
}

#[test]
fn caller_generation_history_and_invalid_noop_gestures_are_rejected() {
    let mut session = Session::new(project()).unwrap();
    let mut candidate = node(3);
    candidate.history.push(nodecraft_graph::Generation { id: nodecraft_types::GenerationId::from_bytes([4; 16]), outputs: vec![] });
    assert!(session.dispatch("node.add", &json!({"canvas":canvas(),"node":candidate})).is_err());
    assert!(session.dispatch("node.move", &move_params(1, 0, "")).is_err());
    assert_eq!(session.revision(), 0);
    assert!(!session.can_undo());
}

#[test]
fn effective_move_of_different_node_interrupts_coalescing() {
    let mut session = Session::new(project()).unwrap();
    for (id, x, g) in [(1, 10, "a"), (2, 10, "b"), (1, 20, "a")] {
        session.dispatch("node.move", &move_params(id, x, g)).unwrap();
    }
    session.dispatch("edit.undo", &Value::Null).unwrap();
    assert_eq!(session.document().canvases().get(&canvas()).unwrap().nodes().get(&node_id(1)).unwrap().pos.x, 10.0);
    assert!(session.can_undo());
}

fn replace_comment(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    Ok(Effect::Edit {
        changes: Changeset::new(vec![Change::PutComment {
            canvas: canvas(),
            comment: Comment {
                id: CommentId::from_bytes([4; 16]),
                text: value.as_str().unwrap().repeat(1024 * 1024),
                pos: Position::default(),
                node: None,
                replies: vec![],
                resolved: false,
            },
        }]),
        result: Value::Null,
        gesture: None,
    })
}
#[test]
fn aggregate_history_byte_budget_evicts_oldest_complete_steps() {
    let mut original = project();
    Change::PutComment {
        canvas: canvas(),
        comment: Comment {
            id: CommentId::from_bytes([4; 16]),
            text: "a".repeat(1024 * 1024),
            pos: Position::default(),
            node: None,
            replies: vec![],
            resolved: false,
        },
    }
    .apply(&mut original)
    .unwrap();
    let mut registry = registry();
    let spec = *registry.get("test.service_1").unwrap();
    registry.register(CommandSpec { id: "test.comment", run: replace_comment, ..spec }).unwrap();
    let mut session = Session::with_registry(original, registry).unwrap();
    for n in 0..17_u8 {
        session.dispatch("test.comment", &json!(char::from(b'b' + n).to_string())).unwrap();
    }
    let mut count = 0;
    while session.can_undo() {
        session.dispatch("edit.undo", &Value::Null).unwrap();
        count += 1;
    }
    assert_eq!(count, 15); // Each complete step is 1MiB plus metadata, so sixteen exceed16MiB.
    let text = &session.document().canvases().get(&canvas()).unwrap().comments().get(&CommentId::from_bytes([4; 16])).unwrap().text;
    assert!(text.starts_with('c'));
}

#[test]
fn all_builtin_schemas_declare_required_properties_and_shortcuts() {
    let session = Session::new(project()).unwrap();
    for spec in session.registry().specs() {
        let schema = (spec.input_schema)();
        assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
        if spec.id.starts_with("edit.") || spec.id == "file.save" {
            assert_eq!(schema["anyOf"][0]["type"], "null");
        } else {
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], false);
            for required in schema["required"].as_array().unwrap() {
                assert!(schema["properties"].get(required.as_str().unwrap()).is_some());
            }
        }
    }
    let registry = session.registry();
    assert_eq!((registry.get("node.add").unwrap().input_schema)()["properties"]["node"]["required"], json!(["id", "kind"]));
    assert!(registry.get("edit.redo").unwrap().alternate_shortcuts.contains(&"Ctrl+Y"));
    assert_eq!(registry.get("node.remove").unwrap().shortcut, None);
}

#[test]
fn invalid_service_result_preserves_existing_redo_and_events() {
    fn invalid_result(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
        Ok(Effect::None { result: json!("x".repeat(1024 * 1024 + 1)) })
    }
    let mut registry = registry();
    let spec = *registry.get("test.service_1").unwrap();
    registry.register(CommandSpec { id: "test.invalid_result", run: invalid_result, ..spec }).unwrap();
    let mut session = Session::with_registry(project(), registry).unwrap();
    session.dispatch("node.move", &move_params(1, 10, "a")).unwrap();
    session.dispatch("edit.undo", &Value::Null).unwrap();
    let events = session.events_since(0).unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    assert!(session.dispatch("test.invalid_result", &Value::Null).is_err());
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.events_since(0).unwrap(), events);
    assert!(session.can_redo());
    assert!(!session.can_undo());
}

#[test]
#[ignore = "manual release comparison of 1000-node chain move dispatch"]
fn move_dispatch_benchmark() {
    use nodecraft_graph::{Canvas, Edge, Endpoint};
    use nodecraft_types::{EdgeId, NodeId, ProjectId};
    use std::{hint::black_box, time::Instant};
    let mut document = nodecraft_graph::Project::new(ProjectId::from_bytes([0; 16]), "Benchmark").unwrap();
    let mut changes = vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "Benchmark") }];
    for n in 1..=1000_u128 {
        let mut item = node(1);
        item.id = NodeId::from_bytes(n.to_le_bytes());
        changes.push(Change::AddNode { canvas: canvas(), node: item });
        if n > 1 {
            changes.push(Change::Connect {
                canvas: canvas(),
                edge: Edge {
                    id: EdgeId::from_bytes(n.to_le_bytes()),
                    from: Endpoint { node: NodeId::from_bytes((n - 1).to_le_bytes()), port: "out".into() },
                    to: Endpoint { node: NodeId::from_bytes(n.to_le_bytes()), port: "in".into() },
                    order: 0,
                },
            });
        }
    }
    Changeset::new(changes).apply(&mut document).unwrap();
    let mut session = Session::new(document.clone()).unwrap();
    let now = Instant::now();
    for x in 1..=100 {
        session.dispatch("node.move", &json!({"canvas":canvas(),"id":NodeId::from_bytes(1_u128.to_le_bytes()),"pos":{"x":x,"y":0},"gesture":"drag"})).unwrap();
    }
    let staged = now.elapsed();
    black_box(session.document());
    let now = Instant::now();
    for x in 1..=100 {
        let changes =
            Changeset::new(vec![Change::MoveNode { canvas: canvas(), id: NodeId::from_bytes(1_u128.to_le_bytes()), pos: Position { x: x as f32, y: 0.0 } }]);
        changes.validate(&document).unwrap();
        let mut draft = document.clone();
        let inverse = changes.apply(&mut draft).unwrap();
        let mut undone = draft.clone();
        black_box(inverse.apply(&mut undone).unwrap());
        document = draft;
    }
    let legacy = now.elapsed();
    eprintln!("100 moves /1000nodes999edges: staged={staged:?}, legacy5clonepath={legacy:?}");
}

#[test]
fn explicit_host_service_effects_invoke_bounded_capabilities_without_document_edits() {
    use nodecraft_engine::commands::Services;
    use std::{cell::RefCell, rc::Rc};
    fn request(_: &Context<'_>, params: &Value) -> Result<Effect, Error> {
        Ok(Effect::Service { capability: "host.settings_set", request: params.clone() })
    }
    fn oversized_request(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
        Ok(Effect::Service { capability: "host.settings_set", request: json!("x".repeat(1024 * 1024 + 1)) })
    }
    let mut commands = registry();
    let spec = *commands.get("test.service_1").unwrap();
    commands.register(CommandSpec { id: "test.settings_set", run: request, ..spec }).unwrap();
    commands.register(CommandSpec { id: "test.oversized_request", run: oversized_request, ..spec }).unwrap();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let log = calls.clone();
    let mut services = Services::default();
    services
        .register("host.settings_set", move |request| {
            log.borrow_mut().push(request.clone());
            match request.get("mode").and_then(Value::as_str) {
                Some("error") => Err(Error::new(ErrorKind::Conflict, "host rejected operation")),
                Some("invalid_result") => Ok(json!("x".repeat(1024 * 1024 + 1))),
                Some("panic") => panic!("host callback failed"),
                _ => Ok(json!({"updated":true})),
            }
        })
        .unwrap();
    assert!(services.contains("host.settings_set"));
    let mut session = Session::with_registry(project(), commands).unwrap().with_services(services);
    session.dispatch("node.move", &move_params(1, 10, "a")).unwrap();
    session.dispatch("edit.undo", &Value::Null).unwrap();
    let before = session.document().clone();
    let events = session.events_since(0).unwrap();
    let revision = session.revision();
    assert_eq!(session.dispatch("test.settings_set", &json!({"zoom_speed":2})).unwrap(), json!({"updated":true}));
    assert_eq!(*calls.borrow(), vec![json!({"zoom_speed":2})]);
    assert!(session.dispatch("test.settings_set", &json!("x".repeat(1024 * 1024 + 1))).is_err());
    assert_eq!(calls.borrow().len(), 1);
    assert!(session.dispatch("test.oversized_request", &Value::Null).is_err());
    assert_eq!(calls.borrow().len(), 1);
    for (index, mode) in ["error", "invalid_result", "panic"].into_iter().enumerate() {
        let error = session.dispatch("test.settings_set", &json!({"mode":mode})).unwrap_err();
        let expected = match mode {
            "error" => ErrorKind::Conflict,
            "panic" => ErrorKind::Internal,
            _ => ErrorKind::InvalidInput,
        };
        assert_eq!(error.kind, expected);
        assert_eq!(calls.borrow().len(), index + 2); // At most once, even on host error/panic/invalid response.
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert!(session.can_redo());
    }
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.events_since(0).unwrap(), events);
    assert!(session.can_redo());
    let mut registry = registry();
    let spec = *registry.get("test.service_1").unwrap();
    registry.register(CommandSpec { id: "test.settings_set", run: request, ..spec }).unwrap();
    let mut missing = Session::with_registry(project(), registry).unwrap();
    assert_eq!(missing.dispatch("test.settings_set", &Value::Null).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(calls.borrow().len(), 4);
}
