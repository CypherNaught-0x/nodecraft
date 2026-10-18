mod common;

use common::*;
use nodecraft_engine::{
    Session,
    commands::{CommandSpec, Context, Effect, Registry},
    session::{MAX_EVENTS, MAX_HISTORY_STEPS},
};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

fn moved(x: i32, gesture: Option<&str>) -> Value {
    json!({"canvas": canvas(), "id": node_id(1), "pos": {"x": x, "y": 0}, "gesture": gesture})
}
fn x(session: &Session) -> f32 {
    session.document().canvases().get(&canvas()).unwrap().nodes().get(&node_id(1)).unwrap().pos.x
}

#[test]
fn every_builtin_edit_undoes_and_redoes_exactly() {
    let original = project();
    for (id, params) in [
        ("node.add", json!({"canvas":canvas(), "node":node(3)})),
        ("node.remove", json!({"canvas":canvas(), "id":node_id(1)})),
        ("node.move", moved(42, None)),
        ("edge.disconnect", json!({"canvas":canvas(), "id":edge().id})),
    ] {
        let mut session = Session::new(original.clone()).unwrap();
        session.enabled(id, &params).unwrap();
        session.dispatch(id, &params).unwrap();
        let edited = session.document().clone();
        assert_ne!(edited, original);
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document(), &original, "{id}");
        session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(session.document(), &edited, "{id}");
        let events = session.events_since(0).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events.last().unwrap().command, "edit.redo");
    }
    let mut disconnected = original;
    Change::Disconnect { canvas: canvas(), id: edge().id }.apply(&mut disconnected).unwrap();
    let mut session = Session::new(disconnected.clone()).unwrap();
    assert_eq!(session.dispatch("edge.connect", &json!({"canvas":canvas(), "edge":edge()})).unwrap(), json!({"id":edge().id}));
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &disconnected);
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document(), &project());
}

#[test]
fn disabled_bad_params_and_unknown_commands_preserve_state() {
    let mut session = Session::new(project()).unwrap();
    for (id, params) in [
        ("edit.undo", json!({})),
        ("edit.redo", json!({})),
        ("missing.command", json!({})),
        ("node.remove", json!({"canvas":canvas(), "id":node_id(99)})),
        ("node.add", json!({"canvas":canvas(), "node":node(1)})),
        ("node.move", json!({"canvas":canvas(), "id":node_id(1), "pos":{"x":1e100,"y":0}})),
        ("node.move", moved(3, Some(""))),
        ("node.move", moved(3, Some(&"x".repeat(129)))),
        ("node.move", json!({"secret_key":"do-not-echo"})),
    ] {
        assert!(session.enabled(id, &params).is_err(), "{id}");
        let error = session.dispatch(id, &params).unwrap_err();
        assert!(!error.message.contains("do-not-echo"));
        assert_eq!(session.document(), &project());
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo());
        assert!(session.events_since(0).unwrap().is_empty());
    }
}

#[test]
fn drag_coalescing_retains_first_inverse_and_latest_redo() {
    let mut session = Session::new(project()).unwrap();
    for value in [10, 20, 30] {
        session.dispatch("node.move", &moved(value, Some("drag-one"))).unwrap();
    }
    assert_eq!(x(&session), 30.0);
    assert_eq!(session.events_since(0).unwrap().len(), 3);
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(x(&session), 0.0);
    assert!(!session.can_undo());
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(x(&session), 30.0);
    // Undo/redo breaks the gesture even if a caller incorrectly reuses its old token.
    session.dispatch("node.move", &moved(40, Some("drag-one"))).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(x(&session), 30.0);
    // A new edit after undo clears redo.
    session.dispatch("node.move", &moved(50, Some("drag-two"))).unwrap();
    assert!(!session.can_redo());
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(x(&session), 30.0);
}

#[test]
fn unrelated_edits_nodes_and_omitted_gestures_never_coalesce() {
    let mut session = Session::new(project()).unwrap();
    session.dispatch("node.move", &moved(10, Some("drag"))).unwrap();
    session.dispatch("node.add", &json!({"canvas":canvas(),"node":node(3)})).unwrap();
    session.dispatch("node.move", &moved(20, Some("drag"))).unwrap();
    session.dispatch("node.move", &json!({"canvas":canvas(),"id":node_id(2),"pos":{"x":20,"y":0},"gesture":"drag"})).unwrap();
    for value in [30, 40] {
        session.dispatch("node.move", &moved(value, None)).unwrap();
    }
    for _ in 0..6 {
        session.dispatch("edit.undo", &json!({})).unwrap();
    }
    assert!(!session.can_undo());
    assert_eq!(session.document(), &project());
}

#[test]
fn bounded_event_log_supports_independent_cursors_and_explicit_resync() {
    let mut session = Session::new(project()).unwrap();
    for i in 1..=MAX_EVENTS + 1 {
        session.dispatch("node.move", &moved(i as i32, Some("drag"))).unwrap();
    }
    assert_eq!(session.events_since(0).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(session.events_since(1).unwrap().len(), MAX_EVENTS);
    assert_eq!(session.events_since(session.revision() - 1).unwrap().len(), 1);
    assert!(session.events_since(session.revision()).unwrap().is_empty());
    assert!(session.events_since(session.revision() + 1).is_err());
    assert_eq!(session.events_since(1).unwrap().len(), MAX_EVENTS); // Reading never drains another subscriber.
}

#[test]
fn undo_history_has_a_bounded_horizon() {
    let mut session = Session::new(project()).unwrap();
    for i in 1..=MAX_HISTORY_STEPS + 2 {
        session.dispatch("node.move", &moved(i as i32, None)).unwrap();
    }
    for _ in 0..MAX_HISTORY_STEPS {
        session.dispatch("edit.undo", &json!({})).unwrap();
    }
    assert_eq!(x(&session), 2.0);
    assert!(!session.can_undo());
    for _ in 0..MAX_HISTORY_STEPS {
        session.dispatch("edit.redo", &json!({})).unwrap();
    }
    assert_eq!(x(&session), (MAX_HISTORY_STEPS + 2) as f32);
}

fn always(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}
fn fail_after_local_edit(context: &Context<'_>, _: &Value) -> Result<Effect, Error> {
    let mut local = context.document.clone();
    Change::SetProjectTitle { title: "Never published".into() }.apply(&mut local)?;
    panic!("simulated handler failure");
}
fn invalid_suffix(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
    Ok(Effect::Edit {
        changes: Changeset::new(vec![
            Change::MoveNode { canvas: canvas(), id: node_id(1), pos: Position { x: 9.0, y: 0.0 } },
            Change::RemoveNode { canvas: canvas(), id: node_id(99) },
        ]),
        result: Value::Null,
        gesture: None,
    })
}

#[test]
fn panicking_handlers_and_atomic_graph_failures_preserve_document_history_and_events() {
    let mut registry = Registry::builtins().unwrap();
    for (id, run) in [("test.panic", fail_after_local_edit as fn(&Context<'_>, &Value) -> Result<Effect, Error>), ("test.invalid", invalid_suffix)] {
        registry
            .register(CommandSpec {
                id,
                alternate_shortcuts: &[],
                input_schema: || json!({"type":"object"}),
                label: "Test",
                shortcut: None,
                params_doc: "{}",
                enabled: always,
                run,
            })
            .unwrap();
    }
    let mut session = Session::with_registry(project(), registry).unwrap();
    session.dispatch("node.move", &moved(10, None)).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    let before = session.document().clone();
    let events = session.events_since(0).unwrap();
    for id in ["test.panic", "test.invalid"] {
        assert!(session.dispatch(id, &json!({})).is_err());
        assert_eq!(session.document(), &before);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert!(session.can_redo());
        assert!(!session.can_undo());
    }
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(x(&session), 10.0);
}

#[test]
fn registration_rejects_invalid_or_duplicate_stable_ids() {
    let mut registry = Registry::builtins().unwrap();
    let navigation = ["canvas.fit_all", "canvas.fit_selection", "canvas.minimap_toggle", "canvas.pan_to"];
    assert_eq!(
        nodecraft_engine::navigation_cmds::specs().into_iter().map(|spec| spec.id).collect::<std::collections::BTreeSet<_>>(),
        navigation.into_iter().collect()
    );
    let registered: std::collections::BTreeSet<_> = registry.specs().filter(|spec| navigation.contains(&spec.id)).map(|spec| spec.id).collect();
    assert_eq!(registered, navigation.into_iter().collect());
    for id in navigation {
        let spec = *registry.get(id).unwrap();
        assert_eq!(registry.register(spec).unwrap_err().kind, ErrorKind::Conflict);
    }
    let builtin_ids: std::collections::BTreeSet<_> = registry.specs().map(|spec| spec.id).collect();
    let spec = *registry.get("node.move").unwrap();
    assert_eq!(registry.register(spec).unwrap_err().kind, ErrorKind::Conflict);
    for id in ["nodot", ".empty", "Upper.case", "two..dots", "with space.name"] {
        assert_eq!(registry.register(CommandSpec { id, ..spec }).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    assert_eq!(
        registry.specs().count(),
        45,
        "7 graph, 2 node editing, 3 edge, node.create, node.create_connected, canvas.paste, 3 keys, canvas.stale, 4 navigation, 5 search, 3 file, 4 run/usage, 6 history and 4 perf commands"
    );
    assert_eq!(registry.specs().map(|spec| spec.id).collect::<std::collections::BTreeSet<_>>(), builtin_ids);
}

fn oversized_redo(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
    let changes = (0..17_u8)
        .map(|n| Change::PutComment {
            canvas: canvas(),
            comment: nodecraft_graph::Comment { id: nodecraft_types::CommentId::from_bytes([n; 16]), text: "x".repeat(1024 * 1024), pos: Position::default() },
        })
        .collect();
    Ok(Effect::Edit { changes: Changeset::new(changes), result: Value::Null, gesture: None })
}

#[test]
fn edits_with_small_undo_but_oversized_redo_are_rejected_atomically() {
    let mut registry = Registry::builtins().unwrap();
    registry
        .register(CommandSpec {
            id: "test.large",
            alternate_shortcuts: &[],
            input_schema: || json!({"type":"object"}),
            label: "Large",
            shortcut: None,
            params_doc: "{}",
            enabled: always,
            run: oversized_redo,
        })
        .unwrap();
    let mut session = Session::with_registry(project(), registry).unwrap();
    assert!(session.dispatch("test.large", &json!({})).is_err());
    assert_eq!(session.document(), &project());
    assert!(!session.can_undo());
    assert_eq!(session.revision(), 0);
}

#[test]
fn keys_commands_go_through_the_host_service_and_never_echo_keys() {
    use nodecraft_engine::commands::Services;
    use nodecraft_engine::secrets::{MemoryStore, register_key_service};
    use std::sync::Arc;
    let mut services = Services::default();
    register_key_service(&mut services, Arc::new(MemoryStore::default())).unwrap();
    let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([3; 16]), "Keys").unwrap();
    let mut session = Session::new(project).unwrap().with_services(services);
    let set = session.dispatch("keys.set", &serde_json::json!({"account": "anthropic", "key": "sk-ant-abcdefghijk9876"})).unwrap();
    assert_eq!(set, serde_json::json!({"stored": true, "masked": "…9876"}));
    let status = session.dispatch("keys.status", &serde_json::json!({"account": "anthropic"})).unwrap();
    assert_eq!(status["stored"], true);
    assert!(!session.can_undo(), "keys never enter the document or its history");
    let err = session.dispatch("keys.set", &serde_json::json!({"account": "Bad Name", "key": "sk-leak-me-123456"})).unwrap_err();
    assert!(!err.to_string().contains("sk-leak"), "{err}");
    assert!(session.dispatch("keys.set", &serde_json::json!({"account": "openai"})).is_err(), "the key is required");
    assert_eq!(session.dispatch("keys.remove", &serde_json::json!({"account": "anthropic"})).unwrap(), serde_json::json!({"removed": true}));
}
