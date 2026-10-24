//! Fanned results (#92): a generation tagged with the batch items it was made from.
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Incomplete, ItemKey, MAX_ITEM_AXES, MAX_SENT_KEY, Missing, MissingItem, Node, Project, SentKey};
use nodecraft_types::{CanvasId, GenerationId, ItemId, NodeId, ProjectId, Value};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);

fn project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Items").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }, Change::AddNode { canvas: C, node: Node::new(N, "test.items") }])
        .apply(&mut project)
        .unwrap();
    project
}

fn key(batch: u8, item: u8) -> ItemKey {
    ItemKey { batch: NodeId::from_bytes([batch; 16]), item: ItemId::from_bytes([item; 16]) }
}

fn record(generation: Generation) -> Result<Project, nodecraft_types::Error> {
    let mut project = project();
    Change::AddGeneration { canvas: C, id: N, index: 0, generation }.apply(&mut project)?;
    Ok(project)
}

fn fanned(items: Vec<Vec<ItemKey>>, outputs: Vec<Value>) -> Generation {
    Generation { items, ..Generation::new(GenerationId::from_bytes([3; 16]), outputs) }
}

fn list(n: usize) -> Value {
    Value::List((0..n).map(|i| Value::Text(i.to_string())).collect())
}

#[test]
fn a_fanned_result_round_trips_and_plain_results_keep_their_shape() {
    let generation = fanned(vec![vec![key(1, 1), key(2, 1)], vec![key(1, 2), key(2, 1)]], vec![list(2)]);
    let project = record(generation.clone()).unwrap();
    let json = serde_json::to_string(&project).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, project);
    let node = back.canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    assert_eq!(node.history.first().unwrap().items, generation.items);

    // A result that isn't fanned writes no items field, so older files stay byte-identical.
    let plain = serde_json::to_value(Generation::new(GenerationId::from_bytes([4; 16]), vec![Value::Text("x".into())])).unwrap();
    assert!(plain.get("items").is_none());
}

#[test]
fn malformed_item_tags_are_rejected() {
    // Every output needs one value per item.
    assert!(record(fanned(vec![vec![key(1, 1)], vec![key(1, 2)]], vec![list(3)])).is_err());
    assert!(record(fanned(vec![vec![key(1, 1)]], vec![Value::Text("x".into())])).is_err());
    // Tags are non-empty and name each batch once, in order.
    assert!(record(fanned(vec![vec![]], vec![list(1)])).is_err());
    assert!(record(fanned(vec![vec![key(2, 1), key(1, 1)]], vec![list(1)])).is_err());
    assert!(record(fanned(vec![vec![key(1, 1), key(1, 2)]], vec![list(1)])).is_err());
    let wide: Vec<ItemKey> = (0..=u8::try_from(MAX_ITEM_AXES).unwrap()).map(|b| key(b, 1)).collect();
    assert!(record(fanned(vec![wide], vec![list(1)])).is_err());
    let widest: Vec<ItemKey> = (0..u8::try_from(MAX_ITEM_AXES).unwrap()).map(|b| key(b, 1)).collect();
    assert!(record(fanned(vec![widest], vec![list(1)])).is_ok());
}

#[test]
fn a_partial_result_names_some_of_its_items_once_each_in_order() {
    let partial = |missing: &[u32]| {
        let items = vec![vec![key(1, 1)], vec![key(1, 2)], vec![key(1, 3)]];
        let missing = missing.iter().map(|item| MissingItem::new(*item, Missing::Failed)).collect();
        Generation { incomplete: Some(Incomplete { missing }), ..fanned(items, vec![list(3)]) }
    };
    let project = record(partial(&[0, 2])).unwrap();
    let back: Project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(back, project);
    assert!(back.canvases().get(&C).unwrap().nodes().get(&N).unwrap().history.first().unwrap().is_partial());
    // Not none, not all, none out of range, none twice, in order.
    for bad in [&[][..], &[0, 1, 2], &[3], &[1, 1], &[2, 0]] {
        assert!(record(partial(bad)).is_err(), "{bad:?}");
    }
    // Only a fanned result can be partial, and a complete one writes no field.
    let plain = Generation::new(GenerationId::from_bytes([4; 16]), vec![Value::Text("x".into())]);
    assert!(serde_json::to_value(&plain).unwrap().get("incomplete").is_none());
    let missing = vec![MissingItem::new(0, Missing::Cancelled)];
    assert!(record(Generation { incomplete: Some(Incomplete { missing }), ..plain }).is_err());
}

#[test]
fn a_failed_item_keeps_the_keys_its_jobs_went_under() {
    let partial = |why: Missing, keys: Vec<(u32, String)>| {
        let items = vec![vec![key(1, 1)], vec![key(1, 2)]];
        let keys = keys.into_iter().map(|(job, key)| SentKey { job, key }).collect();
        let missing = vec![MissingItem { keys, ..MissingItem::new(1, why) }];
        Generation { incomplete: Some(Incomplete { missing }), ..fanned(items, vec![list(2)]) }
    };
    let sent = |job: u32| (job, format!("nodecraft:{:032x}:{job}", 7));
    let project = record(partial(Missing::Failed, vec![sent(0), sent(1)])).unwrap();
    let json = serde_json::to_string(&project).unwrap();
    assert!(json.contains("nodecraft:"), "{json}");
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, project);
    // An item without keys writes no field, so files stay as older builds wrote them.
    let none = serde_json::to_string(&record(partial(Missing::Failed, vec![])).unwrap()).unwrap();
    assert!(!none.contains("\"keys\""), "{none}");

    // Only a failed item's, in job order, once each, each a short visible key of NodeCraft's.
    let long = (0, format!("nodecraft:{}", "a".repeat(MAX_SENT_KEY)));
    for bad in [
        partial(Missing::Cancelled, vec![sent(0)]),
        partial(Missing::Failed, vec![sent(1), sent(0)]),
        partial(Missing::Failed, vec![sent(0), sent(0)]),
        partial(Missing::Failed, vec![(0, String::new())]),
        partial(Missing::Failed, vec![(0, "fal:123".into())]),
        partial(Missing::Failed, vec![(0, "nodecraft:a b".into())]),
        partial(Missing::Failed, vec![long]),
    ] {
        assert!(record(bad.clone()).is_err(), "{bad:?}");
    }
}
