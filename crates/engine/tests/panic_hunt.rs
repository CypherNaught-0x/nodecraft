use nodecraft_nodes::NodeKind;
mod common;

use nodecraft_engine::Session;
use nodecraft_types::ErrorKind;
use serde_json::{Value, json};

#[test]
#[ignore = "adversarial command corpus; run explicitly when commands change"]
fn panic_hunt() {
    let original = common::project();
    let mut session = Session::new(original.clone()).unwrap();
    let ids: Vec<_> = session.registry().specs().map(|spec| spec.id).collect();
    let mut deep = Value::Null;
    for _ in 0..128 {
        deep = json!([deep]);
    }
    let mut corpus = vec![
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(u64::MAX),
        json!(1e100),
        json!("💥"),
        json!([]),
        json!({"unknown":true}),
        deep,
        json!("x".repeat(1024 * 1024 + 1)),
        json!(vec![Value::Null; 16_385]),
    ];
    // Exercise typed parameter decoding and graph validation, not only top-level JSON errors.
    for raw in [json!("not-an-id"), Value::Null, json!(u64::MAX), json!({}), json!([])] {
        for field in ["canvas", "id", "pos", "node", "edge", "gesture", "title", "name", "value"] {
            let mut params = json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":1,"y":0}});
            params[field] = raw.clone();
            corpus.push(params);
        }
    }
    for raw in [Value::Null, json!("wrong"), json!({"type":"list","value":[null]}), json!({"type":"number","value":1e100})] {
        corpus.push(json!({"canvas":common::canvas(),"id":common::node_id(1),"name":"text","value":raw}));
        corpus.push(json!({"canvas":common::canvas(),"id":common::node_id(1),"title":raw}));
    }
    for id in ids {
        for params in &corpus {
            let before = session.document().clone();
            let revision = session.revision();
            let enabled = session.enabled(id, params);
            let result = session.dispatch(id, params);
            assert!(!enabled.as_ref().is_err_and(|error| error.kind == ErrorKind::Internal), "enabled panicked for {id}");
            assert!(!result.as_ref().is_err_and(|error| error.kind == ErrorKind::Internal), "dispatch panicked for {id}");
            if result.is_err() {
                assert_eq!(session.document(), &before, "{id}");
                assert_eq!(session.revision(), revision, "{id}");
            } else {
                assert!(enabled.is_ok());
                session.document().validate().unwrap();
            }
        }
    }
}

fn template(id: &str) -> (Session, Value) {
    use common::*;
    let mut document = project();
    if id == "edge.connect" {
        nodecraft_graph::Change::Disconnect { canvas: canvas(), id: edge().id }.apply(&mut document).unwrap();
    }
    if id == "node.param_set" {
        let mut source = nodecraft_nodes::text::kind().descriptor().instantiate(node_id(3), &std::collections::BTreeMap::new()).unwrap();
        source.params.insert("text".into(), nodecraft_types::Value::Text("before".into()));
        nodecraft_graph::Change::AddNode { canvas: canvas(), node: source }.apply(&mut document).unwrap();
    }
    let mut session = Session::new(document).unwrap();
    let movement = json!({"canvas":canvas(),"id":node_id(1),"pos":{"x":12,"y":0}});
    let params = match id {
        "node.add" => json!({"canvas":canvas(),"node":node(3)}),
        "node.remove" => json!({"canvas":canvas(),"id":node_id(1)}),
        "node.move" => movement.clone(),
        "node.title" => json!({"canvas":canvas(),"id":node_id(1),"title":"Renamed"}),
        "node.param_set" => json!({"canvas":canvas(),"id":node_id(3),"name":"text","value":null}),
        "edge.connect" => json!({"canvas":canvas(),"edge":edge()}),
        "edge.disconnect" => json!({"canvas":canvas(),"id":edge().id}),
        "edit.undo" => {
            session.dispatch("node.move", &movement).unwrap();
            Value::Null
        }
        "edit.redo" => {
            session.dispatch("node.move", &movement).unwrap();
            session.dispatch("edit.undo", &Value::Null).unwrap();
            json!({})
        }
        _ => panic!("missing valid template for {id}"),
    };
    (session, params)
}

#[test]
#[ignore = "valid command templates with individual field mutations"]
fn typed_template_panic_hunt() {
    let ids: Vec<_> = Session::new(common::project()).unwrap().registry().specs().map(|spec| spec.id).collect();
    let mut successes = 0;
    for id in ids {
        let (_, valid) = template(id);
        let mut cases = vec![valid.clone()];
        if let Some(object) = valid.as_object() {
            for key in object.keys() {
                for raw in [Value::Null, json!(true), json!("secret-do-not-echo"), json!(u64::MAX), json!([]), json!({})] {
                    let mut mutated = valid.clone();
                    mutated[key] = raw;
                    cases.push(mutated);
                }
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(key);
                cases.push(missing);
            }
        }
        for path in ["/pos/x", "/pos/y", "/node/id", "/node/kind", "/node/pos/x", "/edge/from/node", "/edge/from/port", "/edge/to/node", "/edge/order"] {
            if valid.pointer(path).is_some() {
                for raw in [Value::Null, json!("secret-do-not-echo"), json!(1e100), json!([])] {
                    let mut mutated = valid.clone();
                    *mutated.pointer_mut(path).unwrap() = raw;
                    cases.push(mutated);
                }
            }
        }
        for params in cases {
            let (mut session, _) = template(id);
            let before = session.document().clone();
            let revision = session.revision();
            let events = session.events_since(0).unwrap();
            let result = session.dispatch(id, &params);
            match result {
                Err(error) => {
                    assert_ne!(error.kind, ErrorKind::Internal, "{id}");
                    assert!(!error.message.contains("secret-do-not-echo"));
                    assert_eq!(session.document(), &before);
                    assert_eq!(session.revision(), revision);
                    assert_eq!(session.events_since(0).unwrap(), events);
                }
                Ok(_) => {
                    successes += 1;
                    let after = session.document().clone();
                    session.document().validate().unwrap();
                    let (reverse, again) = if id == "edit.undo" { ("edit.redo", "edit.undo") } else { ("edit.undo", "edit.redo") };
                    session.dispatch(reverse, &Value::Null).unwrap();
                    assert_eq!(session.document(), &before, "{id}");
                    session.dispatch(again, &Value::Null).unwrap();
                    assert_eq!(session.document(), &after, "{id}");
                }
            }
        }
    }
    assert!(successes >= 7);
}
