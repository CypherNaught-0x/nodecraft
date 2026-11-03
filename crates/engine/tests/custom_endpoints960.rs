//! Custom endpoints (#960): a catalog provider served from a base URL the person set, against a
//! local fixture server speaking the OpenAI API. Never contacts a real provider.
#![cfg(not(target_arch = "wasm32"))]

use nodecraft_engine::{
    Session,
    commands::{Registry, Resources},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
    provider_setup::{HostEnv, Readiness, make},
    secrets::{Account, MemoryStore, Secret, SecretStore},
    settings::ProviderSettings,
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, JobInput, Output, Provider, ProviderError, ProviderJob, ReqwestHttp, Submission};
use nodecraft_types::{CanvasId, NodeId, ProjectId, Value};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const KEY: &str = "sk-public-fixture-960";
const REPLY: &str = r#"{"choices":[{"message":{"content":"Hello from the proxy"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4}}"#;

/// What the fixture server saw: the request line, its headers (lowercased names) and its body.
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Serves one OpenAI-shaped chat reply on 127.0.0.1 and reports the request it got.
fn fixture_server() -> (String, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut headers = Vec::new();
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                break;
            }
            if let Some((name, value)) = header.trim_end().split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                if name == "content-length" {
                    length = value.trim().parse().unwrap();
                }
                headers.push((name, value.trim().to_owned()));
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nx-litellm-response-cost: 0.5\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REPLY}",
            REPLY.len()
        );
        let _ = tx.send(Seen { line: line.trim_end().to_owned(), headers, body });
    });
    (format!("http://{address}"), rx)
}

fn keys() -> MemoryStore {
    let keys = MemoryStore::default();
    keys.set(&Account::new("openai").unwrap(), &Secret::new(KEY).unwrap()).unwrap();
    keys
}

fn env<'a>(keys: &'a MemoryStore, http: Arc<dyn Http>, settings: &'a ProviderSettings) -> HostEnv<'a> {
    HostEnv { keys, http, settings, now_ms: Arc::new(|| 0), progress: None }
}

fn job() -> ProviderJob {
    ProviderJob {
        model: "gpt-4.1-mini-2025-04-14".into(),
        params: [("max_tokens".to_owned(), json!(64))].into_iter().collect(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A public-safe harbour".into() }],
        idempotency_key: "nodecraft:fixture:960".into(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

#[test]
fn the_request_and_the_key_go_to_the_configured_base_url() {
    let (base, seen) = fixture_server();
    let mut settings = ProviderSettings::default();
    settings.base_urls.insert("openai".into(), format!("{base}/v1/").into());
    let keys = keys();
    let provider = make("openai", &env(&keys, Arc::new(ReqwestHttp::new().unwrap()), &settings)).unwrap();
    assert_eq!(provider.id(), "openai", "the catalog provider keeps its identity");
    assert_eq!(provider.custom_base_url(), Some(format!("{base}/v1").as_str()));
    let done = runtime().block_on(provider.submit(&job())).unwrap();
    let Submission::Finished(done) = done else { panic!("chat finishes at submit") };
    assert_eq!(done.outputs, vec![Output::Text("Hello from the proxy".into())]);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(3), Some(4)));
    assert_eq!(done.usage.cost, None, "a custom server's spend header doesn't replace the catalog price");
    let seen = seen.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(seen.line, "POST /v1/chat/completions HTTP/1.1");
    assert!(seen.headers.contains(&("authorization".into(), format!("Bearer {KEY}"))), "the key reaches the configured host");
    assert!(seen.headers.iter().any(|(name, value)| name == "user-agent" && value.starts_with("NodeCraft/")));
    let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
    assert_eq!(body["model"], "gpt-4.1-mini-2025-04-14", "the catalog model id is sent unchanged");
}

#[test]
fn an_invalid_scheme_is_refused_and_nothing_is_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let keys = keys();
    for bad in [format!("ftp://{address}/v1"), format!("file://{address}/v1"), format!("ws://{address}/v1"), "http://proxy.example/v1".to_owned()] {
        let mut settings = ProviderSettings::default();
        settings.base_urls.insert("openai".into(), bad.clone().into());
        let Err(Readiness::NotSetUp(why)) = make("openai", &env(&keys, Arc::new(ReqwestHttp::new().unwrap()), &settings)) else {
            panic!("{bad} must be refused")
        };
        assert!(why.contains("HTTPS"), "{why}");
        assert!(!why.contains(&address.to_string()) && !why.contains("proxy.example"), "the error doesn't echo the URL: {why}");
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(listener.accept().is_err(), "no connection was made");
}

/// Records requests and answers with the fixture reply, without a network.
#[derive(Default)]
struct Recorder(Mutex<Vec<HttpRequest>>);
impl Http for Recorder {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.lock().unwrap().push(request);
        Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: REPLY.as_bytes().to_vec() }) })
    }
}

#[test]
fn the_default_is_unchanged_when_nothing_is_set() {
    let keys = keys();
    let http = Arc::new(Recorder::default());
    for settings in [ProviderSettings::default(), ProviderSettings { base_urls: [("openai".to_owned(), "  ".into())].into(), ..Default::default() }] {
        let provider = make("openai", &env(&keys, http.clone(), &settings)).unwrap();
        assert_eq!(provider.custom_base_url(), None);
        assert!(provider.retry_ambiguous_submit(), "OpenAI's own API keeps its retry policy");
        futures::executor::block_on(provider.submit(&job())).unwrap();
        let request = http.0.lock().unwrap().pop().unwrap();
        assert_eq!(request.url, "https://api.openai.com/v1/chat/completions");
    }
}

fn fixture_session(provider: Arc<dyn Provider>) -> (Session, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([60; 16]);
    let node = NodeId::from_bytes([61; 16]);
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let values: nodecraft_types::ParamValues =
        [("model".into(), Value::Text("openai.gpt-4.1-mini".into())), ("prompt".into(), Value::Text("A harbour".into()))].into();
    let card = kinds.get(nodecraft_nodes::generation::TEXT_KIND).unwrap().instantiate_instance(node, Default::default(), &values).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([62; 16]), "Custom endpoint public fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(nodecraft_engine::run_cmds::BudgetState { budgets: Default::default(), spent: Default::default() });
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    route(&mut session, provider, node);
    session.register_run_host(|_| Ok(json!({"run": "fixture"}))).unwrap();
    (session, canvas, node)
}

fn route(session: &mut Session, provider: Arc<dyn Provider>, node: NodeId) {
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(provider).unwrap();
    routing.select(node, "openai", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
}

#[test]
fn a_catalog_model_keeps_its_price_and_a_changed_base_url_needs_a_new_quote() {
    let keys = keys();
    let http: Arc<dyn Http> = Arc::new(Recorder::default());
    let default = make("openai", &env(&keys, http.clone(), &ProviderSettings::default())).unwrap();
    let mut settings = ProviderSettings::default();
    settings.base_urls.insert("openai".into(), "http://127.0.0.1:4000/v1".into());
    let custom = make("openai", &env(&keys, http.clone(), &settings)).unwrap();

    let (mut session, canvas, node) = fixture_session(default.clone());
    let request = json!({"canvas": canvas, "mode": "selected", "nodes": [node]});
    let at_default = session.dispatch("run.quote", &request).unwrap();
    route(&mut session, custom.clone(), node);
    let at_custom = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(at_custom["steps"][0]["price"], at_default["steps"][0]["price"], "the catalog price is kept: {at_custom}");
    assert_eq!(at_custom["totals"], at_default["totals"]);
    assert_eq!(at_custom["complete"], at_default["complete"]);
    assert_ne!(at_custom["token"], at_default["token"], "the quote binds where the run goes");

    // A quote taken before the base URL changed can't start the run; the same acceptance
    // starts it when nothing changed.
    for change in [true, false] {
        let (mut session, canvas, node) = fixture_session(default.clone());
        let mut request = json!({"canvas": canvas, "mode": "selected", "nodes": [node]});
        let quote = session.dispatch("run.quote", &request).unwrap();
        let micros = quote["totals"]["USD"]["micros"].as_u64().unwrap();
        assert!(micros > 0, "{quote}");
        if change {
            route(&mut session, custom.clone(), node);
        }
        request["accept"] = json!({"token": quote["token"], "amounts": {"USD": micros}, "partial": quote["complete"] == false, "unpriced_jobs": 1});
        assert_eq!(session.dispatch("run.start", &request).is_ok(), !change, "{quote}");
    }
}
