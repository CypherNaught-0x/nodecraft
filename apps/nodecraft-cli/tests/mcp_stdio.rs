//! `nodecraft-cli mcp` end to end: a client session over the real binary's stdin and stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn an_mcp_client_session() {
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    // A server that stops answering fails the test instead of hanging it.
    let pid = child.id();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        eprintln!("mcp server {pid} timed out");
        std::process::exit(1);
    });
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |id: i64, method: &str, params: Value| -> Value {
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], id, "{v}");
        v
    };
    let init = ask(1, "initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "nodecraft");
    let overview = ask(2, "tools/call", json!({"name": "canvas_inspect", "arguments": {}}));
    let canvas = overview["result"]["structuredContent"]["canvases"][0]["id"].clone();
    let estimate = ask(
        11,
        "tools/call",
        json!({"name": "command_run", "arguments": {
            "id": "run.estimate", "params": {"canvas": canvas, "mode": "all_stale"}
        }}),
    );
    assert_eq!(estimate["result"]["isError"], false, "{estimate}");
    let estimated = &estimate["result"]["structuredContent"]["result"];
    assert_eq!(estimated["status"], "estimated");
    assert_eq!(estimated["request"], 0);
    assert_eq!(estimated["quoted"], 0);
    assert_eq!(estimated["unquoted"], 0);
    let id = ask(3, "tools/call", json!({"name": "ids_new"}))["result"]["structuredContent"]["ids"][0].clone();
    let added =
        ask(4, "tools/call", json!({"name": "command_run", "arguments": {"id": "node.add", "params": {"canvas": canvas, "node": {"id": id, "kind": "text"}}}}));
    assert_eq!(added["result"]["isError"], false, "{added}");
    let id2 = ask(5, "tools/call", json!({"name": "ids_new"}))["result"]["structuredContent"]["ids"][0].clone();
    assert_ne!(id, id2, "fresh ids");
    let listed = ask(6, "tools/call", json!({"name":"command_list","arguments":{}}));
    let commands = listed["result"]["structuredContent"]["commands"].as_array().unwrap();
    for (offset, command) in ["file.open", "file.save", "file.save_as"].into_iter().enumerate() {
        // Listed, but refused without folders the person allowed (#423).
        assert!(commands.iter().any(|item| item["id"] == command));
        let refused = ask(
            7 + i64::try_from(offset).unwrap(),
            "tools/call",
            json!({"name":"command_run","arguments":{"id":command,"params":{"location":"PRIVATE_cli_destination.ncraft"}}}),
        );
        assert_eq!(refused["result"]["isError"], true);
        let text = refused["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with(&format!("unsupported: {command} needs folders automation may use")), "{text}");
        assert!(!refused.to_string().contains("PRIVATE_cli_destination"));
    }
    let intact = ask(10, "tools/call", json!({"name":"canvas_inspect","arguments":{"canvas":canvas}}));
    assert_eq!(intact["result"]["isError"], false);
    drop(stdin);
    assert!(child.wait().unwrap().success(), "closing stdin ends the server cleanly");
}

#[test]
fn unknown_mcp_options_are_refused() {
    let out = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["mcp", "--listen"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("takes only"));
}

#[test]
fn the_bridge_serves_a_running_app_over_stdio() {
    use nodecraft_automation::control::{self, Token, TokenFile};
    // A stand-in app with its control channel and token file.
    let token = Token::generate().unwrap();
    let listener = control::listen(token.clone(), std::sync::Arc::new(|| {})).unwrap();
    let dir = std::env::temp_dir().join(format!("nodecraft-bridge-{}", std::process::id()));
    let private = control::private_dir(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = private.join("control.json");
    TokenFile { port: listener.addr.port(), token: token.to_hex(), pid: std::process::id() }.write(&path).unwrap();
    std::thread::spawn(move || {
        let canvas = nodecraft_types::CanvasId::from_bytes([2; 16]);
        let mut session = nodecraft_automation::blank_session(nodecraft_types::ProjectId::from_bytes([1; 16]), canvas).unwrap();
        while let Ok(call) = listener.calls.recv_timeout(std::time::Duration::from_secs(30)) {
            match control::answer(&mut session, &call.request) {
                Some(result) => call.reply(result),
                None => call.reply(control::screenshot_answer(2, 2, &[255; 16])),
            }
        }
    });
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["mcp", "--bridge"]).arg(&path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |id: i64, method: &str, params: Value| -> Value {
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    ask(1, "initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
    let overview = ask(2, "tools/call", json!({"name": "canvas_inspect", "arguments": {}}));
    assert_eq!(overview["result"]["structuredContent"]["canvases"][0]["id"], "02020202-0202-0202-0202-020202020202");
    let shot = ask(3, "tools/call", json!({"name": "screenshot"}));
    assert_eq!(shot["result"]["content"][0]["type"], "image");
    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(&dir).unwrap();

    // Without an app to reach, the bridge fails with an actionable message.
    let out = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["mcp", "--bridge", "/nonexistent/control.json"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--control"));
}
