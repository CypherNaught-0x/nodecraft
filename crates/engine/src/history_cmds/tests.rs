use crate::Session;
use nodecraft_graph::{Canvas, Change, Generation, Node, Port, Project};
use nodecraft_types::{CanvasId, ErrorKind, GenerationId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);

fn g(n: u8) -> GenerationId {
    GenerationId::from_bytes([n; 16])
}

/// A node with `results` text results (oldest first), the last one active, plus `extra` ports.
fn session(results: u8, outputs: Vec<(&str, Modality, bool)>, values: impl Fn(u8) -> Vec<Value>) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "History").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(N, "test.gen");
    node.outputs = outputs.into_iter().map(|(name, modality, many)| Port { name: name.into(), port_type: PortType { modality, many } }).collect();
    for n in 1..=results {
        node.history.push(Generation::new(g(n), values(n)));
    }
    node.active = (results > 0).then(|| g(results));
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    Session::new(project).unwrap()
}

fn texts(results: u8) -> Session {
    session(results, vec![("out", Modality::Text, false)], |n| vec![Value::Text(format!("take {n}"))])
}

fn node(session: &Session, id: NodeId) -> &Node {
    session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap()
}

fn history(session: &Session) -> Vec<GenerationId> {
    node(session, N).history.iter().map(|g| g.id).collect()
}

fn at() -> Json {
    json!({"canvas": C, "id": N})
}

#[test]
fn select_sets_and_clears_the_active_result_as_one_undo_step() {
    let mut s = texts(3);
    assert_eq!(s.dispatch("history.select", &json!({"canvas": C, "id": N, "generation": g(1)})).unwrap(), json!({"active": g(1)}));
    assert_eq!(node(&s, N).active, Some(g(1)));
    s.dispatch("history.select", &json!({"canvas": C, "id": N, "generation": null})).unwrap();
    assert_eq!(node(&s, N).active, None);
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(node(&s, N).active, Some(g(1)));
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(node(&s, N).active, Some(g(3)));
    let unknown = json!({"canvas": C, "id": N, "generation": g(9)});
    assert_eq!(s.enabled("history.select", &unknown).unwrap_err().kind, ErrorKind::NotFound);
    let same = json!({"canvas": C, "id": N, "generation": g(3)});
    assert_eq!(s.enabled("history.select", &same).unwrap_err().kind, ErrorKind::Conflict, "no empty undo steps");
}

#[test]
fn previous_and_next_step_through_results_and_stop_at_the_ends() {
    let mut s = texts(3);
    assert_eq!(s.enabled("history.next", &at()).unwrap_err().kind, ErrorKind::Conflict, "already the newest");
    s.dispatch("history.previous", &at()).unwrap();
    s.dispatch("history.previous", &at()).unwrap();
    assert_eq!(node(&s, N).active, Some(g(1)));
    let error = s.dispatch("history.previous", &at()).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Conflict, "this is the oldest result"));
    s.dispatch("history.next", &at()).unwrap();
    assert_eq!(node(&s, N).active, Some(g(2)));
    // With nothing active, either direction starts at the newest.
    s.dispatch("history.select", &json!({"canvas": C, "id": N, "generation": null})).unwrap();
    s.dispatch("history.previous", &at()).unwrap();
    assert_eq!(node(&s, N).active, Some(g(3)));
    let empty = texts(0);
    assert_eq!(empty.enabled("history.next", &at()).unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn deleting_the_active_result_activates_a_neighbour_and_undo_restores_order() {
    let mut s = texts(3);
    s.dispatch("history.select", &json!({"canvas": C, "id": N, "generation": g(2)})).unwrap();
    assert_eq!(s.dispatch("history.delete", &json!({"canvas": C, "id": N, "generation": g(2)})).unwrap(), json!({"active": g(3)}), "the newer one");
    assert_eq!(history(&s), [g(1), g(3)]);
    assert_eq!(s.dispatch("history.delete", &json!({"canvas": C, "id": N, "generation": g(3)})).unwrap(), json!({"active": g(1)}), "else the older one");
    assert_eq!(s.dispatch("history.delete", &json!({"canvas": C, "id": N, "generation": g(1)})).unwrap(), json!({"active": null}));
    assert!(history(&s).is_empty());
    for _ in 0..3 {
        s.dispatch("edit.undo", &Json::Null).unwrap();
    }
    assert_eq!(history(&s), [g(1), g(2), g(3)]);
    assert_eq!(node(&s, N).active, Some(g(2)));
    // Deleting an inactive result leaves the active one alone.
    s.dispatch("history.delete", &json!({"canvas": C, "id": N, "generation": g(1)})).unwrap();
    assert_eq!(node(&s, N).active, Some(g(2)));
}

#[test]
fn clear_removes_every_result_in_one_undo_step() {
    let mut s = texts(4);
    assert_eq!(s.dispatch("history.clear", &at()).unwrap(), json!({"removed": 4}));
    assert!(history(&s).is_empty());
    assert_eq!(node(&s, N).active, None);
    assert_eq!(s.enabled("history.clear", &at()).unwrap_err().kind, ErrorKind::Conflict);
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(history(&s), [g(1), g(2), g(3), g(4)]);
    assert_eq!(node(&s, N).active, Some(g(4)));
}

fn extract(s: &mut Session, extra: Json) -> Result<Json, nodecraft_types::Error> {
    let mut params = json!({"canvas": C, "id": N, "generation": g(1), "node": NodeId::from_bytes([7; 16]), "result": g(70), "pos": {"x": 400, "y": 10}});
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    s.dispatch("history.extract", &params)
}

#[test]
fn extract_adds_a_source_node_with_the_result_already_active() {
    let mut s = texts(2);
    let new = NodeId::from_bytes([7; 16]);
    assert_eq!(extract(&mut s, json!({})).unwrap(), json!({"id": new}));
    let extracted = node(&s, new);
    assert_eq!(extracted.kind, "source.text");
    assert_eq!(extracted.params.get("text"), Some(&Value::Text("take 1".into())));
    assert_eq!(extracted.active, Some(g(70)));
    assert_eq!(extracted.history, [Generation::new(g(70), vec![Value::Text("take 1".into())])], "no input hash: never stale");
    assert_eq!((extracted.pos.x, extracted.pos.y), (400.0, 10.0));
    assert_eq!(history(&s), [g(1), g(2)], "the source keeps its results");
    let staled = s.dispatch("canvas.stale", &json!({"canvas": C})).unwrap();
    assert!(!staled["nodes"].as_array().unwrap().contains(&json!(new)));
    // The caller's ids must be fresh.
    let error = extract(&mut s, json!({"node": NodeId::from_bytes([8; 16]), "result": g(2)})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput, "{}", error.message);
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&new));
}

#[test]
fn extract_picks_a_port_and_a_list_item_and_refuses_what_it_cannot_hold() {
    let lists =
        |n: u8| vec![Value::Text(format!("caption {n}")), Value::List(vec![Value::Text("a".into()), Value::Text("b".into())]), Value::Text("mesh".into())];
    let mut s = session(1, vec![("caption", Modality::Text, false), ("variants", Modality::Text, true), ("model", Modality::Mesh, false)], lists);
    let error = extract(&mut s, json!({"port": "variants"})).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::InvalidInput, "that output is a list: choose an item to extract"));
    assert_eq!(extract(&mut s, json!({"port": "variants", "item": 5})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(extract(&mut s, json!({"item": 0})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(extract(&mut s, json!({"port": "model"})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(extract(&mut s, json!({"port": "nope"})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(extract(&mut s, json!({"pos": {"x": 1e30, "y": 0}})).unwrap_err().kind, ErrorKind::InvalidInput);
    extract(&mut s, json!({"port": "variants", "item": 1})).unwrap();
    assert_eq!(node(&s, NodeId::from_bytes([7; 16])).params.get("text"), Some(&Value::Text("b".into())));
}
