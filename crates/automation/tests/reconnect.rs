//! The control client replacing its connection: at the app's connection limit, after a lost
//! reply (never resending a request that may have run), with a rotated token file and against an
//! app speaking another protocol.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nodecraft_automation::blank_session;
use nodecraft_automation::control::{self, Client, MAX_CONNECTIONS, Token, TokenFile};
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
    // No slots leaked: free the others and fill the app again.
    drop(others);
    std::thread::sleep(Duration::from_millis(200));
    let again: Vec<Client> = (1..MAX_CONNECTIONS).map(|_| Client::connect(addr, &token.to_hex()).unwrap()).collect();
    assert_eq!(again.len(), MAX_CONNECTIONS - 1);
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
    assert!(lost.is_err(), "the reply was lost: the call fails once");
    // The next call reconnects and runs only itself.
    assert!(client.call("state.get", Map::new()).is_ok());
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the mutation ran once and was never resent");
    let methods = seen.lock().unwrap().clone();
    assert_eq!(methods, ["hello", "command.run", "hello", "state.get"]);
}

#[test]
fn a_rotated_token_file_is_followed_on_reconnect() {
    let dir = std::env::temp_dir().join(format!("nodecraft-rotate-{}", std::process::id()));
    let private = control::private_dir(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = private.join("control.json");
    let (first, first_token) = app(3);
    TokenFile { port: first.port(), token: first_token.to_hex(), pid: 1 }.write(&path).unwrap();
    let mut client = Client::from_token_file(&path).unwrap();
    assert_eq!(canvas_of(&mut client), json!(CanvasId::from_bytes([3; 16])));
    // The app restarts: new port, new token.
    let (second, second_token) = app(4);
    TokenFile { port: second.port(), token: second_token.to_hex(), pid: 2 }.write(&path).unwrap();
    client.set_reconnect_after(Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(canvas_of(&mut client), json!(CanvasId::from_bytes([4; 16])), "reconnected to the restarted app");
    // Repeated reconnects keep working.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(canvas_of(&mut client), json!(CanvasId::from_bytes([4; 16])));
    }
    // A token file that's gone: an actionable error, and the next call tries again.
    std::fs::remove_file(&path).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let gone = client.call("state.get", Map::new()).unwrap_err();
    assert!(gone.message.contains("--control"), "{}", gone.message);
    TokenFile { port: second.port(), token: second_token.to_hex(), pid: 2 }.write(&path).unwrap();
    assert!(client.call("state.get", Map::new()).is_ok(), "recovered once the file is back");
    std::fs::remove_dir_all(&dir).unwrap();
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
