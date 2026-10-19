//! Fanned results (#92): a generation tagged with the batch items it was made from.
use nodecraft_graph::{Canvas, Change, Changeset, Generation, ItemKey, MAX_ITEM_AXES, Node, Project};
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
