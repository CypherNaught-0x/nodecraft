//! Format selection runs only through the live window, which supplies trusted geometry (#360).
//! A headless session lists it as not ready, says where it runs, and changes nothing.
use nodecraft_automation::{blank_session, mcp::McpServer};
use nodecraft_types::{CanvasId, ProjectId};
use serde_json::{Value, json};
fn call(server: &mut McpServer, name: &str, arguments: Value) -> Value {
    let reply = server.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string()).unwrap();
    serde_json::from_str::<Value>(&reply).unwrap()["result"].clone()
}
#[test]
fn layout_is_listed_as_live_only_and_refused_headless_without_mutation() {
    let canvas = CanvasId::from_bytes([71; 16]);
    let session = blank_session(ProjectId::from_bytes([73; 16]), canvas).unwrap();
    let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status());
    let mut server = McpServer::new(session, Box::new(|| [74; 16]));
    let listed = call(&mut server, "command_list", json!({"query":"canvas.auto_format"}));
    assert_eq!(listed["isError"], false);
    let commands = listed["structuredContent"]["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["ready"], false);
    assert!(commands[0]["not_ready_because"].as_str().unwrap().contains("nodecraft-cli mcp --bridge"));
    assert!(commands[0]["params_schema"]["properties"]["nodes"].is_object());
    for mode in ["grid", "layered"] {
        let result = call(&mut server, "command_run", json!({"id":"canvas.auto_format","params":{"mode":mode}}));
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"].as_str().unwrap().contains("running app"));
        let session = server.session().unwrap();
        assert_eq!((session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status()), before);
        assert!(!session.can_undo() && !session.can_redo());
    }
}

/// Every agent command a headless session lists as ready must not need a window capability
/// that only the live app supplies: those belong in `LIVE_ONLY`.
#[test]
fn headless_agent_commands_never_need_a_live_host_capability() {
    use nodecraft_automation::mcp::{AGENT_COMMANDS, LIVE_ONLY};
    let canvas = CanvasId::from_bytes([75; 16]);
    let session = blank_session(ProjectId::from_bytes([76; 16]), canvas).unwrap();
    let mut server = McpServer::new(session, Box::new(|| [77; 16]));
    // These need the usage log, a data resource the headless server doesn't supply yet (#883).
    let data_hosted = ["usage.export_csv", "usage.ownership", "usage.unsettled"];
    let mut missing = Vec::new();
    for id in AGENT_COMMANDS.iter().filter(|id| !LIVE_ONLY.contains(id) && !data_hosted.contains(id)) {
        let result = call(&mut server, "command_run", json!({"id":id,"params":{}}));
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        if text.contains("required host capability") {
            missing.push(*id);
        }
    }
    assert!(missing.is_empty(), "need a host capability but are not LIVE_ONLY: {missing:?}");
}
