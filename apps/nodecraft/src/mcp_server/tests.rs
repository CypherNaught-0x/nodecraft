//! The built-in server end to end: a real HTTP client, the window's session answering on the
//! test's thread as the shell does each frame.
use super::*;
use nodecraft_engine::secrets::{MemoryStore, SecretError};
use nodecraft_ui_egui::NodeCraftApp;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn server(secrets: Arc<dyn SecretStore>) -> Server {
    // Port 0 picks a free one, so tests never collide with a running app.
    Server::new(0, secrets, Arc::new(|| {}))
}

/// Polls until starting is over, answering nothing.
fn started(s: &mut Server) -> McpState {
    let until = Instant::now() + Duration::from_secs(10);
    while s.state() == McpState::Starting && Instant::now() < until {
        assert!(s.poll().is_empty());
        std::thread::sleep(Duration::from_millis(10));
    }
    s.state()
}

/// One POST on a new connection; the reply's status, session header and JSON body.
fn post(url: &str, token: &str, session: Option<&str>, body: &Value) -> (u16, Option<String>, Value) {
    let authority = url.trim_start_matches("http://").trim_end_matches("/mcp");
    let mut stream = std::net::TcpStream::connect(authority).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let body = body.to_string();
    let session = session.map(|s| format!("Mcp-Session-Id: {s}\r\n")).unwrap_or_default();
    write!(stream, "POST /mcp HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\n{session}Connection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let text = String::from_utf8(reply).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let session = head.lines().find_map(|l| l.strip_prefix("Mcp-Session-Id: ")).map(str::to_owned);
    (status, session, serde_json::from_str(body).unwrap_or(Value::Null))
}

#[test]
fn turning_it_on_serves_the_windows_session_and_turning_it_off_stops_it() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let mut s = server(Arc::clone(&secrets));
    assert_eq!(s.state(), McpState::Off);
    s.set_enabled(true);
    let McpState::Running { url } = started(&mut s) else { panic!("{:?}", s.state()) };
    assert!(url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp"), "{url}");
    let token = s.token().unwrap();
    let account = Account::new(MCP_TOKEN_ACCOUNT).unwrap();
    assert_eq!(secrets.get(&account).unwrap().unwrap().expose(), token, "kept in the keychain");
    assert_eq!(s.note(), None);

    // The client works on another thread; this one is the window.
    let client = {
        let (url, token) = (url.clone(), token.clone());
        std::thread::spawn(move || {
            let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}});
            let (status, session, _) = post(&url, &token, None, &init);
            assert_eq!(status, 200);
            let session = session.unwrap();
            let canvas = post(
                &url,
                &token,
                Some(&session),
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "canvas_inspect", "arguments": {}}}),
            )
            .2;
            let canvas = canvas["result"]["structuredContent"]["canvases"][0]["id"].clone();
            let node = "00000000-0000-4000-8000-0000000000aa";
            let add = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "command_run", "arguments": {"id": "node.add", "params": {"canvas": canvas, "node": {"id": node, "kind": "text"}}}}});
            let added = post(&url, &token, Some(&session), &add).2;
            assert_eq!(added["result"]["isError"], false, "{added}");
            let set = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "command_run", "arguments": {"id": "automation.mcp_server_set", "params": {"enabled": false}}}});
            let refused = post(&url, &token, Some(&session), &set).2;
            assert_eq!(refused["result"]["isError"], true, "only a person turns it off: {refused}");
            assert_eq!(post(&url, "wrong-token-wrong-token-wrong-token", Some(&session), &set).0, 401);
        })
    };
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let mut pump = crate::control::CallPump::new(Default::default());
    let until = Instant::now() + Duration::from_secs(30);
    while !client.is_finished() && Instant::now() < until {
        pump.pump(s.poll(), &mut app, &ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    client.join().unwrap();
    let canvas = app.session.document().canvases().values().next().unwrap();
    let node: nodecraft_types::NodeId = "00000000-0000-4000-8000-0000000000aa".parse().unwrap();
    assert!(canvas.nodes().contains_key(&node), "the agent's card is on the window's canvas");

    let authority = url.trim_start_matches("http://").trim_end_matches("/mcp").to_owned();
    s.set_enabled(false);
    assert_eq!((s.state(), s.token()), (McpState::Off, None));
    // The port is let go: binding it again works once the accept loop has seen the stop.
    let until = Instant::now() + Duration::from_secs(5);
    while std::net::TcpListener::bind(&authority).is_err() {
        assert!(Instant::now() < until, "the port stayed taken");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_token_survives_restarts_until_a_new_one_is_asked_for() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let mut s = server(Arc::clone(&secrets));
    s.set_enabled(true);
    started(&mut s);
    let first = s.token().unwrap();
    s.set_enabled(false);
    s.set_enabled(true);
    started(&mut s);
    assert_eq!(s.token().unwrap(), first, "clients keep working after a restart");
    s.new_token();
    assert!(matches!(started(&mut s), McpState::Running { .. }));
    let second = s.token().unwrap();
    assert_ne!(second, first);
    assert_eq!(secrets.get(&Account::new(MCP_TOKEN_ACCOUNT).unwrap()).unwrap().unwrap().expose(), second);
    s.stop();
}

struct NoKeychain;
impl SecretStore for NoKeychain {
    fn get(&self, _: &Account) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable("no keyring".into()))
    }
    fn set(&self, _: &Account, _: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("no keyring".into()))
    }
    fn delete(&self, _: &Account) -> Result<bool, SecretError> {
        Ok(false)
    }
}

#[test]
fn without_a_keychain_the_token_lasts_for_the_session_and_a_taken_port_is_reported() {
    let mut s = server(Arc::new(NoKeychain));
    s.set_enabled(true);
    assert!(matches!(started(&mut s), McpState::Running { .. }));
    assert!(s.note().unwrap().contains("lasts until NodeCraft closes"));
    assert!(s.token().is_some());
    s.stop();

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut s = Server::new(taken.local_addr().unwrap().port(), Arc::new(MemoryStore::default()), Arc::new(|| {}));
    s.set_enabled(true);
    let McpState::Failed(why) = started(&mut s) else { panic!("{:?}", s.state()) };
    assert!(why.contains("can't listen"), "{why}");
    assert!(s.enabled(), "the choice stands; the next start tries again");
}

/// A keychain that holds a token but can't be read right now.
#[derive(Default)]
struct Locked {
    writes: std::sync::Mutex<usize>,
}
impl SecretStore for Locked {
    fn get(&self, _: &Account) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Denied("unlock the keychain".into()))
    }
    fn set(&self, _: &Account, _: &Secret) -> Result<(), SecretError> {
        *self.writes.lock().unwrap() += 1;
        Ok(())
    }
    fn delete(&self, _: &Account) -> Result<bool, SecretError> {
        Ok(false)
    }
}

#[test]
fn a_keychain_that_cant_be_read_keeps_its_stored_token() {
    let locked = Arc::new(Locked::default());
    let mut s = server(Arc::clone(&locked) as Arc<dyn SecretStore>);
    s.set_enabled(true);
    assert!(matches!(started(&mut s), McpState::Running { .. }));
    assert_eq!(*locked.writes.lock().unwrap(), 0, "the stored token isn't overwritten");
    let note = s.note().unwrap();
    assert!(note.contains("couldn't be read") && note.contains("lasts until NodeCraft closes"), "{note}");
    s.stop();
}

#[test]
fn a_new_token_rebinds_the_same_port_at_once() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let mut s = Server::new(port, Arc::new(MemoryStore::default()), Arc::new(|| {}));
    s.set_enabled(true);
    assert!(matches!(started(&mut s), McpState::Running { .. }));
    for _ in 0..3 {
        s.new_token();
        let state = started(&mut s);
        assert!(matches!(state, McpState::Running { .. }), "{state:?}");
    }
    s.stop();
}
