use super::*;
use crate::comfy_workflow_cmds::NodeCatalog;
use crate::generation_catalog::default_registry;
use nodecraft_graph::{Canvas, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_types::ProjectId;
use std::sync::Arc;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const COMFY: NodeId = NodeId::from_bytes([2; 16]);
const CLOUD: NodeId = NodeId::from_bytes([3; 16]);
const OBJECT_INFO: &str = r#"{"SaveImage": {"input": {"required": {"images": ["IMAGE", {}]}}, "output": [], "output_node": true}}"#;

/// An SDXL card (runs on ComfyUI) and a card whose model runs in the cloud.
fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Servers").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let (kinds, catalog) = default_registry().unwrap();
    let cloud = catalog.models(KIND).find(|model| model.provider != "comfyui").unwrap().id.clone();
    for (id, model) in [(COMFY, "comfyui.sdxl".to_owned()), (CLOUD, cloud)] {
        let values = [("model".into(), Value::Text(model)), ("prompt".into(), Value::Text("A pier".into()))].into();
        let node = kinds.get(KIND).unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    Session::new(project).unwrap()
}

fn server_of(s: &Session, id: NodeId) -> Option<Value> {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap().config.get(COMFY_SERVER_CONFIG).cloned()
}

fn fetched() -> Fetched {
    let nodes: crate::comfy_workflow_cmds::NodeList = Arc::new(NodeCatalog::parse(OBJECT_INFO.as_bytes()).unwrap().0);
    nodes.into()
}

#[test]
fn a_comfy_card_picks_a_server_and_undo_puts_it_back() {
    let mut s = session();
    let pick = json!({"canvas": C, "id": COMFY, "server": "studio"});
    assert_eq!(s.dispatch("comfy.server.use", &pick).unwrap(), json!({"changed": true}));
    assert_eq!(server_of(&s, COMFY), Some(Value::Text("studio".into())));
    assert_eq!(s.dispatch("comfy.server.use", &pick).unwrap(), json!({"changed": false}), "picking it again changes nothing");

    let follow = json!({"canvas": C, "id": COMFY, "server": null});
    assert_eq!(s.dispatch("comfy.server.use", &follow).unwrap(), json!({"changed": true}));
    assert_eq!(server_of(&s, COMFY), None, "null follows the default");
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(server_of(&s, COMFY), Some(Value::Text("studio".into())));
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(server_of(&s, COMFY), None);
}

#[test]
fn other_cards_and_bad_names_are_refused() {
    let mut s = session();
    let revision = s.revision();
    let refused = |s: &mut Session, p: Json| s.dispatch("comfy.server.use", &p).unwrap_err().kind;
    assert_eq!(refused(&mut s, json!({"canvas": C, "id": CLOUD, "server": "studio"})), ErrorKind::InvalidInput, "a cloud model has no ComfyUI server");
    assert_eq!(refused(&mut s, json!({"canvas": C, "id": COMFY, "server": ""})), ErrorKind::InvalidInput);
    assert_eq!(refused(&mut s, json!({"canvas": C, "id": COMFY, "server": "x".repeat(65)})), ErrorKind::InvalidInput);
    assert_eq!(refused(&mut s, json!({"canvas": C, "id": COMFY})), ErrorKind::InvalidInput, "server is required");
    assert_eq!(refused(&mut s, json!({"canvas": C, "id": NodeId::from_bytes([9; 16]), "server": "studio"})), ErrorKind::NotFound);
    assert_eq!(s.revision(), revision);
}

#[test]
fn each_server_keeps_its_own_check_and_only_the_default_installs_nodes() {
    let mut s = session();
    let other = finish(&mut s, 1, "studio", false, Ok(fetched()));
    assert_eq!(other.nodes, Some(1));
    assert!(installed(&s).is_none(), "another server's nodes aren't the list imports use");
    let failed = finish(&mut s, 2, "laptop", true, Err(Error::new(ErrorKind::Network, "refused")));
    assert_eq!(failed.error.as_deref(), Some("refused"));
    finish(&mut s, 3, "laptop", true, Ok(fetched()));
    assert!(installed(&s).is_some(), "the default's nodes are");
    assert_eq!(report(&s, "studio").map(|r| r.id), Some(1));
    assert_eq!(report(&s, "laptop").map(|r| r.id), Some(3), "a later check replaces the earlier one");

    let status = s.dispatch("comfy.nodes.status", &json!({})).unwrap();
    assert_eq!(status["servers"]["studio"]["nodes"], json!(1), "{status}");

    forget(&mut s, "studio", false);
    assert!(report(&s, "studio").is_none());
    assert!(installed(&s).is_some(), "forgetting another server keeps the default's nodes");
    forget(&mut s, "laptop", true);
    assert!(report(&s, "laptop").is_none());
    assert!(installed(&s).is_none());
}

#[test]
fn reports_are_capped_at_the_server_limit() {
    let mut s = session();
    for n in 0..MAX_COMFY_SERVERS + 5 {
        finish(&mut s, n as u64, &format!("s{n}"), false, Err(Error::new(ErrorKind::Network, "down")));
    }
    let status = s.dispatch("comfy.nodes.status", &json!({})).unwrap();
    assert_eq!(status["servers"].as_object().unwrap().len(), MAX_COMFY_SERVERS);
    finish(&mut s, 99, "s0", false, Ok(fetched()));
    assert_eq!(report(&s, "s0").map(|r| r.id), Some(99), "a known server still updates at the cap");
}

/// How many node classes the list imports use has.
fn installed(s: &Session) -> Option<usize> {
    crate::comfy_workflow_cmds::last(s).1
}
