//! Drive the actual CLI pipes: no keychain, provider, socket or native window.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Client {
    child: Child,
    input: Option<ChildStdin>,
    replies: Receiver<String>,
}
impl Client {
    fn new() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli"))
            .arg("control")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (send, replies) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, input, replies }
    }
    fn bytes(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }
    fn reply(&self) -> Value {
        serde_json::from_str(&self.replies.recv_timeout(Duration::from_secs(20)).expect("control must answer or close, never hang")).unwrap()
    }
    fn ask(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.bytes(format!("{}\n", json!({"id":id,"method":method,"params":params})).as_bytes());
        let reply = self.reply();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }
    fn finish(&mut self) {
        self.input.take();
        assert!(wait(&mut self.child).success());
        assert!(self.replies.recv_timeout(Duration::from_secs(5)).is_err(), "no extra protocol output");
        let mut stderr = String::new();
        std::io::Read::read_to_string(self.child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
        assert!(stderr.is_empty(), "{stderr}");
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("control did not exit after EOF/broken stdout");
        }
        std::thread::yield_now();
    }
}

#[test]
fn trusted_parent_can_negotiate_edit_inspect_and_undo() {
    let mut client = Client::new();
    let hello = client.ask(1, "hello", json!({}));
    assert_eq!(hello["result"]["app"], "nodecraft");
    assert_eq!(hello["result"]["protocol"], 1);
    assert!(hello["result"]["version"].is_string());
    let list = client.ask(2, "commands.list", json!({"query":"node.add"}));
    assert_eq!(list["result"]["commands"][0]["id"], "node.add");
    let state = client.ask(3, "state.get", json!({}));
    let canvas = state["result"]["canvases"][0]["id"].clone();
    assert_eq!(state["result"]["revision"], 0);
    assert_eq!(state["result"]["canvases"][0]["nodes"], 0);
    let node = "11111111-1111-4111-8111-111111111111";
    let added = client.ask(4, "command.run", json!({"id":"node.add","params":{"canvas":canvas,"node":{"id":node,"kind":"source.text"}}}));
    assert_eq!(added["result"]["revision"], 1, "{added}");
    let inspect = client.ask(5, "canvas.inspect", json!({"canvas":canvas}));
    assert_eq!(inspect["result"]["nodes"][0]["id"], node);
    let undo = client.ask(6, "command.run", json!({"id":"edit.undo","params":{}}));
    assert_eq!(undo["result"]["revision"], 2);
    let state = client.ask(7, "state.get", json!({}));
    assert_eq!(state["result"]["canvases"][0]["nodes"], 0);
    assert_eq!(state["result"]["can_redo"], true);
    client.finish();
}

#[test]
fn malformed_oversized_and_non_utf8_lines_recover_without_echoing_payloads() {
    let mut client = Client::new();
    for bytes in [b" \nnot-json-private-marker\n".to_vec(), b"\xff\n".to_vec(), vec![b'x'; 4 * 1024 * 1024 + 1].into_iter().chain(*b"\n").collect()] {
        client.bytes(&bytes);
        let error = client.reply();
        assert_eq!(error["id"], Value::Null);
        assert_eq!(error["error"]["kind"], "invalid_input");
        assert!(!error.to_string().contains("private-marker"));
        assert_eq!(client.ask(10, "hello", json!({}))["result"]["protocol"], 1);
    }
    let error = client.ask(11, "command.run", json!([]));
    assert_eq!(error["error"]["kind"], "invalid_input");
    // A final valid line without newline is processed, and EOF then exits cleanly.
    client.bytes(b"{\"id\":12,\"method\":\"state.get\"}");
    client.input.take();
    assert_eq!(client.reply()["id"], 12);
    client.finish();
    let mut empty = Client::new();
    empty.finish();
}

#[test]
fn headless_ui_and_human_only_operations_are_refused_without_document_changes() {
    let mut client = Client::new();
    // Trusted parent may omit hello; it isn't authentication for stdio.
    let before = client.ask(1, "state.get", json!({}));
    for (id, params) in [
        ("canvas.fit_all", json!({})),
        ("canvas.fit_selection", json!({})),
        ("canvas.minimap_toggle", json!({})),
        ("canvas.pan_to", json!({"x":0,"y":0})),
        ("keys.set", json!({"account":"test","key":"fixture-not-a-secret"})),
        ("keys.remove", json!({"account":"test"})),
        ("file.open", json!({})),
        ("file.save", json!({})),
        ("run.selected", json!({})),
    ] {
        let reply = client.ask(2, "command.run", json!({"id":id,"params":params}));
        // File commands need desktop registration; absent there is NotFound, never dispatch.
        let expected = if id.starts_with("file.") || id == "run.selected" { "not_found" } else { "unsupported" };
        assert_eq!(reply["error"]["kind"], expected, "{reply}");
        assert!(!reply.to_string().contains("fixture-not-a-secret"));
    }
    let screenshot = client.ask(3, "screenshot", json!({}));
    assert_eq!(screenshot["error"]["kind"], "unsupported");
    assert!(screenshot["error"]["message"].as_str().unwrap().contains("desktop"));
    assert_eq!(client.ask(4, "state.get", json!({}))["result"], before["result"]);
    client.finish();
}

#[test]
fn broken_stdout_and_unknown_options_fail_without_panicking() {
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).arg("control").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(b"{\"id\":1,\"method\":\"hello\"}\n").unwrap();
    assert!(!wait(&mut child).success());
    let mut error = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut error).unwrap();
    assert!(error.contains("control stdio I/O failed"), "{error}");
    assert!(!error.contains("panicked"));
    let output = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["control", "--token"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("takes no options"));
}
