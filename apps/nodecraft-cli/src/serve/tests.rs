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
    let dir = tempfile::tempdir().unwrap();
    // The token file is only written to a folder no one else can read.
    let file = nodecraft_automation::control::private_dir(dir.path()).unwrap().join("serve.json");
    let config = Config { listen: SocketAddr::from(([127, 0, 0, 1], 0)), token_file: Some(file.clone()), policy };
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
}

fn parse(raw: &[u8]) -> Result<http::Request, Refusal> {
    let limits = Limits { head_bytes: 256, headers: 4, body_bytes: 16, deadline: Duration::from_secs(5) };
    read(raw, limits, || {})
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
