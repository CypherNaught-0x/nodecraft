//! MCP over Streamable HTTP (#116), driven by a real HTTP client over loopback TCP.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use nodecraft_automation::blank_session;
use nodecraft_automation::control;
use nodecraft_automation::mcp::McpServer;
use nodecraft_automation::mcp::http::{self, BearerToken, Config, MAX_CONNECTIONS, Stopper, Timeouts};
use nodecraft_types::{CanvasId, ProjectId};
use serde_json::{Value, json};

const CANVAS: CanvasId = CanvasId::from_bytes([2; 16]);

fn token() -> BearerToken {
    BearerToken::new(&"t0ken".repeat(8)).unwrap()
}

fn ids() -> nodecraft_automation::mcp::IdSource {
    let mut n: u64 = 0;
    Box::new(move || {
        n += 1;
        let mut b = [7u8; 16];
        b[..8].copy_from_slice(&n.to_le_bytes());
        b
    })
}

/// A headless server over HTTP on a free loopback port, served on its own thread.
fn start_with(timeouts: Timeouts) -> (SocketAddr, Stopper, std::thread::JoinHandle<()>) {
    let mut config = Config::new("127.0.0.1:0".parse().unwrap(), token());
    config.timeouts = timeouts;
    let listening = http::bind(config).unwrap();
    let (addr, stopper) = (listening.addr(), listening.stopper());
    let serving = std::thread::spawn(move || {
        let mut server = McpServer::new(blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap(), ids());
        listening.serve(&mut |m| server.handle_message(m));
    });
    (addr, stopper, serving)
}

fn start() -> (SocketAddr, Stopper, std::thread::JoinHandle<()>) {
    start_with(Timeouts::default())
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", String::from_utf8_lossy(&self.body)))
    }
}

/// Reads one response off `stream`.
fn read_reply(stream: &mut TcpStream) -> Reply {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        assert_eq!(stream.read(&mut byte).unwrap(), 1, "connection closed mid-head: {:?}", String::from_utf8_lossy(&buf));
        buf.push(byte[0]);
    }
    let head = String::from_utf8(buf).unwrap();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> =
        lines.filter(|l| !l.is_empty()).map(|l| l.split_once(':').unwrap()).map(|(n, v)| (n.to_owned(), v.trim().to_owned())).collect();
    let length: usize = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("content-length")).unwrap().1.parse().unwrap();
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).unwrap();
    Reply { status, headers, body }
}

/// A client on one keep-alive connection.
struct Client {
    stream: TcpStream,
    addr: SocketAddr,
    session: Option<String>,
}

impl Client {
    fn new(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        Self { stream, addr, session: None }
    }

    fn raw(&mut self, request: &[u8]) -> Reply {
        self.stream.write_all(request).unwrap();
        read_reply(&mut self.stream)
    }

    /// A POST with the right headers plus `extra` ones, and `body`.
    fn post(&mut self, body: &str, extra: &str) -> Reply {
        let session = self.session.as_ref().map(|s| format!("Mcp-Session-Id: {s}\r\n")).unwrap_or_default();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{session}{extra}Content-Length: {}\r\n\r\n{body}",
            self.addr,
            token().expose(),
            body.len()
        );
        self.raw(request.as_bytes())
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let reply = self.post(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string(), "");
        assert_eq!(reply.status, 200, "{:?}", String::from_utf8_lossy(&reply.body));
        assert_eq!(reply.header("content-type"), Some("application/json"));
        reply.json()
    }

    fn initialize(&mut self) -> Value {
        let reply = self.post(&json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}).to_string(), "");
        assert_eq!(reply.status, 200);
        let session = reply.header("mcp-session-id").expect("initialize opens a session").to_owned();
        assert_eq!(session.len(), 64);
        self.session = Some(session);
        reply.json()
    }
}

#[test]
fn a_client_initializes_lists_tools_and_runs_commands_on_one_connection() {
    let (addr, stopper, serving) = start();
    let mut c = Client::new(addr);
    assert_eq!(c.initialize()["result"]["protocolVersion"], "2025-06-18");
    let note = c.post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "MCP-Protocol-Version: 2025-06-18\r\n");
    assert_eq!((note.status, note.body.len()), (202, 0), "notifications are accepted without a body");
    let tools = c.rpc("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["command_list", "command_run", "canvas_inspect", "ids_new"]);
    let node = "00000000-0000-4000-8000-000000000001";
    let added = c
        .rpc("tools/call", json!({"name": "command_run", "arguments": {"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": node, "kind": "text"}}}}));
    assert_eq!(added["result"]["isError"], false, "{added}");
    let page = c.rpc("tools/call", json!({"name": "canvas_inspect", "arguments": {"canvas": CANVAS}}));
    assert_eq!(page["result"]["structuredContent"]["nodes"][0]["id"], node, "the edit landed in the shared session");
    // People-only commands are refused exactly as over stdio.
    let keys =
        c.rpc("tools/call", json!({"name": "command_run", "arguments": {"id": "keys.set", "params": {"account": "openai", "key": "sk-test-123456789"}}}));
    assert_eq!(keys["result"]["isError"], true);
    assert!(keys["result"]["content"][0]["text"].as_str().unwrap().contains("can only be run by a person"));
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn paid_runs_get_the_same_answers_as_over_stdio() {
    let (addr, stopper, serving) = start();
    let mut c = Client::new(addr);
    c.initialize();
    let mut stdio = McpServer::new(blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap(), ids());
    for (id, params) in
        [("run.start", json!({})), ("run.start", json!({"canvas": CANVAS})), ("run.start", json!({"canvas": CANVAS, "quote": "forged", "confirm": true}))]
    {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "command_run", "arguments": {"id": id, "params": params}}});
        let over_http = c.post(&message.to_string(), "");
        assert_eq!(over_http.status, 200);
        let over_stdio: Value = serde_json::from_str(&stdio.handle(&message.to_string()).unwrap()).unwrap();
        assert_eq!(over_http.json(), over_stdio, "{id}");
    }
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn requests_without_the_token_or_from_web_pages_are_refused_before_anything_runs() {
    let (addr, stopper, serving) = start();
    let body = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#;
    let send = |headers: String| {
        let mut c = Client::new(addr);
        c.raw(format!("POST /mcp HTTP/1.1\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
    };
    let good = format!("Bearer {}", token().expose());
    let none = send(format!("Host: {addr}\r\n"));
    assert_eq!(none.status, 401);
    assert_eq!(none.header("www-authenticate"), Some("Bearer realm=\"nodecraft-mcp\""));
    assert_eq!(none.header("connection"), Some("close"));
    let wrong = format!("{}x", &good[..good.len() - 1]);
    assert_eq!(send(format!("Host: {addr}\r\nAuthorization: {wrong}\r\n")).status, 401);
    assert_eq!(send(format!("Host: {addr}\r\nAuthorization: Basic {}\r\n", token().expose())).status, 401);
    // DNS rebinding: a page whose name now resolves to 127.0.0.1 still sends its own Host/Origin.
    assert_eq!(send(format!("Host: evil.example:{}\r\nAuthorization: {good}\r\n", addr.port())).status, 403);
    assert_eq!(send(format!("Host: 127.0.0.1:1\r\nAuthorization: {good}\r\n")).status, 403, "another port");
    assert_eq!(send(format!("Authorization: {good}\r\n")).status, 403, "no Host");
    assert_eq!(send(format!("Host: {addr}\r\nHost: {addr}\r\nAuthorization: {good}\r\n")).status, 403);
    assert_eq!(send(format!("Host: {addr}\r\nOrigin: http://evil.example\r\nAuthorization: {good}\r\n")).status, 403);
    assert_eq!(send(format!("Host: {addr}\r\nOrigin: null\r\nAuthorization: {good}\r\n")).status, 403);
    let local_page = send(format!("Host: localhost:{}\r\nOrigin: http://localhost:3000\r\nAuthorization: {good}\r\n", addr.port()));
    assert_eq!(local_page.status, 200, "a page on this computer with the token may connect");
    assert!(local_page.header("access-control-allow-origin").is_none(), "no CORS, ever");
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn sessions_are_required_after_initialize_and_end_with_delete() {
    let (addr, stopper, serving) = start();
    let mut c = Client::new(addr);
    let ping = json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}).to_string();
    assert_eq!(c.post(&ping, "").status, 400, "no session yet");
    c.initialize();
    assert_eq!(c.post(&ping, "").status, 200);
    let mut other = Client::new(addr);
    other.session = Some("0".repeat(64));
    assert_eq!(other.post(&ping, "").status, 404, "an unknown session must initialize again");
    let session = c.session.clone().unwrap();
    let end = c.raw(
        format!("DELETE /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\nMcp-Session-Id: {session}\r\nContent-Length: 0\r\n\r\n", token().expose())
            .as_bytes(),
    );
    assert_eq!(end.status, 204);
    assert_eq!(c.post(&ping, "").status, 404, "ended");
    assert_eq!(c.post(&ping, "MCP-Protocol-Version: 1999-01-01\r\n").status, 400, "an unknown protocol version");
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn hostile_requests_get_errors_and_the_server_keeps_serving() {
    let (addr, stopper, serving) = start();
    let auth = format!("Host: {addr}\r\nAuthorization: Bearer {}\r\n", token().expose());
    let cases: Vec<(String, u16)> = vec![
        (format!("GET /mcp HTTP/1.1\r\n{auth}\r\n"), 405),
        (format!("OPTIONS /mcp HTTP/1.1\r\n{auth}Origin: http://localhost:1\r\n\r\n"), 405),
        (format!("POST /other HTTP/1.1\r\n{auth}Content-Length: 0\r\n\r\n"), 404),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: text/plain\r\nContent-Length: 2\r\n\r\n{{}}"), 415),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nAccept: text/html\r\nContent-Length: 2\r\n\r\n{{}}"), 406),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\n\r\n"), 411),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: 99999999999\r\n\r\n"), 413),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: -1\r\n\r\n"), 400),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{{}}"), 400),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"), 501),
        (format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: 5\r\n\r\n{{nope"), 400),
        (format!("POST /mcp HTTP/2.0\r\n{auth}\r\n"), 505),
        (format!("POST /mcp HTTP/1.1\r\n{auth} folded: yes\r\n\r\n"), 400),
        ("\u{0}\u{1}garbage\r\n\r\n".to_owned(), 400),
        (format!("POST /mcp HTTP/1.1\r\n{auth}X-Big: {}\r\n\r\n", "a".repeat(20 * 1024)), 431),
    ];
    for (request, want) in cases {
        let mut c = Client::new(addr);
        let reply = c.raw(request.as_bytes());
        assert_eq!(reply.status, want, "{:?}", request.chars().take(120).collect::<String>());
        reply.json();
    }
    let parse = {
        let mut c = Client::new(addr);
        c.post("{nope", "")
    };
    assert_eq!(parse.json()["error"]["code"], -32700);
    let mut c = Client::new(addr);
    let mut bad_utf8 = format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: 4\r\n\r\n").into_bytes();
    bad_utf8.extend_from_slice(b"\"\xff\xfe\"");
    let reply = c.raw(&bad_utf8);
    assert_eq!((reply.status, reply.json()["error"]["code"].as_i64()), (400, Some(-32700)));
    // A client that connects and says nothing, or stops mid-head, doesn't wedge the server.
    let _silent = TcpStream::connect(addr).unwrap();
    let mut slow = TcpStream::connect(addr).unwrap();
    slow.write_all(b"POST /mcp HTTP/1.1\r\nHo").unwrap();
    let mut c = Client::new(addr);
    c.initialize();
    assert_eq!(c.rpc("ping", json!({}))["result"], json!({}));
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn slow_heads_time_out_and_too_many_connections_are_turned_away() {
    let timeouts = Timeouts { head: Duration::from_millis(300), idle: Duration::from_millis(300), ..Timeouts::default() };
    let (addr, stopper, serving) = start_with(timeouts);
    let mut slow = TcpStream::connect(addr).unwrap();
    slow.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    slow.write_all(b"POST /mcp HTTP/1.1\r\n").unwrap();
    assert_eq!(read_reply(&mut slow).status, 408);
    let held: Vec<TcpStream> = (0..MAX_CONNECTIONS + 2).map(|_| TcpStream::connect(addr).unwrap()).collect();
    let mut refused = 0;
    for mut s in held.into_iter().rev().take(2) {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        if String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 503") {
            refused += 1;
        }
    }
    assert!(refused >= 1, "connections beyond the limit are refused");
    // Idle ones time out, freeing their slots.
    std::thread::sleep(Duration::from_millis(800));
    let mut c = Client::new(addr);
    c.initialize();
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn only_loopback_addresses_are_served_and_stopping_frees_the_port() {
    for addr in ["0.0.0.0:0", "[::]:0"] {
        let err = http::bind(Config::new(addr.parse().unwrap(), token())).err().unwrap();
        assert_eq!(err.kind, nodecraft_types::ErrorKind::Unsupported, "{addr}");
    }
    let (addr, stopper, serving) = start();
    assert_eq!(http::url(addr), format!("http://127.0.0.1:{}/mcp", addr.port()));
    stopper.stop();
    serving.join().unwrap();
    // Rebinding the same port succeeds once the server has stopped.
    let again = http::bind(Config::new(addr, token())).unwrap();
    assert_eq!(again.addr(), addr);
}

#[test]
fn the_in_app_server_answers_on_the_apps_thread_with_the_bridge_tools() {
    let (client, calls) = control::local(std::sync::Arc::new(|| {}));
    // The "UI thread": owns the session and answers calls, as the app does each frame.
    let ui = std::thread::spawn(move || {
        let mut session = blank_session(ProjectId::from_bytes([1; 16]), CANVAS).unwrap();
        for call in calls.iter() {
            let answer = match control::answer(&mut session, &call.request) {
                Some(answer) => answer,
                None => control::screenshot_answer(1, 1, &[0, 0, 0, 255]),
            };
            call.reply(answer);
        }
        session.document().canvases().get(&CANVAS).unwrap().nodes().len()
    });
    let listening = http::bind(Config::new("127.0.0.1:0".parse().unwrap(), token())).unwrap();
    let (addr, stopper) = (listening.addr(), listening.stopper());
    let serving = std::thread::spawn(move || {
        let mut server = McpServer::in_app(client, ids());
        listening.serve(&mut |m| server.handle_message(m));
    });
    let mut c = Client::new(addr);
    c.initialize();
    let tools = c.rpc("tools/list", json!({}));
    assert!(tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "screenshot"));
    let node = "00000000-0000-4000-8000-000000000002";
    let added = c
        .rpc("tools/call", json!({"name": "command_run", "arguments": {"id": "node.add", "params": {"canvas": CANVAS, "node": {"id": node, "kind": "text"}}}}));
    assert_eq!(added["result"]["isError"], false, "{added}");
    let shot = c.rpc("tools/call", json!({"name": "screenshot", "arguments": {}}));
    assert_eq!(shot["result"]["content"][0]["mimeType"], "image/png");
    let human = c.rpc("tools/call", json!({"name": "command_run", "arguments": {"id": "keys.remove", "params": {"account": "openai"}}}));
    assert_eq!(human["result"]["isError"], true);
    stopper.stop();
    serving.join().unwrap();
    assert_eq!(ui.join().unwrap(), 1, "the app's own session holds the edit");
}

/// A server whose handler records each message id it runs and takes `slow` long over the id
/// "slow"; every request gets an empty result.
fn start_recording(timeouts: Timeouts, slow: Duration) -> (SocketAddr, Stopper, std::thread::JoinHandle<()>, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    let mut config = Config::new("127.0.0.1:0".parse().unwrap(), token());
    config.timeouts = timeouts;
    let listening = http::bind(config).unwrap();
    let (addr, stopper) = (listening.addr(), listening.stopper());
    let handled = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&handled);
    let serving = std::thread::spawn(move || {
        listening.serve(&mut |m| {
            let id = m.get("id").cloned()?;
            seen.lock().unwrap().push(id.clone());
            if id == "slow" {
                std::thread::sleep(slow);
            }
            Some(json!({"jsonrpc": "2.0", "id": id, "result": {}}))
        });
    });
    (addr, stopper, serving, handled)
}

fn ping(id: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "ping"}).to_string()
}

#[test]
fn queued_messages_whose_client_left_or_that_waited_too_long_never_run() {
    let timeouts = Timeouts { queue: Duration::from_millis(400), ..Timeouts::default() };
    let (addr, stopper, serving, handled) = start_recording(timeouts, Duration::from_millis(1500));
    let mut setup = Client::new(addr);
    setup.initialize();
    let session = setup.session.clone();
    // Occupies the serving thread.
    let busy = std::thread::spawn(move || {
        let mut c = Client::new(addr);
        c.session = session;
        c.post(&ping("slow"), "").status
    });
    std::thread::sleep(Duration::from_millis(100));
    // Queued behind it, then its client gives up and disconnects.
    {
        let mut gone = Client::new(addr);
        gone.session = setup.session.clone();
        let body = ping("gone");
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nMcp-Session-Id: {}\r\nContent-Length: {}\r\n\r\n{body}",
            token().expose(),
            gone.session.as_deref().unwrap(),
            body.len()
        );
        gone.stream.write_all(request.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }
    // Queued behind it too, and still waiting when the queue timeout passes: refused, not run.
    let late = setup.post(&ping("late"), "");
    assert_eq!(late.status, 503);
    assert!(late.json()["error"]["message"].as_str().unwrap().contains("nothing ran"));
    assert_eq!(busy.join().unwrap(), 200);
    // The thread is free again: a new message runs.
    assert_eq!(setup.post(&ping("after"), "").status, 200);
    let ran: Vec<Value> = handled.lock().unwrap().clone();
    assert_eq!(ran, [json!(0), json!("slow"), json!("after")], "neither the abandoned nor the late message ran");
    stopper.stop();
    serving.join().unwrap();
}

#[test]
fn bodies_expectations_and_hosts_are_strict() {
    let timeouts = Timeouts { head: Duration::from_millis(300), ..Timeouts::default() };
    let (addr, stopper, serving) = start_with(timeouts);
    let auth = format!("Host: {addr}\r\nAuthorization: Bearer {}\r\n", token().expose());
    // DELETE carries no body: one is refused rather than taken for the next request.
    let mut c = Client::new(addr);
    let reply = c.raw(format!("DELETE /mcp HTTP/1.1\r\n{auth}Mcp-Session-Id: x\r\nContent-Length: 5\r\n\r\nhello").as_bytes());
    assert_eq!((reply.status, reply.header("connection")), (400, Some("close")));
    // Expect: 100-continue gets the go-ahead before the body; anything else is refused.
    let body = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}).to_string();
    let mut c = Client::new(addr);
    c.stream
        .write_all(
            format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
        )
        .unwrap();
    let mut interim = [0u8; 25];
    c.stream.read_exact(&mut interim).unwrap();
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    c.stream.write_all(body.as_bytes()).unwrap();
    assert_eq!(read_reply(&mut c.stream).status, 200);
    let mut c = Client::new(addr);
    assert_eq!(
        c.raw(format!("POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nExpect: magic\r\nContent-Length: 2\r\n\r\n").as_bytes()).status,
        417
    );
    // An IPv6 host must be bracketed.
    let mut c = Client::new(addr);
    let bare =
        c.raw(format!("POST /mcp HTTP/1.1\r\nHost: ::1:{}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\n\r\n", addr.port(), token().expose()).as_bytes());
    assert_eq!(bare.status, 403);
    // A new connection that sends nothing is closed after the head timeout, not the idle one.
    let mut silent = TcpStream::connect(addr).unwrap();
    silent.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let began = std::time::Instant::now();
    let mut buf = Vec::new();
    let _ = silent.read_to_end(&mut buf);
    assert!(began.elapsed() < Duration::from_secs(3), "closed after {:?}", began.elapsed());
    stopper.stop();
    serving.join().unwrap();
}
