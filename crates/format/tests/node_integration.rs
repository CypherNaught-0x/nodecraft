//! Reconciliation of the reviewed archive implementation with instance config/scalar nodes.
use nodecraft_format::{AssetStore, Snapshot, assets_in_changes, decode, encode, journal};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{AssetRef, CanvasId, Integer, MediaMeta, NodeId, Number, ProjectId, Unsigned, Value};
use std::collections::BTreeSet;
#[test]
fn scalar_values_and_config_only_nested_media_survive_archive_and_recovery() {
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Config").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(b"config-only public fixture").unwrap();
    let media = Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() });
    let mut node = Node::new(NodeId::from_bytes([3; 16]), "test.config");
    node.config.insert("test.v1.asset".into(), Value::List(vec![media]));
    for (name, value) in [
        ("bool", Value::Bool(true)),
        ("signed", Value::Integer(Integer(i64::MIN))),
        ("unsigned", Value::Unsigned(Unsigned(u64::MAX))),
        ("number", Value::Number(Number::new(0.1).unwrap())),
    ] {
        node.params.insert(name.into(), value);
    }
    let before = Snapshot { project, assets, epoch: 0 };
    let checkpoint = journal::checkpoint(&before).unwrap();
    assert!(!checkpoint.assets.contains(&hash));
    let changes = Changeset::new(vec![Change::AddNode { canvas, node: node.clone() }]);
    let mut after = before.clone();
    changes.apply(&mut after.project).unwrap();
    assert_eq!(after.assets.validate_project(&after.project).unwrap(), BTreeSet::from([hash]));
    assert_eq!(assets_in_changes(&changes).unwrap(), BTreeSet::from([hash]));
    assert_eq!(assets_in_changes(&Changeset::new(vec![Change::SetNode { canvas, node: node.clone() }])).unwrap(), BTreeSet::from([hash]));
    assert_eq!(assets_in_changes(&Changeset::new(vec![Change::RestoreNode { canvas, node, edges: vec![] }])).unwrap(), BTreeSet::from([hash]));
    let loaded = decode(&encode(&after).unwrap()).unwrap();
    assert_eq!(loaded, after);
    let mut journal_bytes = checkpoint.bytes;
    let frame = journal::frame(checkpoint.identity, 1, &changes, &after.assets, &BTreeSet::from([hash])).unwrap();
    journal_bytes.extend(frame);
    let recovered = journal::recover(&journal_bytes).unwrap();
    assert_eq!(recovered.snapshot, after);
    assert_eq!(recovered.applied, 1);
    let mut missing = after;
    missing.assets = AssetStore::default();
    assert!(encode(&missing).is_err());
}
