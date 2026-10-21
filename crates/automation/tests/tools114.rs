//! The MCP tools called in-process (#114): the same list and results as over JSON-RPC, and what
//! each call would do before it is made.
use nodecraft_automation::{blank_session, tools};
use nodecraft_engine::impact::Impact;
use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId};
use serde_json::json;

const CANVAS: CanvasId = CanvasId::from_bytes([2; 16]);

fn ids() -> impl FnMut() -> [u8; 16] {
    let mut n = 0u8;
    move || {
        n = n.wrapping_add(1);
        [n; 16]
    }
}

#[test]
fn the_tools_are_the_mcp_tools() {
    let names: Vec<String> = tools::specs().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["command_list", "command_run", "canvas_inspect", "ids_new"]);
    assert!(tools::specs().iter().all(|t| t.input_schema["type"] == "object" && !t.description.is_empty()));
}

#[test]
fn reads_are_reads_and_commands_are_what_they_would_do() {
    let mut session = blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap();
    let revision = session.revision();
    for name in ["command_list", "canvas_inspect", "ids_new"] {
        assert_eq!(tools::impact(&mut session, name, &json!({})).unwrap(), Impact::Read, "{name}");
    }
    let id = NodeId::from_bytes([9; 16]);
    let add = json!({"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": id, "kind": "text"}}});
    assert_eq!(tools::impact(&mut session, "command_run", &add).unwrap(), Impact::Edit);
    assert_eq!(session.revision(), revision, "judging a call doesn't make it");

    let ran = tools::call(&mut session, "command_run", &add, &mut ids()).unwrap();
    assert_eq!(ran["revision"], json!(session.revision()));
    assert_ne!(session.revision(), revision);
    let page = tools::call(&mut session, "canvas_inspect", &json!({"canvas": CANVAS}), &mut ids()).unwrap();
    assert_eq!(page["nodes"][0]["id"], json!(id));
    let fresh = tools::call(&mut session, "ids_new", &json!({"count": 2}), &mut ids()).unwrap();
    assert_eq!(fresh["ids"].as_array().unwrap().len(), 2);
}

#[test]
fn what_the_server_refuses_is_refused_before_and_when_called() {
    let mut session = blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap();
    let human = json!({"id": "file.save"});
    for err in [tools::impact(&mut session, "command_run", &human).unwrap_err(), tools::call(&mut session, "command_run", &human, &mut ids()).unwrap_err()] {
        assert_eq!(err.kind, ErrorKind::Unsupported);
        assert!(err.message.contains("person"), "{err:?}");
    }
    assert_eq!(tools::impact(&mut session, "command_run", &json!({"id": "no.such"})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(tools::impact(&mut session, "command_run", &json!({})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(tools::impact(&mut session, "rm_rf", &json!({})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(tools::call(&mut session, "rm_rf", &json!({}), &mut ids()).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(tools::impact(&mut session, "ids_new", &json!([1])).unwrap_err().kind, ErrorKind::InvalidInput);
    let missing = json!({"id": "node.title", "params": {"canvas": CANVAS, "id": NodeId::from_bytes([3; 16]), "title": "x"}});
    assert_eq!(tools::impact(&mut session, "command_run", &missing).unwrap_err().kind, ErrorKind::NotFound);
}
