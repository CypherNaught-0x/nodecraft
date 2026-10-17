use super::*;
use crate::blank_session;
use nodecraft_types::{CanvasId, ProjectId};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

const CANVAS: CanvasId = CanvasId::from_bytes([2; 16]);

fn token() -> Token {
    Token::from_bytes([7; 32])
}

fn session() -> Session {
    blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap()
}

#[test]
fn tokens_are_random_hex_compared_whole_and_never_printed() {
    let a = Token::generate().unwrap();
    let b = Token::generate().unwrap();
    assert_ne!(a, b);
    assert_eq!(a.to_hex().len(), 64);
    assert!(a.matches(&a.to_hex()));
    assert!(!a.matches(&b.to_hex()));
    assert!(!a.matches(&a.to_hex()[..63]));
    assert!(!a.matches(""));
    assert_eq!(format!("{a:?}"), "Token(…)");
}

#[test]
fn a_connection_must_say_hello_with_the_token_first() {
    let mut c = Connection::new(token());
    assert!(matches!(c.step(r#"{"id":1,"method":"state.get"}"#), Step::Close(_)), "no request before hello");
    let mut c = Connection::new(token());
    assert!(matches!(c.step(r#"{"id":1,"method":"hello","params":{"token":"nope"}}"#), Step::Close(_)));
    let mut c = Connection::new(token());
    assert!(matches!(c.step("not json"), Step::Close(_)));
    let mut c = Connection::new(token());
    let hello = format!(r#"{{"id":1,"method":"hello","params":{{"token":"{}"}}}}"#, token().to_hex());
    let Step::Reply(reply) = c.step(&hello) else { panic!("hello refused") };
    assert_eq!(reply["result"]["protocol"], PROTOCOL);
    assert!(matches!(c.step(r#"{"id":2,"method":"state.get"}"#), Step::Ask(id, Request::State) if id == 2));
    assert!(matches!(c.step(r#"{"id":"x","method":"screenshot"}"#), Step::Ask(_, Request::Screenshot)));
    assert!(matches!(c.step(r#"{"id":3,"method":"rm"}"#), Step::Reply(r) if r["error"]["kind"] == "not_found"));
    assert!(matches!(c.step(r#"{"id":4,"method":"command.run","params":[1]}"#), Step::Reply(r) if r["error"]["kind"] == "invalid_input"));
    assert!(matches!(c.step("garbage"), Step::Reply(_)), "once authenticated, bad lines don't close the connection");
}

#[test]
fn the_ui_thread_answers_through_the_shared_session_contract() {
    let mut s = session();
    let list = answer(&mut s, &Request::ListCommands(Map::new())).unwrap().unwrap();
    let ids: Vec<&str> = list["commands"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"node.add") && !ids.contains(&"keys.set"), "the automation classification applies");
    let run = |s: &mut Session, args: Value| answer(s, &Request::RunCommand(args.as_object().unwrap().clone())).unwrap();
    assert!(run(&mut s, json!({"id": "keys.set", "params": {"account": "openai", "key": "sk-x"}})).is_err(), "people-only commands are refused");
    let node = json!({"id": "11111111-1111-4111-8111-111111111111", "kind": "source.text"});
    let added = run(&mut s, json!({"id": "node.add", "params": {"canvas": CANVAS, "node": node}})).unwrap();
    assert_eq!(added["revision"], 1);
    let state = answer(&mut s, &Request::State).unwrap().unwrap();
    assert_eq!((state["canvases"][0]["nodes"].as_u64(), state["can_undo"].as_bool()), (Some(1), Some(true)));
    assert!(answer(&mut s, &Request::Screenshot).is_none(), "the window takes screenshots");
}

#[test]
fn screenshots_are_png_in_base64() {
    let rgba = vec![200u8; 4 * 3 * 4];
    let shot = screenshot_answer(4, 3, &rgba).unwrap();
    assert_eq!((shot["mime"].as_str(), shot["width"].as_u64()), (Some("image/png"), Some(4)));
    assert!(shot["data"].as_str().unwrap().starts_with("iVBORw0KGgo"), "a PNG signature");
    assert!(screenshot_answer(4, 3, &rgba[1..]).is_err());
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64(b"fo"), "Zm8=");
}

#[test]
fn token_files_are_private() {
    let dir = std::env::temp_dir().join(format!("nodecraft-control-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("control.json");
    let file = TokenFile { port: 4242, token: token().to_hex(), pid: 1 };
    file.write(&path).unwrap();
    file.write(&path).unwrap();
    let back: TokenFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(back, file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A stand-in for the app's UI thread: answers calls against a session until told to stop.
fn ui_thread(calls: std::sync::mpsc::Receiver<Call>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut s = session();
        while let Ok(call) = calls.recv_timeout(Duration::from_secs(20)) {
            match answer(&mut s, &call.request) {
                Some(result) => call.reply(result),
                None => call.reply(screenshot_answer(2, 2, &[9; 16])),
            }
        }
    })
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Client {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        Self { reader: BufReader::new(stream.try_clone().unwrap()), writer: stream }
    }
    fn ask(&mut self, message: Value) -> Value {
        writeln!(self.writer, "{message}").unwrap();
        self.read()
    }
    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or(Value::Null)
    }
}

#[test]
fn a_client_drives_the_app_over_tcp() {
    let woken = Arc::new(AtomicUsize::new(0));
    let w = Arc::clone(&woken);
    let listener = listen(
        token(),
        Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    assert!(listener.addr.ip().is_loopback(), "local only");
    let ui = ui_thread(listener.calls);
    let mut c = Client::connect(listener.addr);
    let hello = c.ask(json!({"id": 1, "method": "hello", "params": {"token": token().to_hex()}}));
    assert_eq!(hello["result"]["app"], "nodecraft");
    let node = json!({"id": "11111111-1111-4111-8111-111111111111", "kind": "source.text"});
    let added = c.ask(json!({"id": 2, "method": "command.run", "params": {"id": "node.add", "params": {"canvas": CANVAS, "node": node}}}));
    assert_eq!(added["result"]["revision"], 1, "{added}");
    let state = c.ask(json!({"id": 3, "method": "state.get"}));
    assert_eq!(state["result"]["canvases"][0]["nodes"], 1);
    let shot = c.ask(json!({"id": 4, "method": "screenshot"}));
    assert_eq!(shot["result"]["mime"], "image/png");
    let refused = c.ask(json!({"id": 5, "method": "command.run", "params": {"id": "keys.set", "params": {"account": "openai", "key": "sk-secret-1"}}}));
    assert!(refused["error"].is_object() && !refused.to_string().contains("sk-secret"), "{refused}");
    assert!(woken.load(Ordering::SeqCst) >= 4, "the app is woken for each request");

    // A wrong token is answered once, then the connection closes.
    let mut intruder = Client::connect(listener.addr);
    let denied = intruder.ask(json!({"id": 1, "method": "hello", "params": {"token": "0".repeat(64)}}));
    assert_eq!(denied["error"]["kind"], "authentication");
    assert_eq!(intruder.read(), Value::Null, "closed");
    drop(c);
    drop(intruder);
    let _ = ui;
}

#[test]
fn connections_are_limited_and_long_lines_skipped() {
    let listener = listen(token(), Arc::new(|| {})).unwrap();
    let _ui = ui_thread(listener.calls);
    let mut held: Vec<Client> = (0..MAX_CONNECTIONS).map(|_| Client::connect(listener.addr)).collect();
    for c in &mut held {
        assert!(c.ask(json!({"id": 1, "method": "hello", "params": {"token": token().to_hex()}}))["result"].is_object());
    }
    let mut extra = Client::connect(listener.addr);
    assert_eq!(extra.read()["error"]["kind"], "busy");
    // A line over the limit is refused and the connection carries on.
    let c = held.first_mut().unwrap();
    let long = format!("{}\n", "x".repeat(MAX_MESSAGE_BYTES + 1));
    c.writer.write_all(long.as_bytes()).unwrap();
    assert_eq!(c.read()["error"]["kind"], "invalid_input");
    assert_eq!(c.ask(json!({"id": 9, "method": "state.get"}))["id"], 9);
    // Closing one frees a slot.
    held.pop();
    std::thread::sleep(Duration::from_millis(200));
    let mut again = Client::connect(listener.addr);
    assert!(again.ask(json!({"id": 1, "method": "hello", "params": {"token": token().to_hex()}}))["result"].is_object());
}
