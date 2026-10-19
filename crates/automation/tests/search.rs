//! Search discovery shares the real automation policy; a headless caller has no live canvas.
use nodecraft_automation::{blank_session, mcp::McpServer};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::{Value, json};

fn call(server: &mut McpServer, name: &str, arguments: Value) -> Value {
    let reply = server.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string()).unwrap();
    serde_json::from_str::<Value>(&reply).unwrap()["result"].clone()
}
#[test]
fn search_is_discoverable_but_headless_dispatch_preserves_history() {
    let canvas = CanvasId::from_bytes([71; 16]);
    let node = NodeId::from_bytes([72; 16]);
    let mut session = blank_session(ProjectId::from_bytes([73; 16]), canvas).unwrap();
    session.dispatch("node.add", &json!({"canvas":canvas,"node":{"id":node,"kind":"source.text"}})).unwrap();
    session.dispatch("node.title", &json!({"canvas":canvas,"id":node,"title":"Search history"})).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(session.can_undo() && session.can_redo());
    let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap());
    for (id, params) in [
        ("canvas.search_open", json!({})),
        ("canvas.search_query", json!({"query":"needle"})),
        ("canvas.search_next", json!({})),
        ("canvas.search_previous", json!({})),
        ("canvas.search_close", json!({})),
    ] {
        assert_eq!(session.enabled(id, &params).unwrap_err().kind, nodecraft_types::ErrorKind::Unsupported);
    }
    let mut server = McpServer::new(session, Box::new(|| [74; 16]));
    let listed = call(&mut server, "command_list", json!({"query":"canvas.search"}));
    assert_eq!(listed["isError"], false);
    let entries = listed["structuredContent"]["commands"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    for id in ["canvas.search_open", "canvas.search_query", "canvas.search_next", "canvas.search_previous", "canvas.search_close"] {
        let entry = entries.iter().find(|entry| entry["id"] == id).unwrap();
        if id != "canvas.search_query" {
            assert_eq!(entry["ready"], false);
        }
        let params = if id == "canvas.search_query" { json!({"query":"needle"}) } else { json!({}) };
        let result = call(&mut server, "command_run", json!({"id":id,"params":params}));
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"].as_str().unwrap().starts_with("unsupported:"));
        let session = server.session().unwrap();
        assert_eq!((session.document().clone(), session.revision(), session.events_since(0).unwrap()), before);
        assert!(session.can_undo() && session.can_redo());
    }
    assert_eq!(call(&mut server, "command_run", json!({"id":"edit.redo"}))["isError"], false);
    assert_eq!(call(&mut server, "command_run", json!({"id":"edit.undo"}))["isError"], false);
    assert_eq!(server.session().unwrap().document(), &before.0);
}
