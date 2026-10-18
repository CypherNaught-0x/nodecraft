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
        for field in [
            "canvas",
            "id",
            "pos",
            "node",
            "edge",
            "gesture",
            "title",
            "name",
            "value",
            "replace",
            "nodes",
            "edges",
            "location",
            "discard_dirty",
            "text",
            "reply",
            "resolved",
            "expected_revision",
        ] {
            let mut params = json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":1,"y":0}});
            params[field] = raw.clone();
            corpus.push(params);
        }
    }
    for raw in [Value::Null, json!("wrong"), json!({"type":"list","value":[null]}), json!({"type":"number","value":1e100})] {
        corpus.push(json!({"canvas":common::canvas(),"id":common::node_id(1),"name":"text","value":raw}));
        corpus.push(json!({"canvas":common::canvas(),"id":common::node_id(1),"title":raw}));
    }
    // edge.rewire and canvas.delete: duplicate, missing and oversized id lists.
    corpus.push(json!({"canvas":common::canvas(),"edge":common::edge(),"replace":[common::edge().id, common::edge().id]}));
    corpus.push(json!({"canvas":common::canvas(),"nodes":[common::node_id(1), common::node_id(1)],"edges":[common::edge().id]}));
    corpus.push(json!({"canvas":common::canvas(),"nodes":vec![common::node_id(9); 20_000]}));
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
    let root = nodecraft_types::CommentId::from_bytes([10; 16]);
    let reply = nodecraft_types::CommentId::from_bytes([11; 16]);
    if id.starts_with("comment.") {
        let mut comment = nodecraft_graph::Comment::new(root, "Original", nodecraft_graph::Position::default());
        comment.replies.push(nodecraft_graph::CommentReply { id: reply, text: "Reply".into() });
        nodecraft_graph::Change::PutComment { canvas: canvas(), comment }.apply(&mut document).unwrap();
    }
    let mut session = if id.starts_with("file.") {
        let mut storage = nodecraft_format::MemoryStorage::default();
        let snapshot = nodecraft_format::Snapshot { project: document.clone(), assets: Default::default(), epoch: 0 };
        nodecraft_format::ProjectStorage::save(
            &mut storage,
            &nodecraft_format::StorageLocation::from_command("template.ncraft".into()).unwrap(),
            &nodecraft_format::encode(&snapshot).unwrap(),
        )
        .unwrap();
        let mut session = Session::with_storage(snapshot, Box::new(storage)).unwrap();
        session.dispatch("file.save_as", &json!({"location":"template.ncraft"})).unwrap();
        session
    } else {
        Session::new(document).unwrap()
    };
    if matches!(id, "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" | "canvas.pan_to") {
        session.swap_resource(Some(nodecraft_engine::navigation_cmds::NavigationContext {
            canvas: canvas(),
            viewport_available: true,
            blocked: false,
            fit_all: true,
            fit_selection: true,
        }));
        session.register_host_service(nodecraft_engine::navigation_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    if id.starts_with("canvas.search_") {
        session.swap_resource(Some(nodecraft_engine::search_cmds::SearchContext {
            canvas: canvas(),
            viewport_available: true,
            blocked: false,
            open: true,
            has_query: true,
        }));
        session.register_host_service(nodecraft_engine::search_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    if matches!(id, "comment.open" | "comment.compose" | "canvas.comments_toggle") {
        session.swap_resource(Some(nodecraft_engine::comment_cmds::CommentContext {
            project: session.document().id(),
            canvas: canvas(),
            revision: session.revision(),
            blocked: false,
        }));
        session.register_host_service(nodecraft_engine::comment_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    let movement = json!({"canvas":canvas(),"id":node_id(1),"pos":{"x":12,"y":0}});
    let params = match id {
        "comment.create" => json!({"canvas":canvas(),"id":nodecraft_types::CommentId::from_bytes([20;16]),"text":"New","pos":{"x":1,"y":2}}),
        "comment.edit" => json!({"canvas":canvas(),"id":root,"text":"Edited"}),
        "comment.reply" => json!({"canvas":canvas(),"id":root,"reply":nodecraft_types::CommentId::from_bytes([12;16]),"text":"New reply"}),
        "comment.reply_remove" => json!({"canvas":canvas(),"id":root,"reply":reply}),
        "comment.move" => json!({"canvas":canvas(),"id":root,"pos":{"x":12,"y":24},"node":node_id(1)}),
        "comment.resolve" => json!({"canvas":canvas(),"id":root,"resolved":true}),
        "comment.remove" => json!({"canvas":canvas(),"id":root}),
        "comment.open" => json!({"id":root}),
        "comment.compose" => json!({"pos":{"x":12,"y":24}}),
        "canvas.comments_toggle" => json!({}),
        "node.add" => json!({"canvas":canvas(),"node":node(3)}),
        "node.remove" => json!({"canvas":canvas(),"id":node_id(1)}),
        "node.move" => movement.clone(),
        "node.title" => json!({"canvas":canvas(),"id":node_id(1),"title":"Renamed"}),
        "node.param_set" => json!({"canvas":canvas(),"id":node_id(3),"name":"text","value":null}),
        "edge.connect" => json!({"canvas":canvas(),"edge":edge()}),
        "edge.disconnect" => json!({"canvas":canvas(),"id":edge().id}),
        "canvas.stale" => json!({"canvas":canvas()}),
        "edge.rewire" => {
            let moved = nodecraft_graph::Edge { id: nodecraft_types::EdgeId::from_bytes([9; 16]), ..edge() };
            json!({"canvas":canvas(),"edge":moved,"replace":[edge().id]})
        }
        "canvas.paste" => json!({"canvas":canvas(),"nodes":[node(3)],"edges":[]}),
        "node.create" => json!({"canvas":canvas(),"id":node_id(7),"kind":"source.text","pos":{"x":1,"y":2}}),
        "canvas.delete" => json!({"canvas":canvas(),"nodes":[node_id(1)],"edges":[edge().id]}),
        "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" => json!({}),
        "canvas.search_open" | "canvas.search_next" | "canvas.search_previous" | "canvas.search_close" => json!({}),
        "canvas.search_query" => json!({"query":"needle"}),
        "canvas.pan_to" => json!({"x":0,"y":0}),
        "edit.undo" => {
            session.dispatch("node.move", &movement).unwrap();
            Value::Null
        }
        "edit.redo" => {
            session.dispatch("node.move", &movement).unwrap();
            session.dispatch("edit.undo", &Value::Null).unwrap();
            json!({})
        }
        // The keys service isn't registered here: dispatch must fail cleanly, never panic.
        "keys.set" => json!({"account":"openai","key":"sk-test-123456789"}),
        "keys.remove" | "keys.status" => json!({"account":"openai"}),
        "file.open" => json!({"location":"template.ncraft","discard_dirty":true}),
        "file.save" => Value::Null,
        "file.save_as" => json!({"location":"new.ncraft"}),
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
            let history = (session.can_undo(), session.can_redo());
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
                    if matches!(
                        id,
                        "canvas.fit_all"
                            | "canvas.fit_selection"
                            | "canvas.minimap_toggle"
                            | "canvas.pan_to"
                            | "canvas.comments_toggle"
                            | "comment.open"
                            | "comment.compose"
                            | "canvas.stale"
                            | "canvas.search_open"
                            | "canvas.search_query"
                            | "canvas.search_next"
                            | "canvas.search_previous"
                            | "canvas.search_close"
                    ) {
                        assert_eq!(session.document(), &before);
                        assert_eq!(session.revision(), revision);
                        assert_eq!(session.events_since(0).unwrap(), events);
                        assert_eq!((session.can_undo(), session.can_redo()), history);
                        continue;
                    }
                    let after = session.document().clone();
                    session.document().validate().unwrap();
                    if id == "canvas.paste" {
                        assert_ne!(after, before, "a successful paste is a document edit");
                        assert_eq!(session.revision(), revision + 1);
                    }
                    if id.starts_with("file.") {
                        assert_eq!(after, before);
                        continue;
                    }
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
