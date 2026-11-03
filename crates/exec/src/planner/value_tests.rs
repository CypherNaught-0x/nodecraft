//! What a node gives without running (#84), as a run reads it: a source's own current value,
//! never its history; anything else's usable active result.
use super::*;
use nodecraft_graph::Generation;
use nodecraft_types::{AssetRef, Blake3, GenerationId};

fn asset(n: u8) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() })
}

#[test]
fn a_source_gives_its_current_value_not_an_older_result() {
    let kinds = Registry::builtins().unwrap();
    let mut source =
        kinds.get("source.image").unwrap().instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &[("asset".into(), asset(5))].into()).unwrap();
    assert_eq!(value_without_running(&kinds, &source, "out"), Some(asset(5)));
    // It ran with an earlier picture, then its picture was replaced: the run sends the new one.
    let id = GenerationId::from_bytes([2; 16]);
    source.history.push(Generation::new(id, vec![asset(4)]));
    source.active = Some(id);
    assert_eq!(value_without_running(&kinds, &source, "out"), Some(asset(5)));
    assert_eq!(value_without_running(&kinds, &source, "missing"), None);
    let empty = kinds.get("source.image").unwrap().instantiate_instance(NodeId::from_bytes([3; 16]), Default::default(), &Default::default()).unwrap();
    assert_eq!(value_without_running(&kinds, &empty, "out"), None, "a source with nothing in it gives nothing");
}

#[test]
fn anything_else_gives_its_usable_active_result() {
    let kinds = Registry::builtins().unwrap();
    // A kind this registry doesn't know is read from its result, as planning reads it.
    let mut node = kinds.get("source.image").unwrap().instantiate_instance(NodeId::from_bytes([4; 16]), Default::default(), &Default::default()).unwrap();
    node.kind = "fixture.unknown".into();
    assert_eq!(value_without_running(&kinds, &node, "out"), None, "no result yet");
    let id = GenerationId::from_bytes([5; 16]);
    node.history.push(Generation::new(id, vec![asset(7)]));
    node.active = Some(id);
    assert_eq!(value_without_running(&kinds, &node, "out"), Some(asset(7)));
    node.history[0].outputs.push(asset(8));
    assert_eq!(value_without_running(&kinds, &node, "out"), None, "a result for another port layout is not used");
}
