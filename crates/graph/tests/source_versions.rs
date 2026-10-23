use nodecraft_graph::{Generation, ItemKey, MAX_SOURCE_VERSIONS, SourceVersion};
use nodecraft_types::{Blake3, GenerationId, ItemId, NodeId, Value};

#[test]
fn source_versions_are_bounded_unique_and_belong_to_their_result_item() {
    let mut generation = Generation::new(GenerationId::from_bytes([1; 16]), vec![]);
    let source = SourceVersion { role: "clip".into(), hash: Blake3::from_bytes([2; 32]), item: None };
    generation.source_versions.push(source.clone());
    generation.validate().unwrap();
    generation.source_versions.push(source.clone());
    assert!(generation.validate().is_err(), "no ambiguous duplicate clip versions");
    generation.source_versions = vec![SourceVersion { role: "x".repeat(65), ..source.clone() }];
    assert!(generation.validate().is_err());
    generation.source_versions = vec![SourceVersion { item: Some(0), ..source.clone() }];
    assert!(generation.validate().is_err(), "ordinary result has no item zero");
    generation.items = vec![vec![ItemKey { batch: NodeId::from_bytes([3; 16]), item: ItemId::from_bytes([4; 16]) }]];
    generation.outputs = vec![Value::List(vec![Value::Text("result".into())])];
    generation.validate().unwrap();
    generation.source_versions = vec![source.clone()];
    assert!(generation.validate().is_err(), "fanned versions name their item");
    generation.source_versions = vec![source; MAX_SOURCE_VERSIONS + 1];
    let refusal = generation.validate().unwrap_err();
    assert!(refusal.message.contains("render fewer batch items or Timeline clips"), "count refused before inspecting/serializing records, with a remedy");
}
