//! The MCP server through its JSON-RPC surface: what a client sees.

use nodecraft_automation::blank_session;
use nodecraft_automation::mcp::{AGENT_COMMANDS, HUMAN_ONLY, MAX_MESSAGE_BYTES, McpServer, PAGE_BUDGET_BYTES, ServeEnd, serve};
use nodecraft_engine::commands::Registry;
use nodecraft_types::{CanvasId, ProjectId};
use serde_json::{Value, json};

const CANVAS: CanvasId = CanvasId::from_bytes([2; 16]);

fn server() -> McpServer {
    let mut n: u64 = 0;
    let ids = move || {
        n += 1;
        let mut b = [7u8; 16];
        b[..8].copy_from_slice(&n.to_le_bytes());
        b
    };
    McpServer::new(blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap(), Box::new(ids))
}

fn ask(s: &mut McpServer, method: &str, params: Value) -> Value {
    let reply = s.handle(&json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string()).unwrap();
    let v: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!((v["jsonrpc"].as_str(), v["id"].as_i64()), (Some("2.0"), Some(7)), "{v}");
    v
}

fn call(s: &mut McpServer, tool: &str, args: Value) -> Value {
    ask(s, "tools/call", json!({"name": tool, "arguments": args}))["result"].clone()
}

#[test]
fn handshake_and_version_negotiation() {
    let mut s = server();
    let init = ask(&mut s, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "a version we speak is echoed");
    assert_eq!(init["result"]["serverInfo"]["name"], "nodecraft");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let future = ask(&mut s, "initialize", json!({"protocolVersion": "2099-01-01"}));
    assert_eq!(future["result"]["protocolVersion"], "2025-11-25", "otherwise our newest");
    assert_eq!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None, "notifications get no reply");
    assert_eq!(ask(&mut s, "ping", json!({}))["result"], json!({}));
    let tools = ask(&mut s, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["command_list", "command_run", "canvas_inspect", "ids_new"]);
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
    }
}

#[test]
fn protocol_errors() {
    let mut s = server();
    let parse = |s: &mut McpServer, m: &str| -> Value { serde_json::from_str(&s.handle(m).unwrap()).unwrap() };
    assert_eq!(parse(&mut s, "{not json")["error"]["code"], -32700);
    assert_eq!(parse(&mut s, "[]")["error"]["code"], -32600, "no batches");
    assert_eq!(parse(&mut s, r#"{"jsonrpc":"2.0","id":1}"#)["error"]["code"], -32600);
    assert_eq!(parse(&mut s, r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#)["error"]["code"], -32600);
    assert_eq!(ask(&mut s, "resources/list", json!({}))["error"]["code"], -32601);
    assert_eq!(ask(&mut s, "tools/call", json!({"name": "rm_rf"}))["error"]["code"], -32602);
    assert_eq!(ask(&mut s, "tools/call", json!({"name": "command_list", "arguments": [1]}))["error"]["code"], -32602);
    assert_eq!(s.handle(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#), None, "responses to nothing are ignored");
    let huge = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"x":"{}"}}}}"#, "a".repeat(MAX_MESSAGE_BYTES));
    assert_eq!(parse(&mut s, &huge)["error"]["code"], -32600);
}

#[test]
fn commands_are_listed_and_run_like_the_ui() {
    let mut s = server();
    let list = call(&mut s, "command_list", json!({}));
    assert_eq!(list["isError"], false);
    let commands = list["structuredContent"]["commands"].as_array().unwrap().clone();
    let ids: Vec<&str> = commands.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"node.add") && ids.contains(&"edit.undo") && ids.contains(&"keys.status"), "{ids:?}");
    for hidden in HUMAN_ONLY {
        assert!(!ids.contains(hidden), "{hidden} is for people only");
    }
    let undo = commands.iter().find(|c| c["id"] == "edit.undo").unwrap();
    assert_eq!(undo["ready"], false, "nothing to undo yet");
    assert!(undo["not_ready_because"].as_str().is_some());
    assert!(commands.iter().all(|c| c["params_schema"].is_object()));
    let filtered = call(&mut s, "command_list", json!({"query": "UNDO"}));
    assert_eq!(filtered["structuredContent"]["commands"].as_array().unwrap().len(), 1);
    assert_eq!(list["content"][0]["type"], "text", "text for clients that don't read structured content");

    let id = call(&mut s, "ids_new", json!({}))["structuredContent"]["ids"][0].as_str().unwrap().to_owned();
    assert_eq!(id.as_bytes().get(14), Some(&b'4'), "a version-4 UUID: {id}");
    let added = call(&mut s, "command_run", json!({"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": id, "kind": "text"}}}));
    assert_eq!(added["isError"], false, "{added}");
    assert_eq!(added["structuredContent"]["revision"], 1);
    let page = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS}));
    assert_eq!(page["structuredContent"]["nodes"][0]["id"], id.as_str());
    assert_eq!(call(&mut s, "command_run", json!({"id": "edit.undo"}))["isError"], false);
    assert_eq!(s.session().revision(), 2);
    let overview = call(&mut s, "canvas_inspect", json!({}));
    assert_eq!(overview["structuredContent"]["canvases"][0]["nodes"], 0, "undone");
}

#[test]
fn node_edits_use_shared_validation_and_undo_over_mcp() {
    let mut s = server();
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let id = nodecraft_types::NodeId::from_bytes([8; 16]);
    let node = kinds.get("source.text").unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
    let added = call(&mut s, "command_run", json!({"id":"node.add","params":{"canvas":CANVAS,"node":node}}));
    assert_eq!(added["isError"], false, "{added}");
    let original = s.session().document().clone();
    let list = call(&mut s, "command_list", json!({"query":"node."}));
    let commands = list["structuredContent"]["commands"].as_array().unwrap();
    for command in ["node.title", "node.param_set"] {
        let entry = commands.iter().find(|entry| entry["id"] == command).unwrap();
        assert_eq!(entry["params_schema"], (s.session().registry().get(command).unwrap().input_schema)());
    }
    let inspect = call(&mut s, "canvas_inspect", json!({"canvas":CANVAS,"node":id}));
    assert_eq!(inspect["structuredContent"]["node"]["title"], "", "automatic titles retain empty stored data");
    for (command, params) in [
        ("node.title", json!({"canvas":CANVAS,"id":id,"title":"Renamed"})),
        ("node.param_set", json!({"canvas":CANVAS,"id":id,"name":"text","value":{"type":"text","value":"Edited prompt"}})),
    ] {
        let before = s.session().document().clone();
        let revision = s.session().revision();
        let edited = call(&mut s, "command_run", json!({"id":command,"params":params}));
        assert_eq!(edited["isError"], false, "{edited}");
        assert_eq!(s.session().revision(), revision + 1);
        assert_ne!(s.session().document(), &before);
        assert_eq!(call(&mut s, "command_run", json!({"id":"edit.undo"}))["isError"], false);
        assert_eq!(s.session().document(), &before);
        assert_eq!(call(&mut s, "command_run", json!({"id":"edit.redo"}))["isError"], false);
        let inspected = call(&mut s, "canvas_inspect", json!({"canvas":CANVAS,"node":id}));
        if command == "node.title" {
            assert_eq!(inspected["structuredContent"]["node"]["title"], "Renamed");
        } else {
            assert_eq!(inspected["structuredContent"]["node"]["params"]["text"]["value"], "Edited prompt");
        }
        assert_eq!(call(&mut s, "command_run", json!({"id":"edit.undo"}))["isError"], false);
    }
    assert_eq!(s.session().document(), &original);
    for (command, params) in [
        ("node.title", json!({"canvas":CANVAS,"id":id,"title":"x".repeat(4097)})),
        ("node.param_set", json!({"canvas":CANVAS,"id":id,"name":"text","value":{"type":"bool","value":true}})),
        ("node.param_set", json!({"canvas":CANVAS,"id":id,"name":"unknown","value":null})),
        ("node.param_set", json!({"canvas":CANVAS,"id":id,"name":"text"})),
    ] {
        let revision = s.session().revision();
        let history = (s.session().can_undo(), s.session().can_redo());
        assert!(s.session().enabled(command, &params).is_err());
        let rejected = call(&mut s, "command_run", json!({"id":command,"params":params}));
        assert_eq!(rejected["isError"], true, "{rejected}");
        assert_eq!(s.session().document(), &original);
        assert_eq!(s.session().revision(), revision);
        assert_eq!((s.session().can_undo(), s.session().can_redo()), history);
        assert!(s.session().events_since(revision).unwrap().is_empty());
    }
    assert_eq!(call(&mut s, "command_run", json!({"id":"edit.redo"}))["isError"], false, "rejected edits preserve redo");
}

#[test]
fn tool_failures_are_results_the_model_can_read() {
    let mut s = server();
    for (args, says) in [
        (json!({"id": "keys.set", "params": {"account": "openai", "key": "sk-secret-1234567890"}}), "only be run by a person"),
        (json!({"id": "keys.remove", "params": {"account": "openai"}}), "only be run by a person"),
        (json!({"id": "nope.nope"}), "unknown command"),
        (json!({"id": "node.add", "params": {"canvas": CANVAS}}), "node"),
        (json!({}), "needs the command `id`"),
    ] {
        let r = call(&mut s, "command_run", args.clone());
        assert_eq!(r["isError"], true, "{args}");
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(says), "{text}");
        assert!(!text.contains("sk-secret"), "{text}");
    }
    assert_eq!(s.session().revision(), 0, "nothing changed");
    assert_eq!(call(&mut s, "canvas_inspect", json!({"canvas": "not-a-uuid"}))["isError"], true);
    assert_eq!(call(&mut s, "canvas_inspect", json!({"canvas": CanvasId::from_bytes([9; 16])}))["isError"], true);
    assert_eq!(call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "offset": -1}))["isError"], true);
    assert_eq!(call(&mut s, "ids_new", json!({"count": 0}))["isError"], true);
    assert_eq!(call(&mut s, "ids_new", json!({"count": 101}))["isError"], true);
    assert_eq!(call(&mut s, "ids_new", json!({"count": 100}))["structuredContent"]["ids"].as_array().unwrap().len(), 100);
}

#[test]
fn canvas_pages() {
    let mut s = server();
    let ids = call(&mut s, "ids_new", json!({"count": 5}))["structuredContent"]["ids"].clone();
    for id in ids.as_array().unwrap() {
        let r = call(&mut s, "command_run", json!({"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": id, "kind": "text"}}}));
        assert_eq!(r["isError"], false, "{r}");
    }
    let first = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "limit": 2}))["structuredContent"].clone();
    assert_eq!((first["nodes"].as_array().unwrap().len(), first["next_offset"].as_u64()), (2, Some(2)));
    let last = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "offset": 4, "limit": 2}))["structuredContent"].clone();
    assert_eq!((last["nodes"].as_array().unwrap().len(), last["next_offset"].clone()), (1, Value::Null));
    let past = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "offset": 99}))["structuredContent"].clone();
    assert_eq!(past["nodes"], json!([]));
    assert_eq!(first["canvas"]["nodes_total"], 5);
}

#[test]
fn stdio_framing() {
    let mut s = server();
    let long = format!("{}\n", "x".repeat(MAX_MESSAGE_BYTES + 10));
    let input = format!(
        "{}\n\n{long}{}\r\n{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "ping"})
    );
    // A tiny buffer makes lines span many reads.
    let mut reader = std::io::BufReader::with_capacity(7, input.as_bytes());
    let mut out = Vec::new();
    assert!(matches!(serve(&mut s, &mut reader, &mut out), ServeEnd::Closed));
    let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 4, "{replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["error"]["code"], -32600, "an oversized line is skipped with an error");
    assert_eq!((replies[2]["id"].as_i64(), replies[3]["id"].as_i64()), (Some(2), Some(3)), "CRLF and a last line without a newline");
}

#[test]
fn every_command_is_classified_for_automation() {
    // Adding a command means deciding whether agents may run it: put it in AGENT_COMMANDS or
    // HUMAN_ONLY (crates/automation/src/mcp.rs).
    for spec in Registry::builtins().unwrap().specs() {
        let (agent, human) = (AGENT_COMMANDS.contains(&spec.id), HUMAN_ONLY.contains(&spec.id));
        assert!(agent != human, "{} must be in exactly one of AGENT_COMMANDS and HUMAN_ONLY", spec.id);
    }
}

#[test]
fn ready_is_only_reported_for_parameterless_commands() {
    let mut s = server();
    let list = call(&mut s, "command_list", json!({}))["structuredContent"]["commands"].clone();
    let get = |id: &str| list.as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
    assert!(get("node.add").get("ready").is_none(), "needs params: no verdict");
    assert!(get("keys.status").get("ready").is_none());
    assert_eq!(get("edit.redo")["ready"], false);
    let stale = call(&mut s, "command_run", json!({"id": "canvas.stale", "params": {"canvas": CANVAS}}));
    assert_eq!(stale["structuredContent"]["result"]["nodes"], json!([]), "node kinds are available: {stale}");
}

#[test]
fn pages_stay_small_and_big_nodes_are_summarised() {
    let mut s = server();
    let ids = call(&mut s, "ids_new", json!({"count": 12}))["structuredContent"]["ids"].clone();
    let big = "x".repeat(200 * 1024);
    for id in ids.as_array().unwrap() {
        let r = call(
            &mut s,
            "command_run",
            json!({"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": id, "kind": "text", "params": {"text": {"type": "text", "value": big}}}}}),
        );
        assert_eq!(r["isError"], false, "{r}");
    }
    let page = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS}));
    let text = page["content"][0]["text"].as_str().unwrap();
    assert!(text.len() < 2 * PAGE_BUDGET_BYTES, "{} bytes", text.len());
    let body = &page["structuredContent"];
    assert_eq!(body["nodes"].as_array().unwrap().len(), 12, "summaries are small, so all fit");
    assert!(body["nodes"][0]["summarised"].is_string());
    let first = body["nodes"][0]["id"].clone();
    let full = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "node": first}));
    assert_eq!(full["structuredContent"]["node"]["params"]["text"]["value"].as_str().map(str::len), Some(big.len()), "one node in full");
    assert_eq!(call(&mut s, "canvas_inspect", json!({"node": first}))["isError"], true, "node needs its canvas");
}

#[test]
fn many_medium_nodes_are_paged_by_size() {
    let mut s = server();
    let ids = call(&mut s, "ids_new", json!({"count": 60}))["structuredContent"]["ids"].clone();
    let medium = "y".repeat(40 * 1024);
    for id in ids.as_array().unwrap() {
        call(
            &mut s,
            "command_run",
            json!({"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": id, "kind": "text", "params": {"text": {"type": "text", "value": medium}}}}}),
        );
    }
    let mut offset = 0;
    let mut seen = 0;
    loop {
        let page = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "offset": offset, "limit": 500}))["structuredContent"].clone();
        let n = page["nodes"].as_array().unwrap().len();
        assert!((1..60).contains(&n), "cut short by size: {n}");
        seen += n;
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(seen, 60, "every node on some page");
}

#[test]
fn malformed_envelopes() {
    let mut s = server();
    let r: Value = serde_json::from_str(&s.handle(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).unwrap()).unwrap();
    assert_eq!(r["error"]["code"], -32600);
    let id = nodecraft_types::NodeId::from_bytes([8; 16]);
    assert_eq!(call(&mut s, "command_run", json!({"id":"node.add","params":{"canvas":CANVAS,"node":{"id":id,"kind":"text"}}}))["isError"], false);
    call(&mut s, "command_run", json!({"id":"node.title","params":{"canvas":CANVAS,"id":id,"title":"redo"}}));
    call(&mut s, "command_run", json!({"id":"edit.undo"}));
    let before = s.session().document().clone();
    let revision = s.session().revision();
    let history = (s.session().can_undo(), s.session().can_redo());
    let events = s.session().events_since(0).unwrap();
    let attempted = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"node.title","params":{"canvas":CANVAS,"id":id,"title":"INVALID_MARKER"}}}}).to_string();
    let marker = attempted.find("INVALID_MARKER").unwrap();
    let mut input = attempted.into_bytes();
    input[marker] = 0xff; // Otherwise valid mutation, with invalid UTF-8 inside caller text.
    input.push(b'\n');
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n");
    let mut reader = std::io::BufReader::with_capacity(7, input.as_slice());
    let mut out = Vec::new();
    assert!(matches!(serve(&mut s, &mut reader, &mut out), ServeEnd::Closed));
    let replies: Vec<Value> = out.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["error"]["code"], -32700, "invalid UTF-8 is a parse error before mutation");
    assert_eq!(replies[1], json!({"jsonrpc":"2.0","id":3,"result":{}}), "following valid frame remains aligned");
    assert_eq!(s.session().document(), &before);
    assert_eq!(s.session().revision(), revision);
    assert_eq!((s.session().can_undo(), s.session().can_redo()), history);
    assert_eq!(s.session().events_since(0).unwrap(), events);
    let valid = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"node.title","params":{"canvas":CANVAS,"id":id,"title":"valid after malformed frame"}}}}).to_string()+"\n";
    let mut out = Vec::new();
    assert!(matches!(serve(&mut s, &mut valid.as_bytes(), &mut out), ServeEnd::Closed));
    let reply: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(s.session().revision(), revision + 1);
    call(&mut s, "command_run", json!({"id":"edit.undo"}));
    assert_eq!(s.session().document(), &before);
}

#[test]
fn a_node_with_many_edges_pages_them() {
    let mut s = server();
    let ids = call(&mut s, "ids_new", json!({"count": 2}))["structuredContent"]["ids"].clone();
    let (a, b) = (ids[0].clone(), ids[1].clone());
    let port = "p".repeat(250);
    let many = json!([{"name": port, "port_type": {"modality": "text", "many": true}}]);
    for id in [&a, &b] {
        let node = json!({"id": id, "kind": "text", "inputs": many, "outputs": many});
        let r = call(&mut s, "command_run", json!({"id": "node.add", "params": {"canvas": CANVAS, "node": node}}));
        assert_eq!(r["isError"], false, "{r}");
    }
    let mut added = 0u32;
    for _ in 0..12 {
        let batch = call(&mut s, "ids_new", json!({"count": 100}))["structuredContent"]["ids"].clone();
        for eid in batch.as_array().unwrap() {
            let edge = json!({"id": eid, "from": {"node": a, "port": port}, "to": {"node": b, "port": port}, "order": added});
            let r = call(&mut s, "command_run", json!({"id": "edge.connect", "params": {"canvas": CANVAS, "edge": edge}}));
            assert_eq!(r["isError"], false, "{r}");
            added += 1;
        }
    }
    // The node the edges enter starts its own page, which holds as many as fit.
    let first = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS}))["structuredContent"].clone();
    let offset = if first["edges_truncated"] == true { 0 } else { first["next_offset"].as_u64().unwrap() };
    let page = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "offset": offset}))["structuredContent"].clone();
    assert_eq!(page["edges_truncated"], true, "{}", page["canvas"]);
    assert_eq!(page["edges_continue"]["node"], b);
    assert!(page["edges_continue"]["node"].is_string());
    let mut offset = 0u64;
    let mut seen = 0usize;
    loop {
        let r = call(&mut s, "canvas_inspect", json!({"canvas": CANVAS, "node": b, "edge_offset": offset}));
        assert!(r["content"][0]["text"].as_str().unwrap().len() < 2 * PAGE_BUDGET_BYTES + 64 * 1024);
        let body = &r["structuredContent"];
        seen += body["edges"].as_array().unwrap().len();
        match body["next_edge_offset"].as_u64() {
            Some(n) => offset = n,
            None => break,
        }
    }
    assert_eq!(seen, added as usize);
}

#[test]
fn navigation_commands_are_discoverable_but_require_a_live_view_host() {
    let mut s = server();
    let list = call(&mut s, "command_list", json!({"query":"canvas."}));
    let commands = list["structuredContent"]["commands"].as_array().unwrap();
    let before = s.session().document().clone();
    let revision = s.session().revision();
    for (id, params) in
        [("canvas.fit_all", json!({})), ("canvas.fit_selection", json!({})), ("canvas.minimap_toggle", json!({})), ("canvas.pan_to", json!({"x":10,"y":20}))]
    {
        let entry = commands.iter().find(|entry| entry["id"] == id).unwrap();
        assert_eq!(entry["params_schema"], (s.session().registry().get(id).unwrap().input_schema)());
        if id != "canvas.pan_to" {
            assert_eq!(entry["ready"], false);
        }
        let error = s.session().enabled(id, &params).unwrap_err();
        assert_eq!(error.kind, nodecraft_types::ErrorKind::Unsupported);
        let result = call(&mut s, "command_run", json!({"id":id,"params":params}));
        assert_eq!(result["isError"], true, "{result}");
        assert!(result["content"][0]["text"].as_str().unwrap().contains("unsupported"));
        assert_eq!(s.session().document(), &before);
        assert_eq!(s.session().revision(), revision);
        assert!(!s.session().can_undo() && !s.session().can_redo());
        assert!(s.session().events_since(revision).unwrap().is_empty());
    }
}

#[test]
fn navigation_dispatch_uses_explicit_live_host_without_document_mutations() {
    use nodecraft_engine::commands::Resources;
    use nodecraft_engine::navigation_cmds::{NavigationContext, SERVICE};
    use std::{cell::RefCell, rc::Rc};
    let mut session = blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap();
    let mut resources = Resources::default();
    resources.insert(NavigationContext { canvas: CANVAS, viewport_available: true, blocked: false, fit_all: true, fit_selection: true });
    session = session.with_resources(resources);
    let requests = Rc::new(RefCell::new(Vec::<Value>::new()));
    let recorded = Rc::clone(&requests);
    session
        .register_host_service(SERVICE, move |request| {
            recorded.borrow_mut().push(request.clone());
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    for (id, params) in
        [("canvas.fit_all", json!({})), ("canvas.fit_selection", json!({})), ("canvas.minimap_toggle", json!({})), ("canvas.pan_to", json!({"x":10,"y":20}))]
    {
        session.enabled(id, &params).unwrap();
    }
    let mut s = McpServer::new(session, Box::new(|| [7; 16]));
    let list = call(&mut s, "command_list", json!({"query":"canvas."}));
    for id in ["canvas.fit_all", "canvas.fit_selection", "canvas.minimap_toggle"] {
        let entry = list["structuredContent"]["commands"].as_array().unwrap().iter().find(|entry| entry["id"] == id).unwrap();
        assert_eq!(entry["ready"], true);
    }
    for (id, params) in
        [("canvas.fit_all", json!({})), ("canvas.fit_selection", json!({})), ("canvas.minimap_toggle", json!({})), ("canvas.pan_to", json!({"x":10,"y":20}))]
    {
        let result = call(&mut s, "command_run", json!({"id":id,"params":params}));
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["structuredContent"]["result"], json!({"accepted":true}));
        assert_eq!(s.session().document(), &before);
        assert_eq!(s.session().revision(), revision);
        assert!(!s.session().can_undo() && !s.session().can_redo());
        assert!(s.session().events_since(revision).unwrap().is_empty());
    }
    assert_eq!(
        *requests.borrow(),
        vec![json!({"action":"fit_all"}), json!({"action":"fit_selection"}), json!({"action":"minimap_toggle"}), json!({"action":"pan_to","x":10.0,"y":20.0})]
    );
    let rejected = call(&mut s, "command_run", json!({"id":"canvas.pan_to","params":{"x":1e13,"y":0}}));
    assert_eq!(rejected["isError"], true);
    assert_eq!(requests.borrow().len(), 4, "invalid params never reach the host");
}
