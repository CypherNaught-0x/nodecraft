use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, Snapshot};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, NodeId, ProjectId, TechniqueId};
use serde_json::{Value, json};

use super::http::{Limits, Refusal, read};
use super::worker::{amount, within};
use super::*;

const DRAFT: CanvasId = CanvasId::from_bytes([1; 16]);
const BRIEF: NodeId = NodeId::from_bytes([2; 16]);
const SWAP: NodeId = NodeId::from_bytes([3; 16]);
const VERSION: CanvasId = CanvasId::from_bytes([4; 16]);
const TECHNIQUE: TechniqueId = TechniqueId::from_bytes([5; 16]);

/// A project publishing "Swap": a Brief text input, through Replace text, out as "Swapped".
fn snapshot() -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Served").unwrap();
    Change::AddCanvas { canvas: Canvas::new(DRAFT, "Draft") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.dispatch("node.create", &json!({"canvas": DRAFT, "kind": "source.text", "id": BRIEF, "pos": {"x": 0.0, "y": 0.0}})).unwrap();
    session.dispatch("node.create", &json!({"canvas": DRAFT, "kind": "text.replace", "id": SWAP, "pos": {"x": 300.0, "y": 0.0}})).unwrap();
    session.dispatch("edge.connect", &json!({"canvas": DRAFT, "edge": {"id": nodecraft_types::EdgeId::from_bytes([7; 16]), "from": {"node": BRIEF, "port": "out"}, "to": {"node": SWAP, "port": "text"}, "order": 0}})).unwrap();
    session
        .dispatch(
            "technique.publish",
            &json!({"canvas": DRAFT, "published": VERSION, "seed": NodeId::from_bytes([6; 16]), "technique": TECHNIQUE, "name": "Swap",
                "inputs": [{"node": BRIEF, "name": "Brief"}], "outputs": [{"node": SWAP, "name": "Swapped"}],
                "params": [{"node": SWAP, "param": "find"}, {"node": SWAP, "param": "replacement"}]}),
        )
        .unwrap();
    Snapshot { project: session.document().clone(), assets: AssetStore::default(), epoch: 0 }
}

/// No provider is needed: Replace text runs on the machine.
fn loaded() -> Result<Loaded, nodecraft_types::Error> {
    let maker = || -> Result<Box<crate::quote::Maker<'static>>, nodecraft_types::Error> {
        Ok(Box::new(|_: &str| Err(nodecraft_engine::provider_setup::Readiness::NotSetUp("no providers in tests".into()))))
    };
    Ok(Loaded { snapshot: snapshot(), maker: Box::new(maker) })
}

struct Served {
    _dir: tempfile::TempDir,
    addr: SocketAddr,
    token: String,
}

fn serve(policy: Policy) -> Served {
    serve_with(policy, None)
}

fn serve_with(policy: Policy, webhooks: Option<Webhooks>) -> Served {
    let dir = tempfile::tempdir().unwrap();
    // The token file is only written to a folder no one else can read.
    let file = nodecraft_automation::control::private_dir(dir.path()).unwrap().join("serve.json");
    let config = Config { listen: SocketAddr::from(([127, 0, 0, 1], 0)), token_file: Some(file.clone()), policy, webhooks };
    let server = start(config, loaded).unwrap();
    let written: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(written["port"], server.addr.port());
    assert_eq!(written["token"], server.token.to_hex());
    Served { addr: server.addr, token: server.token.to_hex(), _dir: dir }
}

/// One raw exchange: the status, the headers' text and the body.
fn exchange(addr: SocketAddr, raw: &[u8]) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    stream.write_all(raw).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(reply[..split].to_vec()).unwrap();
    let status = head[9..12].parse().unwrap();
    (status, head, reply[split + 4..].to_vec())
}

impl Served {
    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, Vec<u8>) {
        let mut raw = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n", self.token, body.len());
        for (name, value) in headers {
            raw.push_str(&format!("{name}: {value}\r\n"));
        }
        raw.push_str("\r\n");
        raw.push_str(body);
        exchange(self.addr, raw.as_bytes())
    }

    fn json(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, Value) {
        let (status, _, body) = self.request(method, path, headers, body);
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn submit(&self, body: &Value, key: Option<&str>) -> (u16, Value) {
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(key) = key {
            headers.push(("Idempotency-Key", key));
        }
        self.json("POST", &format!("/techniques/{TECHNIQUE}/runs"), &headers, &body.to_string())
    }

    /// Polls the run until it finishes.
    fn finished(&self, id: &str) -> Value {
        for _ in 0..400 {
            let (status, run) = self.json("GET", &format!("/runs/{id}"), &[], "");
            assert_eq!(status, 200, "{run}");
            if run["status"] == "succeeded" || run["status"] == "failed" {
                return run;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the run didn't finish");
    }
}

#[test]
fn a_technique_is_listed_run_and_its_output_fetched() {
    let served = serve(Policy::default());
    let (status, listed) = served.json("GET", "/techniques", &[], "");
    assert_eq!(status, 200, "{listed}");
    let swap = &listed["techniques"][0];
    assert_eq!(swap["name"], "Swap", "{listed}");
    assert_eq!(swap["inputs"][0]["name"], "Brief", "{listed}");
    let port = swap["inputs"][0]["port"].as_str().unwrap().to_owned();
    let params: Vec<&str> = swap["params"].as_array().unwrap().iter().map(|p| p["spec"]["name"].as_str().unwrap()).collect();
    assert_eq!(params.len(), 2, "{listed}");

    let body = json!({"inputs": {port.clone(): "a red fox"}, "params": {params[0]: "red", params[1]: "grey"}});
    let (status, queued) = served.submit(&body, Some("first"));
    assert_eq!(status, 202, "{queued}");
    let id = queued["id"].as_str().unwrap().to_owned();
    let run = served.finished(&id);
    assert_eq!(run["status"], "succeeded", "{run}");
    assert_eq!(run["outputs"][0]["name"], "Swapped", "{run}");
    assert_eq!(run["outputs"][0]["text"], "a grey fox", "{run}");
    let (status, _, text) = served.request("GET", &format!("/runs/{id}/outputs/0"), &[], "");
    assert_eq!((status, text.as_slice()), (200, b"a grey fox".as_slice()));

    // The same key and body give the same run; the same key with another body is refused.
    let (status, again) = served.submit(&body, Some("first"));
    assert_eq!((status, again["id"].as_str()), (200, Some(id.as_str())), "{again}");
    let (status, reused) = served.submit(&json!({"inputs": {port: "other"}}), Some("first"));
    assert_eq!(status, 422, "{reused}");
    assert_eq!(reused["error"]["kind"], "idempotency_key_reuse");
}

#[test]
fn requests_are_checked_before_anything_runs() {
    let served = serve(Policy::default());
    let (status, _, _) = exchange(served.addr, b"GET /techniques HTTP/1.1\r\n\r\n");
    assert_eq!(status, 401, "no token");
    let (status, _, _) = exchange(served.addr, b"GET /techniques HTTP/1.1\r\nAuthorization: Bearer 00\r\n\r\n");
    assert_eq!(status, 401, "a wrong token");
    assert_eq!(served.json("GET", "/nowhere", &[], "").0, 404);
    assert_eq!(served.json("DELETE", "/techniques", &[], "").0, 405);
    assert_eq!(served.json("GET", "/runs/nope", &[], "").0, 404);
    let json_type = [("Content-Type", "application/json")];
    let path = format!("/techniques/{TECHNIQUE}/runs");
    assert_eq!(served.json("POST", &path, &[], "{}").0, 415, "no content type");
    assert_eq!(served.json("POST", &path, &json_type, "[1]").0, 400, "not an object");
    assert_eq!(served.json("POST", &path, &json_type, "{\"nope\":1}").0, 422, "an unknown field");
    assert_eq!(served.json("POST", &path, &json_type, "{\"inputs\":{\"nope\":\"x\"}}").0, 422, "an unknown input");
    assert_eq!(served.json("POST", &path, &json_type, "{\"params\":{\"nope\":\"x\"}}").0, 422, "an unknown parameter");
    assert_eq!(served.json("POST", &path, &json_type, "{\"version\":99}").0, 404, "an unknown version");
    assert_eq!(served.json("POST", "/techniques/nope/runs", &json_type, "{}").0, 404, "an unknown technique");
    assert_eq!(served.json("POST", &path, &[("Content-Type", "application/json"), ("Idempotency-Key", "a b")], "{}").0, 400, "a bad key");
}

#[test]
fn a_run_over_the_limit_or_unpriced_is_refused() {
    let priced = json!({"totals": {"USD": {"micros": 2_000_000}}, "complete": true, "steps": []});
    let mut policy = Policy::default();
    let refused = within(&priced, &policy).unwrap_err();
    assert!(refused.message.contains("--max-cost USD="), "{}", refused.message);
    policy.max_cost.insert("USD".into(), 1_500_000);
    let over = within(&priced, &policy).unwrap_err();
    assert!(over.message.contains("2 USD") && over.message.contains("1.5 USD"), "{}", over.message);
    policy.max_cost.insert("USD".into(), 2_000_000);
    within(&priced, &policy).unwrap();
    let unpriced = json!({"totals": {}, "complete": false, "steps": []});
    assert!(within(&unpriced, &policy).unwrap_err().message.contains("--allow-unpriced"));
    policy.allow_unpriced = true;
    within(&unpriced, &policy).unwrap();
    assert_eq!(amount(1_250_000), "1.25");
    assert_eq!(amount(3_000_000), "3");
}

#[test]
fn amounts_are_parsed_exactly() {
    assert_eq!(micros("1.50"), Some(1_500_000));
    assert_eq!(micros("0.000001"), Some(1));
    assert_eq!(micros("12"), Some(12_000_000));
    for bad in ["", ".5", "1.", "1.0000001", "-1", "1e3", "999999999999999", "1.5x"] {
        assert_eq!(micros(bad), None, "{bad}");
    }
    assert_eq!(parse_max_cost("USD=2"), Ok(("USD".into(), 2_000_000)));
    for bad in ["USD", "usd=1", "USDT=1", "=1", "USD=-1"] {
        assert!(parse_max_cost(bad).is_err(), "{bad}");
    }
}

#[test]
fn base64_decodes_and_refuses_what_isnt() {
    assert_eq!(api::base64("aGVsbG8="), Some(b"hello".to_vec()));
    assert_eq!(api::base64("aGVsbG8"), Some(b"hello".to_vec()));
    assert_eq!(api::base64(""), Some(Vec::new()));
    assert_eq!(api::base64("aGVs bG8="), None);
    assert_eq!(api::base64("a"), None);
    assert_eq!(api::base64("aGVsbG9="), None, "stray bits");
    assert_eq!(api::base64("aGVsbG8=="), None, "too much padding");
    assert_eq!(api::base64("aGVsbA="), None, "padding that doesn't fill a group");
    assert_eq!(api::base64("aGVsbA=="), Some(b"hell".to_vec()));
}

#[test]
fn the_body_isnt_read_until_the_token_is_checked() {
    let mut continued = false;
    let refused = read(
        b"POST / HTTP/1.1\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\n".as_slice(),
        Limits::DEFAULT,
        |_| Err(Refusal::UNAUTHORIZED),
        || continued = true,
    )
    .unwrap_err();
    assert_eq!(refused.status, 401);
    assert!(!continued, "no body was invited");
    // Over the wire: a large body is never waited for.
    let served = serve(Policy::default());
    let mut stream = TcpStream::connect(served.addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(b"POST /techniques HTTP/1.1\r\nContent-Length: 60000000\r\n\r\n").unwrap();
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply);
    assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
    assert!(reply.contains("WWW-Authenticate: Bearer"), "{reply}");
}

#[test]
fn one_key_sent_twice_at_once_makes_one_run() {
    let served = std::sync::Arc::new(serve(Policy::default()));
    let (_, listed) = served.json("GET", "/techniques", &[], "");
    let port = listed["techniques"][0]["inputs"][0]["port"].as_str().unwrap().to_owned();
    let body = json!({"inputs": {port: "a red fox"}});
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let (served, body) = (std::sync::Arc::clone(&served), body.clone());
            std::thread::spawn(move || served.submit(&body, Some("once")))
        })
        .collect();
    let answers: Vec<(u16, Value)> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let ids: std::collections::BTreeSet<&str> = answers.iter().map(|(_, run)| run["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 1, "{answers:?}");
    assert_eq!(answers.iter().filter(|(status, _)| *status == 202).count(), 1, "{answers:?}");
}

#[test]
fn an_expired_file_keeps_its_place_and_answers_gone() {
    use super::worker::{Body, Produced, Run, Runs};
    let runs = std::sync::Arc::new(Runs::default());
    let mut run = Run::new("t".into(), 1, None);
    run.status = worker::Status::Succeeded;
    run.expired = true;
    run.outputs = vec![
        Produced { name: "Picture".into(), port: "a".into(), item: None, body: Body::Expired { mime: "image/png".into() } },
        Produced { name: "Caption".into(), port: "b".into(), item: None, body: Body::Text("fox".into()) },
    ];
    runs.lock().add("r1".into(), run).unwrap();
    let token = nodecraft_automation::control::Token::generate().unwrap();
    let auth = format!("Bearer {}", token.to_hex());
    let (jobs, _queue) = std::sync::mpsc::sync_channel(1);
    let state = api::State::new(token, Vec::new(), runs, jobs, Policy::default(), None);
    let get = |path: &str| {
        let request = http::Request { method: "GET".into(), path: path.into(), headers: vec![("authorization".into(), auth.clone())], body: Vec::new() };
        state.answer(&request)
    };
    assert_eq!(get("/runs/r1/outputs/0").status, 410);
    let caption = get("/runs/r1/outputs/1");
    assert!(matches!(&caption.body, http::Body::Bytes(b) if b == b"fox"), "{caption:?}");
    let described = get("/runs/r1");
    let http::Body::Bytes(bytes) = described.body else { panic!("a JSON body") };
    let described: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(described["outputs"][0]["expired"], true, "{described}");
    assert_eq!(described["outputs"][1]["n"], 1, "{described}");
}

fn parse(raw: &[u8]) -> Result<http::Request, Refusal> {
    let limits = Limits { head_bytes: 256, headers: 4, body_bytes: 16, head_deadline: Duration::from_secs(5), deadline: Duration::from_secs(5) };
    read(raw, limits, |_| Ok(()), || {})
}

#[test]
fn the_parser_bounds_what_a_client_sends() {
    let request = parse(b"POST /a/b?x=1 HTTP/1.1\r\nContent-Length: 3\r\nX-One:  v \r\n\r\nabc").unwrap();
    assert_eq!((request.method.as_str(), request.path.as_str(), request.body.as_slice()), ("POST", "/a/b", b"abc".as_slice()));
    assert_eq!(request.header("x-one"), Some("v"));
    let status = |raw: &[u8]| parse(raw).unwrap_err().status;
    assert_eq!(status(b"GET / HTTP/2\r\n\r\n"), 505);
    assert_eq!(status(b"get / HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"GET x HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\n"), 400, "ends before the blank line");
    assert_eq!(status(format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(300)).as_bytes()), 431);
    assert_eq!(status(b"GET / HTTP/1.1\r\na: 1\r\nb: 1\r\nc: 1\r\nd: 1\r\ne: 1\r\n\r\n"), 431);
    assert_eq!(status(b"POST / HTTP/1.1\r\nContent-Length: 17\r\n\r\n"), 413);
    assert_eq!(status(b"POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n"), 413);
    assert_eq!(status(b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\na"), 400);
    assert_eq!(status(b"POST / HTTP/1.1\r\nContent-Length: +1\r\n\r\na"), 400);
    assert_eq!(status(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"), 501);
    assert_eq!(status(b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\nno colon\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\nX-\xff: 1\r\n\r\n"), 400);
}

/// The test secret, as Standard Webhooks writes one: its key is the 32 bytes after `whsec_`.
const SECRET: &str = "whsec_MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

/// One delivery as a receiver saw it.
struct Received {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> &str {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap()
    }
}

/// A receiver on loopback that answers each delivery with the next of `answers` (then 200),
/// and hands what it got to the returned channel.
fn receiver(answers: Vec<u16>) -> (SocketAddr, std::sync::mpsc::Receiver<Received>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut answers = answers.into_iter();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read(&stream, Limits::DEFAULT, |_| Ok(()), || {}).unwrap();
            // Recorded before answering, so the server can't see the answer before the test does.
            if sent.send(Received { path: request.path, headers: request.headers, body: request.body }).is_err() {
                return;
            }
            let status = answers.next().unwrap_or(200);
            let _ = stream.write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        }
    });
    (addr, got)
}

fn webhooks(receiver: SocketAddr, schedule: &[u64]) -> Webhooks {
    let store = nodecraft_engine::secrets::MemoryStore::default();
    let account = nodecraft_engine::secrets::Account::new(WEBHOOK_ACCOUNT).unwrap();
    nodecraft_engine::secrets::SecretStore::set(&store, &account, &nodecraft_engine::secrets::Secret::new(SECRET).unwrap()).unwrap();
    let allow = parse_webhook_allow(&format!("http://{receiver}/hooks")).unwrap();
    let mut settings = Webhooks::new(vec![allow], true, std::sync::Arc::new(store));
    settings.schedule = schedule.iter().map(|&ms| Duration::from_millis(ms)).collect();
    settings
}

/// Polls the run until its webhook is no longer pending.
fn hooked(served: &Served, id: &str) -> Value {
    for _ in 0..400 {
        let (_, run) = served.json("GET", &format!("/runs/{id}"), &[], "");
        if run["webhook"]["status"] != "pending" {
            return run;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the webhook stayed pending");
}

fn swap_body(served: &Served) -> Value {
    let (_, listed) = served.json("GET", "/techniques", &[], "");
    let port = listed["techniques"][0]["inputs"][0]["port"].as_str().unwrap().to_owned();
    json!({"inputs": {port: "a red fox"}})
}

#[test]
fn a_finished_run_is_delivered_signed_and_retried_under_one_event_id() {
    // The first attempt is answered 500; the retry gets through.
    let (addr, got) = receiver(vec![500]);
    let served = serve_with(Policy::default(), Some(webhooks(addr, &[50, 50])));
    let mut body = swap_body(&served);
    body["webhook"] = json!(format!("http://{addr}/hooks/runs?tag=1"));
    let (status, queued) = served.submit(&body, None);
    assert_eq!(status, 202, "{queued}");
    assert_eq!(queued["webhook"]["status"], "pending", "{queued}");
    let id = queued["id"].as_str().unwrap().to_owned();

    let first = got.recv_timeout(Duration::from_secs(30)).unwrap();
    let second = got.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(first.path, "/hooks/runs");
    // A duplicate: the retry carries the same event id, so a receiver can drop it.
    assert_eq!(first.header("webhook-id"), second.header("webhook-id"));
    assert_eq!(first.body, second.body);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    for delivery in [&first, &second] {
        let (event, timestamp, signature) = (delivery.header("webhook-id"), delivery.header("webhook-timestamp"), delivery.header("webhook-signature"));
        assert_eq!(delivery.header("content-type"), "application/json");
        assert!(delivery.header("user-agent").starts_with("NodeCraft/"));
        webhook::verify(KEY, event, timestamp, signature, &delivery.body, now, 300).unwrap();
        // An invalid signature: another body, another key, or another event id.
        assert!(webhook::verify(KEY, event, timestamp, signature, b"{}", now, 300).is_err());
        assert!(webhook::verify(b"another key, just as long as it is", event, timestamp, signature, &delivery.body, now, 300).is_err());
        assert!(webhook::verify(KEY, "evt_other", timestamp, signature, &delivery.body, now, 300).is_err());
        // A late delivery, replayed after the receiver's tolerance, is refused.
        assert!(webhook::verify(KEY, event, timestamp, signature, &delivery.body, now + 600, 300).is_err());
    }
    let payload: Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!((payload["type"].as_str(), payload["version"].as_u64()), (Some("run.completed"), Some(1)), "{payload}");
    assert_eq!(payload["event"], first.header("webhook-id"), "{payload}");
    assert_eq!(payload["run"]["id"], id.as_str(), "{payload}");
    assert_eq!(payload["run"]["status"], "succeeded", "{payload}");
    assert_eq!(payload["run"]["outputs"][0]["text"], "a red fox", "{payload}");
    assert!(payload["run"].get("webhook").is_none(), "{payload}");

    let run = hooked(&served, &id);
    assert_eq!(run["webhook"], json!({"status": "delivered", "attempts": 2}), "{run}");
}

#[test]
fn a_failed_callback_never_fails_the_run() {
    let (addr, got) = receiver(vec![500, 503]);
    let served = serve_with(Policy::default(), Some(webhooks(addr, &[20])));
    let mut body = swap_body(&served);
    body["webhook"] = json!(format!("http://{addr}/hooks"));
    let (status, queued) = served.submit(&body, None);
    assert_eq!(status, 202, "{queued}");
    let id = queued["id"].as_str().unwrap().to_owned();
    let run = hooked(&served, &id);
    assert_eq!(run["status"], "succeeded", "{run}");
    assert_eq!(run["webhook"]["status"], "failed", "{run}");
    assert_eq!(run["webhook"]["attempts"], 2, "{run}");
    assert_eq!(run["webhook"]["error"], "the receiver answered 503", "{run}");
    assert_eq!(got.try_iter().count(), 2, "the attempts are bounded");
    // The run's results are still there to poll for.
    let (status, _, text) = served.request("GET", &format!("/runs/{id}/outputs/0"), &[], "");
    assert_eq!((status, text.as_slice()), (200, b"a red fox".as_slice()));
}

#[test]
fn webhooks_go_only_where_the_server_allows() {
    // A server without webhooks refuses a request that names one.
    let plain = serve(Policy::default());
    let mut body = swap_body(&plain);
    body["webhook"] = json!("https://hooks.example.com/x");
    assert_eq!(plain.submit(&body, None).0, 422);

    // Loopback is refused at delivery unless the server allows private addresses.
    let (addr, got) = receiver(Vec::new());
    let mut settings = webhooks(addr, &[]);
    settings.allow_private = false;
    let served = serve_with(Policy::default(), Some(settings));
    let mut body = swap_body(&served);
    body["webhook"] = json!(format!("http://{addr}/hooks"));
    let (_, queued) = served.submit(&body, None);
    let run = hooked(&served, queued["id"].as_str().unwrap());
    assert_eq!((run["status"].as_str(), run["webhook"]["status"].as_str()), (Some("succeeded"), Some("failed")), "{run}");
    assert!(run["webhook"]["error"].as_str().unwrap().contains("--webhook-allow-private"), "{run}");
    assert!(got.try_recv().is_err(), "nothing was sent");

    for refused in [
        format!("http://{addr}/elsewhere"),
        format!("http://{addr}/hooksmith"),
        format!("https://{addr}/hooks"),
        format!("http://user:pw@{addr}/hooks"),
        format!("http://{addr}/hooks#frag"),
        "http://127.0.0.2/hooks".into(),
        "file:///etc/passwd".into(),
        "not a url".into(),
        format!("http://{addr}/hooks/{}", "a".repeat(3000)),
    ] {
        body["webhook"] = json!(refused);
        let (status, answer) = served.submit(&body, None);
        assert_eq!(status, 422, "{refused}: {answer}");
    }
    body["webhook"] = json!(7);
    assert_eq!(served.submit(&body, None).0, 422);

    for bad in ["ftp://x.example", "https://", "https://a@x.example", "https://x.example/?q=1", "nope"] {
        assert!(parse_webhook_allow(bad).is_err(), "{bad}");
    }
    let public = |ip: &str| webhook::public(ip.parse().unwrap());
    assert!(public("93.184.216.34") && public("2606:4700::1"));
    assert!(public("64:ff9b::5db8:d822") && public("2002:5db8:d822::1"));
    for private in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:10.0.0.1",
        "::10.0.0.1",
        "64:ff9b::7f00:1",
        "64:ff9b:1::a00:1",
        "2002:a00:1::1",
        "fec0::1",
        "192.0.0.8",
        "198.18.0.1",
    ] {
        assert!(!public(private), "{private}");
    }
}

#[test]
fn signatures_match_hmac_sha256_and_secrets_decode() {
    // Computed independently (Python's hmac module) for this key, id, timestamp and body.
    assert_eq!(webhook::sign(KEY, "evt_1", "1700000000", b"{\"a\":1}").unwrap(), "E91RjL3XwKNvhbIjLEB4Oo053Uu727CszikrY+6s9HE=");
    assert_eq!(webhook::key(SECRET).unwrap(), KEY);
    let plain = "a plain secret, used as it is, 32+";
    assert_eq!(webhook::key(plain).unwrap(), plain.as_bytes());
    assert!(webhook::key("too short").is_err());
    assert!(webhook::key("whsec_not*base64").is_err());
    assert!(webhook::key("whsec_").is_err());
    // 16 bytes of key: valid base64, but too little key.
    assert!(webhook::key("whsec_MDEyMzQ1Njc4OWFiY2RlZg==").is_err());
    for bytes in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", KEY] {
        assert_eq!(api::base64(&webhook::encode(bytes)).unwrap(), bytes);
    }
    assert_eq!(webhook::encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(webhook::encode(b"fooba"), "Zm9vYmE=");
}

#[test]
fn a_big_payload_leaves_out_outputs_then_all_but_the_summary() {
    let read = |body: Vec<u8>| -> serde_json::Value {
        assert!(body.len() <= webhook::MAX_PAYLOAD, "{}", body.len());
        serde_json::from_slice(&body).unwrap()
    };
    let small = json!({"id": "1", "status": "succeeded", "outputs": [{"n": 0, "text": "hi"}]});
    assert_eq!(read(webhook::payload("evt_1", small.clone()))["run"], small);

    let text = "x".repeat(webhook::MAX_PAYLOAD);
    let big = json!({"id": "1", "status": "succeeded", "cost": {"totals": {}}, "outputs": [{"n": 0, "name": "t", "text": text}]});
    let run = read(webhook::payload("evt_1", big))["run"].clone();
    assert_eq!(run["truncated"], true);
    assert_eq!(run["outputs"], json!([{"n": 0, "name": "t"}]));

    let steps: Vec<_> = (0..20_000).map(|i| json!({"step": i, "note": "a long enough note"})).collect();
    let huge = json!({"id": "1", "status": "failed", "error": "no", "steps": steps, "outputs": []});
    let run = read(webhook::payload("evt_1", huge))["run"].clone();
    assert_eq!(run, json!({"id": "1", "status": "failed", "error": "no", "truncated": true}));
}
