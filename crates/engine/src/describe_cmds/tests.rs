use crate::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId};
use serde_json::json;

fn session() -> (Session, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Describe").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut stored = kinds.get("source.image").unwrap().instantiate_instance(node, Default::default(), &Default::default()).unwrap();
    stored.params.insert("stray".into(), nodecraft_types::Value::Bool(true));
    Change::AddNode { canvas, node: stored }.apply(&mut project).unwrap();
    (Session::new(project).unwrap(), canvas, node)
}

#[test]
fn describes_a_kind_and_a_stored_node_without_editing() {
    let (mut session, canvas, node) = session();
    let before = session.document().clone();
    let revision = session.revision();

    let kind = session.dispatch("node.describe", &json!({"kind": "source.text"})).unwrap();
    assert_eq!(kind["contract"]["kind"], "source.text");
    assert_eq!(kind["contract"]["modes"][0]["mode"], "source");
    assert_eq!(kind["contract"]["params"]["schema"]["params"][0]["name"], "text");
    assert!(kind.get("resolution").is_none());

    let stored = session.dispatch("node.describe", &json!({"canvas": canvas, "id": node})).unwrap();
    assert_eq!(stored["contract"]["kind"], "source.image");
    let resolution = &stored["resolution"];
    assert_eq!(resolution["params"][0]["id"], "source.image/asset");
    assert_eq!(resolution["params"][0]["value"], json!({"source": "unset"}));
    let codes: Vec<_> = resolution["issues"].as_array().unwrap().iter().map(|issue| issue["code"].clone()).collect();
    assert_eq!(codes, [json!("unknown_param"), json!("required")], "an agent sees why the node cannot run yet");
    assert_eq!(stored["settings"], json!([]), "no result, no settings");

    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert!(!session.can_undo());
}

#[test]
fn bad_targets_are_errors_not_panics() {
    let (mut session, canvas, node) = session();
    let cases = [
        (json!({}), ErrorKind::InvalidInput),
        (json!({"kind": "source.text", "canvas": canvas, "id": node}), ErrorKind::InvalidInput),
        (json!({"canvas": canvas}), ErrorKind::InvalidInput),
        (json!({"kind": "no.such_kind"}), ErrorKind::NotFound),
        (json!({"canvas": CanvasId::from_bytes([9; 16]), "id": node}), ErrorKind::NotFound),
        (json!({"canvas": canvas, "id": NodeId::from_bytes([9; 16])}), ErrorKind::NotFound),
        (json!({"kind": 7}), ErrorKind::InvalidInput),
        (json!({"kind": "source.text", "extra": true}), ErrorKind::InvalidInput),
    ];
    for (params, kind) in cases {
        assert_eq!(session.enabled("node.describe", &params).unwrap_err().kind, kind, "{params}");
        assert_eq!(session.dispatch("node.describe", &params).unwrap_err().kind, kind, "{params}");
    }
}

#[test]
fn describes_the_settings_the_active_result_was_made_with() {
    let (session, canvas, node) = session();
    let job = |model: &str| nodecraft_graph::JobSettings {
        provider: "mock".into(),
        instance: None,
        model: model.into(),
        params: [("seed".into(), nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(7)))].into(),
        item: None,
    };
    let result = |n: u8, model: &str| nodecraft_graph::Generation {
        settings: vec![job(model)],
        ..nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([n; 16]), vec![])
    };
    let mut project = session.document().clone();
    let mut stored = project.canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().clone();
    stored.history = vec![result(1, "old"), result(2, "new")];
    stored.active = Some(nodecraft_types::GenerationId::from_bytes([1; 16]));
    Change::SetNode { canvas, node: stored }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let described = session.dispatch("node.describe", &json!({"canvas": canvas, "id": node})).unwrap();
    assert_eq!(described["settings"], json!([{"provider": "mock", "model": "old", "params": {"seed": {"type": "unsigned", "value": "7"}}}]));
}
