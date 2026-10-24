use crate::common;

use common::*;
use nodecraft_engine::{
    Session,
    commands::{CommandSpec, Context, Effect, Registry},
};
use nodecraft_graph::{Change, Changeset, Generation};
use nodecraft_types::{Error, GenerationId};
use serde_json::{Value, json};

fn generation(n: u8) -> Generation {
    Generation {
        id: GenerationId::from_bytes([n; 16]),
        outputs: vec![nodecraft_types::Value::Text(format!("output {n}"))],
        input_hash: None,
        items: Vec::new(),
        settings: Vec::new(),
        source_versions: Vec::new(),
        incomplete: None,
    }
}
fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}
fn edit(_: &Context<'_>, params: &Value) -> Result<Effect, Error> {
    let changes: Changeset = serde_json::from_value(params.clone()).unwrap();
    Ok(Effect::Edit { changes, result: Value::Null, gesture: None })
}
fn session() -> Session {
    let mut document = project();
    for (index, n) in [1, 3].into_iter().enumerate() {
        Change::AddGeneration { canvas: canvas(), id: node_id(1), index, generation: generation(n) }.apply(&mut document).unwrap();
    }
    Change::SetActiveGeneration { canvas: canvas(), id: node_id(1), generation: Some(generation(1).id) }.apply(&mut document).unwrap();
    let mut registry = Registry::builtins().unwrap();
    registry
        .register(CommandSpec {
            id: "test.generations",
            alternate_shortcuts: &[],
            input_schema: || json!({"type":"object"}),
            label: "Generation edits",
            shortcut: None,
            params_doc: "Changeset",
            enabled,
            run: edit,
        })
        .unwrap();
    Session::with_registry(document, registry).unwrap()
}
fn dispatch(session: &mut Session, changes: Vec<Change>) -> Result<Value, Error> {
    session.dispatch("test.generations", &serde_json::to_value(Changeset::new(changes)).unwrap())
}

#[test]
fn granular_generation_transactions_preserve_order_and_selection_through_undo_redo() {
    let mut session = session();
    let original = session.document().clone();
    dispatch(
        &mut session,
        vec![
            Change::AddGeneration { canvas: canvas(), id: node_id(1), index: 1, generation: generation(2) },
            Change::SetActiveGeneration { canvas: canvas(), id: node_id(1), generation: Some(generation(2).id) },
            Change::RemoveGeneration { canvas: canvas(), id: node_id(1), generation: generation(1).id },
        ],
    )
    .unwrap();
    let edited = session.document().clone();
    let node = edited.canvases().get(&canvas()).unwrap().nodes().get(&node_id(1)).unwrap();
    assert_eq!(node.history, vec![generation(2), generation(3)]);
    assert_eq!(node.active, Some(generation(2).id));
    for _ in 0..2 {
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document(), &original);
        assert!(!session.can_undo());
        session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(session.document(), &edited);
    }
    assert_eq!(session.events_since(0).unwrap().len(), 5);
}

#[test]
fn failed_generation_suffix_preserves_document_redo_and_events() {
    let mut session = session();
    dispatch(&mut session, vec![Change::AddGeneration { canvas: canvas(), id: node_id(1), index: 1, generation: generation(2) }]).unwrap();
    let edited = session.document().clone();
    session.dispatch("edit.undo", &json!({})).unwrap();
    let before = session.document().clone();
    let events = session.events_since(0).unwrap();
    for suffix in [
        Change::RemoveGeneration { canvas: canvas(), id: node_id(1), generation: generation(1).id },
        Change::SetActiveGeneration { canvas: canvas(), id: node_id(1), generation: Some(generation(99).id) },
        Change::AddGeneration { canvas: canvas(), id: node_id(1), index: 99, generation: generation(4) },
    ] {
        assert!(
            dispatch(&mut session, vec![Change::AddGeneration { canvas: canvas(), id: node_id(1), index: 1, generation: generation(2) }, suffix,]).is_err()
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(session.revision(), 2);
        assert!(!session.can_undo());
        assert!(session.can_redo());
    }
    session.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(session.document(), &edited);
}
