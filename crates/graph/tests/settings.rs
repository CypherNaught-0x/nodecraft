//! Recorded job settings (#162): what each provider job of a result was submitted with.
use nodecraft_graph::{Canvas, Change, Changeset, Generation, ItemKey, JobSettings, Node, Project};
use nodecraft_types::{CanvasId, GenerationId, ItemId, NodeId, ProjectId, Unsigned, Value};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);

fn record(generation: Generation) -> Result<Project, nodecraft_types::Error> {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Settings").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }, Change::AddNode { canvas: C, node: Node::new(N, "test.settings") }])
        .apply(&mut project)
        .unwrap();
    Change::AddGeneration { canvas: C, id: N, index: 0, generation }.apply(&mut project)?;
    Ok(project)
}

fn job(item: Option<u32>) -> JobSettings {
    JobSettings { provider: "mock".into(), instance: None, model: "remote-v2".into(), params: [("seed".into(), Value::Unsigned(Unsigned(42)))].into(), item }
}

fn with(settings: Vec<JobSettings>) -> Generation {
    Generation { settings, ..Generation::new(GenerationId::from_bytes([3; 16]), vec![Value::Text("x".into())]) }
}

#[test]
fn settings_round_trip_and_results_without_them_keep_their_shape() {
    let project = record(with(vec![job(None)])).unwrap();
    let back: Project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(back, project);
    let node = back.canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    assert_eq!(node.history.first().unwrap().settings, vec![job(None)]);

    // No settings, no field: files written before them stay byte-identical.
    let plain = serde_json::to_value(Generation::new(GenerationId::from_bytes([4; 16]), vec![])).unwrap();
    assert!(plain.get("settings").is_none());
    // A file without the field reads as no settings.
    let old: Generation = serde_json::from_value(serde_json::json!({"id": GenerationId::from_bytes([5; 16]), "outputs": []})).unwrap();
    assert!(old.settings.is_empty());
}

#[test]
fn malformed_settings_are_rejected() {
    // An item index must name one of the result's items.
    assert!(record(with(vec![job(Some(0))])).is_err(), "an unfanned result has no items");
    let key = ItemKey { batch: NodeId::from_bytes([7; 16]), item: ItemId::from_bytes([8; 16]) };
    let fanned = |item| Generation {
        items: vec![vec![key]],
        settings: vec![job(Some(item))],
        ..Generation::new(GenerationId::from_bytes([3; 16]), vec![Value::List(vec![Value::Text("x".into())])])
    };
    assert!(record(fanned(0)).is_ok());
    assert!(record(fanned(1)).is_err());
    // A provider and a model are required; names and values stay within the document's limits.
    assert!(record(with(vec![JobSettings { provider: String::new(), ..job(None) }])).is_err());
    assert!(record(with(vec![JobSettings { model: String::new(), ..job(None) }])).is_err());
    assert!(record(with(vec![JobSettings { model: "m".repeat(4097), ..job(None) }])).is_err());
    let deep = (0..40).fold(Value::Text("x".into()), |value, _| Value::List(vec![value]));
    assert!(record(with(vec![JobSettings { params: [("p".into(), deep)].into(), ..job(None) }])).is_err());
    let many = (0..257).map(|n| (format!("p{n}"), Value::Bool(true))).collect();
    assert!(record(with(vec![JobSettings { params: many, ..job(None) }])).is_err());
}
