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
        for field in ["canvas", "id", "pos", "node", "edge", "gesture"] {
            let mut params = json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":1,"y":0}});
            params[field] = raw.clone();
            corpus.push(params);
        }
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
