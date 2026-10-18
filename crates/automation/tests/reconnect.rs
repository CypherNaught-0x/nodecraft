//! The control client replacing its connection: at the app's connection limit, after a lost
//! reply (never resending a request that may have run), with a rotated token file and against an
//! app speaking another protocol.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nodecraft_automation::blank_session;
use nodecraft_automation::control::{self, Client, MAX_CONNECTIONS, RECONNECT_ATTEMPTS, Token, TokenFile};
use nodecraft_types::{CanvasId, ErrorKind, ProjectId};
use serde_json::{Map, Value, json};

/// A stand-in app on its own session; `canvas` tells two apps apart.
fn app(canvas: u8) -> (SocketAddr, Token) {
    let token = Token::generate().unwrap();
    let listener = control::listen(token.clone(), Arc::new(|| {})).unwrap();
    let addr = listener.addr;
    std::thread::spawn(move || {
        let mut session = blank_session(ProjectId::from_bytes([1; 16]), CanvasId::from_bytes([canvas; 16])).unwrap();
        while let Ok(call) = listener.calls.recv_timeout(Duration::from_secs(60)) {
            if let Some(result) = control::answer(&mut session, &call.request) {
                call.reply(result);
            }
        }
    });
    (addr, token)
}

fn canvas_of(client: &mut Client) -> Value {
    client.call("state.get", Map::new()).unwrap()["canvases"][0]["id"].clone()
}

#[test]
fn reconnecting_at_the_connection_limit_needs_no_spare_slot() {
    let (addr, token) = app(2);
    let mut bridge = Client::connect(addr, &token.to_hex()).unwrap();
    bridge.set_reconnect_after(Duration::from_millis(50));
    // The other slots are held by live clients.
    let others: Vec<Client> = (1..MAX_CONNECTIONS).map(|_| Client::connect(addr, &token.to_hex()).unwrap()).collect();
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(80));
        // Due a proactive reconnect every time: it must succeed without a fifth slot.
        assert!(bridge.call("state.get", Map::new()).is_ok());
    }
    // The cap still holds: a fifth connection is refused. (Usually as busy; on a loaded machine
    // the refusal can arrive as a reset, which reconnects retry just the same.)
    let fifth = Client::connect(addr, &token.to_hex()).err().map(|e| e.kind);
    assert!(matches!(fifth, Some(ErrorKind::Busy | ErrorKind::Network)), "{fifth:?}");
    // No slots leaked: free the others, and the app takes three new ones (it frees slots a moment
    // after the clients close, so poll with a deadline).
    drop(others);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut again = Vec::new();
    while again.len() < MAX_CONNECTIONS - 1 {
        match Client::connect(addr, &token.to_hex()) {
            Ok(c) => again.push(c),
            Err(e) => {
                assert!(matches!(e.kind, ErrorKind::Busy | ErrorKind::Network), "{e:?}");
                assert!(std::time::Instant::now() < deadline, "slots were never freed");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    assert!(bridge.call("state.get", Map::new()).is_ok());
}

/// A fake app that runs every request it reads (counting them), then answers all but the first
/// `command.run`: that one it executes and hangs up on without replying.
fn flaky_app() -> (SocketAddr, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<String>>>) {
    let socket = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = socket.local_addr().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (r, s) = (Arc::clone(&runs), Arc::clone(&seen));
    std::thread::spawn(move || {
        let mut dropped = false;
        for stream in socket.incoming().take(4) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                s.lock().unwrap().push(method.clone());
                let result = match method.as_str() {
                    "hello" => json!({"app": "nodecraft", "version": "0", "protocol": control::PROTOCOL}),
                    "command.run" => {
                        r.fetch_add(1, Ordering::SeqCst);
                        if !dropped {
                            // It ran; the reply is lost.
                            dropped = true;
                            break;
                        }
                        json!({"result": {}, "revision": 1})
                    }
                    _ => json!({"canvases": []}),
                };
                writeln!(stream, "{}", json!({"id": request["id"], "result": result})).unwrap();
            }
        }
    });
    (addr, runs, seen)
}

#[test]
fn a_request_that_may_have_run_is_never_resent() {
    let (addr, runs, seen) = flaky_app();
    let mut client = Client::connect(addr, "any").unwrap();
    let add = json!({"id": "node.add", "params": {}}).as_object().unwrap().clone();
    let lost = client.call("command.run", add);
    assert_eq!(lost.err().map(|e| e.kind), Some(ErrorKind::Network), "the reply was lost: the call fails once");
    // The next call reconnects and runs only itself.
    assert!(client.call("state.get", Map::new()).is_ok());
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the mutation ran once and was never resent");
    let methods = seen.lock().unwrap().clone();
    assert_eq!(methods, ["hello", "command.run", "hello", "state.get"]);
}

/// Removes a temporary folder when the test ends, even on failure.
struct TempDir(std::path::PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn token_dir(name: &str) -> (TempDir, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("nodecraft-{name}-{}", std::process::id()));
    let private = control::private_dir(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    (TempDir(dir), private.join("control.json"))
}

#[test]
fn a_rotated_token_file_is_followed_on_reconnect() {
    let (_guard, path) = token_dir("rotate");
    let (first, first_token) = app(3);
    TokenFile { port: first.port(), token: first_token.to_hex(), pid: 1 }.write(&path).unwrap();
    let mut client = Client::from_token_file(&path).unwrap();
    assert_eq!(canvas_of(&mut client), json!(CanvasId::from_bytes([3; 16])));
    // The app restarts: new port, new token.
    let (second, second_token) = app(4);
    TokenFile { port: second.port(), token: second_token.to_hex(), pid: 2 }.write(&path).unwrap();
    client.set_reconnect_after(Duration::ZERO);
    assert_eq!(canvas_of(&mut client), json!(CanvasId::from_bytes([4; 16])), "reconnected to the restarted app");
    for _ in 0..3 {
        assert_eq!(canvas_of(&mut client), json!(CanvasId::from_bytes([4; 16])), "repeated reconnects keep working");
    }
    // The token file is gone: an actionable error, and the next call tries again.
    std::fs::remove_file(&path).unwrap();
    let gone = client.call("state.get", Map::new()).unwrap_err();
    assert!(gone.message.contains("--control"), "{}", gone.message);
    TokenFile { port: second.port(), token: second_token.to_hex(), pid: 2 }.write(&path).unwrap();
    assert!(client.call("state.get", Map::new()).is_ok(), "recovered once the file is back");
}

/// A fake app that refuses every connection as busy, counting the `hello`s it sees and any other
/// request.
fn always_busy() -> (SocketAddr, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let socket = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = socket.local_addr().unwrap();
    let (hellos, others) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (h, o) = (Arc::clone(&hellos), Arc::clone(&others));
    std::thread::spawn(move || {
        for stream in socket.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            if BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let method = serde_json::from_str::<Value>(&line).ok().and_then(|v| v["method"].as_str().map(str::to_owned)).unwrap_or_default();
            if method == "hello" { &h } else { &o }.fetch_add(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"id": null, "error": {"kind": "busy", "message": "too many control connections"}}));
        }
    });
    (addr, hellos, others)
}

#[test]
fn a_busy_app_gets_a_bounded_number_of_handshakes() {
    let (_guard, path) = token_dir("busy");
    let (good, good_token) = app(6);
    TokenFile { port: good.port(), token: good_token.to_hex(), pid: 1 }.write(&path).unwrap();
    let mut client = Client::from_token_file(&path).unwrap();
    // The app is replaced by one that is always full.
    let (busy, hellos, others) = always_busy();
    TokenFile { port: busy.port(), token: good_token.to_hex(), pid: 2 }.write(&path).unwrap();
    client.set_reconnect_after(Duration::ZERO);
    let started = std::time::Instant::now();
    let refused = client.call("command.run", Map::new()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Busy, "{refused:?}");
    assert!(started.elapsed() < Duration::from_secs(3), "bounded: {:?}", started.elapsed());
    assert_eq!(hellos.load(Ordering::SeqCst), RECONNECT_ATTEMPTS as usize, "exactly the attempts allowed");
    assert_eq!(others.load(Ordering::SeqCst), 0, "the request was never sent");
    // The next call tries again, as boundedly.
    assert!(client.call("state.get", Map::new()).is_err());
    assert_eq!(hellos.load(Ordering::SeqCst), 2 * RECONNECT_ATTEMPTS as usize);
}

/// A fake app that speaks protocol 99.
fn other_protocol() -> (SocketAddr, Arc<AtomicUsize>) {
    let socket = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = socket.local_addr().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let s = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in socket.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                s.fetch_add(1, Ordering::SeqCst);
                let id = serde_json::from_str::<Value>(&line).unwrap()["id"].clone();
                let _ = writeln!(stream, "{}", json!({"id": id, "result": {"app": "nodecraft", "protocol": 99}}));
            }
        }
    });
    (addr, seen)
}

#[test]
fn a_replacement_speaking_another_protocol_is_refused_once() {
    let (_guard, path) = token_dir("proto");
    let (good, good_token) = app(7);
    TokenFile { port: good.port(), token: good_token.to_hex(), pid: 1 }.write(&path).unwrap();
    let mut client = Client::from_token_file(&path).unwrap();
    let (other, seen) = other_protocol();
    TokenFile { port: other.port(), token: good_token.to_hex(), pid: 2 }.write(&path).unwrap();
    client.set_reconnect_after(Duration::ZERO);
    let refused = client.call("command.run", Map::new()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert_eq!(seen.load(Ordering::SeqCst), 1, "one hello, not retried, and the request never sent");
}

#[test]
fn an_app_speaking_another_protocol_is_refused() {
    let socket = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = socket.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Some(Ok(mut stream)) = socket.incoming().next() {
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            let id = serde_json::from_str::<Value>(&line).unwrap()["id"].clone();
            writeln!(stream, "{}", json!({"id": id, "result": {"app": "nodecraft", "protocol": 99}})).unwrap();
        }
    });
    let refused = Client::connect(addr, "any").err().unwrap();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
}
