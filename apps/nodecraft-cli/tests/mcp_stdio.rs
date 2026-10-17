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
    let id = ask(3, "tools/call", json!({"name": "ids_new"}))["result"]["structuredContent"]["ids"][0].clone();
    let added =
        ask(4, "tools/call", json!({"name": "command_run", "arguments": {"id": "node.add", "params": {"canvas": canvas, "node": {"id": id, "kind": "text"}}}}));
    assert_eq!(added["result"]["isError"], false, "{added}");
    let id2 = ask(5, "tools/call", json!({"name": "ids_new"}))["result"]["structuredContent"]["ids"][0].clone();
    assert_ne!(id, id2, "fresh ids");
    drop(ask);
    drop(stdin);
    assert!(child.wait().unwrap().success(), "closing stdin ends the server cleanly");
}

#[test]
fn unknown_mcp_options_are_refused() {
    let out = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["mcp", "--bridge"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("takes no options"));
}
