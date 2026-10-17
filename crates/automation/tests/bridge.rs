//! The MCP server bridged to a running app's control channel (the app is simulated: a thread
//! answering control calls against its own session, the way the app's UI thread does).

use std::sync::Arc;
use std::time::Duration;

use nodecraft_automation::blank_session;
use nodecraft_automation::control::{self, Client, Token};
use nodecraft_automation::mcp::McpServer;
use nodecraft_types::{CanvasId, ProjectId};
use serde_json::{Value, json};

const CANVAS: CanvasId = CanvasId::from_bytes([2; 16]);

/// A stand-in app: answers control calls until the channel closes.
fn app() -> (std::net::SocketAddr, Token) {
    let token = Token::generate().unwrap();
    let listener = control::listen(token.clone(), Arc::new(|| {})).unwrap();
    let addr = listener.addr;
    std::thread::spawn(move || {
        let mut session = blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap();
        while let Ok(call) = listener.calls.recv_timeout(Duration::from_secs(30)) {
            match control::answer(&mut session, &call.request) {
                Some(result) => call.reply(result),
                None => call.reply(control::screenshot_answer(2, 2, &[255; 16])),
            }
        }
    });
    (addr, token)
}

fn ask(server: &mut McpServer, method: &str, params: Value) -> Value {
    let reply = server.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()).unwrap();
    serde_json::from_str(&reply).unwrap()
}

fn tool(server: &mut McpServer, name: &str, args: Value) -> Value {
    ask(server, "tools/call", json!({"name": name, "arguments": args}))["result"].clone()
}

#[test]
fn an_agent_edits_and_sees_the_running_app() {
    let (addr, token) = app();
    let mut n = 0u8;
    let mut server = McpServer::bridge(
        Client::connect(addr, &token.to_hex()).unwrap(),
        Box::new(move || {
            n = n.wrapping_add(1);
            [n; 16]
        }),
    );
    assert!(server.session().is_none(), "no session of its own");
    let tools = ask(&mut server, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"screenshot") && names.contains(&"command_run"), "{names:?}");
    let id = tool(&mut server, "ids_new", json!({}))["structuredContent"]["ids"][0].clone();
    let added = tool(&mut server, "command_run", json!({"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": id, "kind": "source.text"}}}));
    assert_eq!(added["isError"], false, "{added}");
    let page = tool(&mut server, "canvas_inspect", json!({"canvas": CANVAS}));
    assert_eq!(page["structuredContent"]["nodes"].as_array().map(Vec::len), Some(1));
    let listed = tool(&mut server, "command_list", json!({"query": "undo"}));
    assert_eq!(listed["structuredContent"]["commands"][0]["id"], "edit.undo");
    let shot = tool(&mut server, "screenshot", json!({}));
    assert_eq!((shot["content"][0]["type"].as_str(), shot["content"][0]["mimeType"].as_str()), (Some("image"), Some("image/png")));
    // The app's policy still applies through the bridge.
    let refused = tool(&mut server, "command_run", json!({"id": "keys.set", "params": {"account": "openai", "key": "sk-secret-1"}}));
    assert_eq!(refused["isError"], true);
    assert!(!refused.to_string().contains("sk-secret"));
}

#[test]
fn a_local_server_has_no_screenshot_tool_and_bad_bridges_fail_clearly() {
    let mut local = McpServer::new(blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap(), Box::new(|| [3; 16]));
    let tools = ask(&mut local, "tools/list", json!({}));
    assert!(!tools.to_string().contains("\"screenshot\""));
    assert_eq!(ask(&mut local, "tools/call", json!({"name": "screenshot"}))["error"]["code"], -32602);
    let (addr, _) = app();
    let wrong = Client::connect(addr, &"0".repeat(64)).err().unwrap();
    assert_eq!(wrong.kind, nodecraft_types::ErrorKind::Authentication, "{wrong:?}");
    let missing = Client::from_token_file(std::path::Path::new("/nonexistent/control.json")).err().unwrap();
    assert!(missing.message.contains("--control"), "{}", missing.message);
}
