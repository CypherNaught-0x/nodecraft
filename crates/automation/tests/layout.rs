//! Layout discovery uses the real security policy; headless execution cannot invent geometry.
use nodecraft_automation::{blank_session, mcp::McpServer};
use nodecraft_types::{CanvasId, ProjectId};
use serde_json::{Value, json};
fn call(server: &mut McpServer, name: &str, arguments: Value) -> Value {
    let reply = server.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string()).unwrap();
    serde_json::from_str::<Value>(&reply).unwrap()["result"].clone()
}
#[test]
fn layout_is_classified_discoverable_and_requires_geometry_without_mutation() {
    let canvas = CanvasId::from_bytes([71; 16]);
    let session = blank_session(ProjectId::from_bytes([73; 16]), canvas).unwrap();
    let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status());
    let mut server = McpServer::new(session, Box::new(|| [74; 16]));
    let listed = call(&mut server, "command_list", json!({"query":"canvas.auto_format"}));
    assert_eq!(listed["isError"], false);
    let commands = listed["structuredContent"]["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["id"], "canvas.auto_format");
    for mode in ["grid", "layered"] {
        let result = call(&mut server, "command_run", json!({"id":"canvas.auto_format","params":{"mode":mode}}));
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"].as_str().unwrap().starts_with("unsupported:"));
        let session = server.session().unwrap();
        assert_eq!((session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status()), before);
        assert!(!session.can_undo() && !session.can_redo());
    }
}
