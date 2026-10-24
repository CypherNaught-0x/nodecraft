//! Drive the actual CLI pipes: no keychain, provider, socket or native window.
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

// Every process creation in this test binary must use spawn_cli: on platforms with
// non-atomic pipe/CLOEXEC setup, a concurrent spawn could inherit an unwanted reader.
static SPAWN_OWNERSHIP: Mutex<()> = Mutex::new(());
static CAPTURE_ID: AtomicU64 = AtomicU64::new(0);
const DIAGNOSTIC_CAP: u64 = 64 * 1024;

struct Capture {
    file: File,
    path: PathBuf,
}
impl Capture {
    fn new() -> Self {
        Self::with_candidates(|| {
            let id = CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!("nodecraft-control-stdio-{}-{id}", std::process::id()))
        })
        .expect("create isolated diagnostic capture")
    }
    fn with_candidates(mut candidate: impl FnMut() -> PathBuf) -> std::io::Result<Self> {
        // A killed earlier process can leave captures behind, and PIDs are reusable.
        // Only collisions justify trying another name; never touch an unknown file.
        for _ in 0..32 {
            let path = candidate();
            match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Self { file, path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "diagnostic capture name candidates exhausted after 32 collisions"))
    }
    fn bytes(&mut self) -> Vec<u8> {
        self.file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.file).take(DIAGNOSTIC_CAP + 1).read_to_end(&mut bytes).unwrap();
        bytes
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct OwnedChild {
    child: Child,
    stdout_capture: Option<Capture>,
    stderr_capture: Option<Capture>,
}
impl Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.child.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl OwnedChild {
    fn diagnostics(&mut self) -> (Vec<u8>, Vec<u8>) {
        let stdout = self.stdout_capture.as_mut().map_or_else(Vec::new, Capture::bytes);
        let stderr = self.stderr_capture.as_mut().map_or_else(Vec::new, Capture::bytes);
        // Callers capture status and both streams before making assertions.
        eprintln!(
            "control child status={:?} stdout={:?} stderr={:?}",
            self.child.try_wait().unwrap(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        assert!(stdout.len() as u64 <= DIAGNOSTIC_CAP && stderr.len() as u64 <= DIAGNOSTIC_CAP, "diagnostic capture exceeded cap");
        (stdout, stderr)
    }
}

#[derive(Clone, Copy)]
enum Readers {
    Protocol,
    CloseStdout,
    CloseBoth,
    Capture,
}

fn spawn_cli(args: &[&str], readers: Readers) -> OwnedChild {
    let stdout_capture = matches!(readers, Readers::Capture).then(Capture::new);
    let stderr_capture = (!matches!(readers, Readers::CloseBoth)).then(Capture::new);
    let mut command = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli"));
    command.args(args).stdin(Stdio::piped());
    command.stdout(stdout_capture.as_ref().map_or_else(Stdio::piped, |capture| Stdio::from(capture.file.try_clone().unwrap())));
    command.stderr(stderr_capture.as_ref().map_or_else(Stdio::piped, |capture| Stdio::from(capture.file.try_clone().unwrap())));
    let guard = SPAWN_OWNERSHIP.lock().expect("CLI spawn ownership lock poisoned");
    // Establish cleanup before any subsequent fallible operation or assertion.
    let mut child = OwnedChild { child: command.spawn().expect("spawn control CLI"), stdout_capture, stderr_capture };
    if matches!(readers, Readers::CloseStdout | Readers::CloseBoth) {
        drop(child.stdout.take());
    }
    if matches!(readers, Readers::CloseBoth) {
        drop(child.stderr.take());
    }
    drop(guard);
    child
}

struct Client {
    child: OwnedChild,
    input: Option<ChildStdin>,
    replies: Receiver<String>,
}
impl Client {
    fn new() -> Self {
        let mut child = spawn_cli(&["control"], Readers::Protocol);
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
        let status = wait(&mut self.child);
        let (_, stderr) = self.child.diagnostics();
        assert!(status.success(), "{status:?}");
        assert!(self.replies.recv_timeout(Duration::from_secs(5)).is_err(), "no extra protocol output");
        assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.input.take();
        // OwnedChild kills and reaps before diagnostic capture cleanup.
    }
}
fn wait(child: &mut OwnedChild) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait();
            let (stdout, stderr) = child.diagnostics();
            panic!("control did not exit after EOF/broken stdout: {status:?}, stdout={stdout:?}, stderr={stderr:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
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
        ("canvas.prompt_overlays_toggle", json!({})),
        ("canvas.pan_to", json!({"x":0,"y":0})),
        ("keys.set", json!({"account":"test","key":"fixture-not-a-secret"})),
        ("keys.remove", json!({"account":"test"})),
        ("file.open", json!({})),
        ("file.save", json!({})),
        ("run.selected", json!({})),
    ] {
        let reply = client.ask(2, "command.run", json!({"id":id,"params":params}));
        // Persistence file commands are builtins but human-only; generation is not registered.
        let expected = if id == "run.selected" { "not_found" } else { "unsupported" };
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
    let mut child = spawn_cli(&["control"], Readers::CloseStdout);
    child.stdin.take().unwrap().write_all(b"{\"id\":1,\"method\":\"hello\"}\n").unwrap();
    let status = wait(&mut child);
    let (_, error) = child.diagnostics();
    let error = String::from_utf8_lossy(&error);
    assert_eq!(status.code(), Some(1), "{status:?}: {error}");
    assert!(error.contains("control stdio I/O failed"), "{error}");
    assert!(!error.contains("panicked"));
    let mut unknown = spawn_cli(&["control", "--token"], Readers::Capture);
    unknown.stdin.take();
    let status = wait(&mut unknown);
    let (stdout, stderr) = unknown.diagnostics();
    assert_eq!(status.code(), Some(1), "{status:?}");
    assert!(stdout.is_empty());
    assert!(String::from_utf8_lossy(&stderr).contains("takes no options"));
}

#[test]
fn closed_stdout_and_stderr_exit_with_io_failure_instead_of_panicking() {
    let mut child = spawn_cli(&["control"], Readers::CloseBoth);
    // Selected readers were closed under the spawn guard, before this input.
    child.stdin.take().unwrap().write_all(b"{\"id\":1,\"method\":\"hello\"}\n").unwrap();
    let status = wait(&mut child);
    child.diagnostics();
    assert_eq!(status.code(), Some(1), "I/O failure must use the normal failure exit, not Rust's panic exit");
}

#[test]
fn capture_collision_preserves_stale_bytes_and_uses_next_candidate() {
    let mut stale = Capture::new();
    stale.file.write_all(b"stale-capture-must-survive").unwrap();
    // Reserve a distinct fresh candidate without reusing any unknown pathname.
    let next = Capture::new();
    let next_path = next.path.clone();
    drop(next);
    let mut attempts = 0;
    let selected = Capture::with_candidates(|| {
        attempts += 1;
        if attempts == 1 { stale.path.clone() } else { next_path.clone() }
    })
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(selected.path, next_path);
    assert_eq!(std::fs::read(&stale.path).unwrap(), b"stale-capture-must-survive");
    drop(selected);
    assert!(!next_path.exists(), "only the selected owned capture is removed");
    assert_eq!(std::fs::read(&stale.path).unwrap(), b"stale-capture-must-survive");
}

#[test]
fn capture_exhaustion_and_noncollision_errors_are_bounded() {
    let mut stale = Capture::new();
    stale.file.write_all(b"existing-owner").unwrap();
    let mut attempts = 0;
    let result = Capture::with_candidates(|| {
        attempts += 1;
        stale.path.clone()
    });
    let error = result.err().expect("all occupied candidates must fail explicitly");
    assert_eq!(attempts, 32);
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(error.to_string().contains("exhausted"));
    assert_eq!(std::fs::read(&stale.path).unwrap(), b"existing-owner");
    let mut attempts = 0;
    let result = Capture::with_candidates(|| {
        attempts += 1;
        // A regular file cannot be a directory on any supported platform.
        stale.path.join("child")
    });
    let error = result.err().expect("noncollision I/O error must be returned");
    assert_ne!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(attempts, 1, "do not retry permission/storage/path errors");
    assert_eq!(std::fs::read(&stale.path).unwrap(), b"existing-owner");
}
