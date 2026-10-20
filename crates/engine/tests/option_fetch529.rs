//! Fetching option lists from a ComfyUI server (#529): `options.refresh` asks the host, the host
//! fetches `/object_info` off the UI thread, and `finish` installs what came back, so the
//! built-in SDXL row's checkpoint is checked against what the server has.
use futures::executor::block_on;
use nodecraft_engine::Session;
use nodecraft_engine::generation_catalog::default_registry;
use nodecraft_engine::option_fetch::{self, OPTION_SERVICE};
use nodecraft_engine::provider_setup::HostEnv;
use nodecraft_engine::secrets::MemoryStore;
use nodecraft_engine::settings::ProviderSettings;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{BoxFuture, ErrorKind as WireKind, Http, HttpRequest, HttpResponse, ProviderError};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId, Value};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const C: CanvasId = CanvasId::from_bytes([51; 16]);
const N: NodeId = NodeId::from_bytes([52; 16]);

/// `/object_info`'s cap, as the ComfyUI client sets it (`nodecraft_comfy::MAX_OBJECT_INFO_BYTES`).
const OBJECT_INFO_CAP: usize = 128 * 1024 * 1024;

/// Answers every request with `status` and `body`, or fails it as offline; keeps the urls and
/// response caps asked for. A body over `cap` (or the request's own cap) is refused as too large,
/// the way the real client refuses one.
struct Server {
    body: Option<Vec<u8>>,
    status: u16,
    cap: usize,
    asked: Mutex<Vec<(String, usize)>>,
}

impl Server {
    fn offline() -> Arc<Self> {
        Arc::new(Server { body: None, status: 200, cap: usize::MAX, asked: Mutex::default() })
    }

    fn answering(status: u16, body: &[u8]) -> Arc<Self> {
        Arc::new(Server { body: Some(body.to_vec()), status, cap: usize::MAX, asked: Mutex::default() })
    }

    fn urls(&self) -> Vec<String> {
        self.asked.lock().unwrap().iter().map(|(url, _)| url.clone()).collect()
    }
}

impl Http for Server {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.asked.lock().unwrap().push((request.url.clone(), request.max_response_bytes));
        let answer = match &self.body {
            Some(body) if body.len() > request.max_response_bytes.min(self.cap) => Err(ProviderError::new(WireKind::Malformed, "the response is too large")),
            Some(body) => Ok(HttpResponse { status: self.status, headers: vec![("content-type".into(), "application/json".into())], body: body.clone() }),
            None => Err(ProviderError::new(WireKind::Network, "connection refused")),
        };
        Box::pin(async move { answer })
    }
}

fn server(checkpoints: &[&str]) -> Arc<Server> {
    let info = json!({
        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [checkpoints]}}, "output": ["MODEL", "CLIP", "VAE"], "category": "loaders"},
        "KSampler": {"input": {"required": {"sampler_name": [["euler", "dpmpp_2m"]], "scheduler": [["normal", "karras"]]}}, "output": ["LATENT"]},
    });
    Server::answering(200, &serde_json::to_vec(&info).unwrap())
}

fn settings() -> ProviderSettings {
    ProviderSettings { comfyui_url: Some("http://127.0.0.1:8188".into()), install_salt: "fixture-salt".into() }
}

fn fetch(http: Arc<Server>, settings: &ProviderSettings, sources: &[String]) -> Result<option_fetch::Fetched, Error> {
    let keys = MemoryStore::default();
    let env = HostEnv { keys: &keys, http, settings, now_ms: Arc::new(|| 0) };
    block_on(option_fetch::fetch(&env, sources))
}

/// A session with one SDXL card and an options service that records what it was asked.
fn session() -> (Session, Rc<RefCell<Vec<serde_json::Value>>>) {
    let mut project = Project::new(ProjectId::from_bytes([53; 16]), "Option fetch").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let (kinds, _) = default_registry().unwrap();
    let values = [("model".into(), Value::Text("comfyui.sdxl".into())), ("prompt".into(), Value::Text("A pier".into()))].into();
    let node = kinds.get(KIND).unwrap().instantiate_instance(N, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    session
        .register_host_service(OPTION_SERVICE, move |request| {
            log.borrow_mut().push(request.clone());
            Ok(json!({"pending": 1}))
        })
        .unwrap();
    (session, asked)
}

fn checkpoint(session: &mut Session, value: &str) -> Result<serde_json::Value, Error> {
    session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "checkpoint", "value": {"type": "text", "value": value}}))
}

fn refresh(session: &mut Session, asked: &RefCell<Vec<serde_json::Value>>) -> Vec<String> {
    assert_eq!(session.dispatch("options.refresh", &json!({})).unwrap(), json!({"pending": 1}));
    let request = asked.borrow_mut().pop().unwrap();
    serde_json::from_value(request["sources"].clone()).unwrap()
}

#[test]
fn a_fetched_checkpoint_list_is_what_the_sdxl_card_accepts() {
    let (mut session, asked) = session();
    assert!(session.dispatch("options.status", &json!({})).unwrap()["last"].is_null());
    // Before a fetch any checkpoint is accepted: the list is unknown, not empty.
    checkpoint(&mut session, "anything.safetensors").unwrap();
    let sources = refresh(&mut session, &asked);
    assert_eq!(sources, ["comfyui.checkpoints"]);
    let http = server(&["sdxl.safetensors", "juggernaut.safetensors", "", "sdxl.safetensors"]);
    let fetched = fetch(http.clone(), &settings(), &sources);
    assert_eq!(http.urls(), ["http://127.0.0.1:8188/object_info"]);
    // The reply's size is capped explicitly, not by the client's default.
    assert_eq!(http.asked.lock().unwrap()[0].1, OBJECT_INFO_CAP);
    let report = option_fetch::finish(&mut session, 1, fetched);
    assert_eq!(report.error, None);
    assert_eq!(report.installed.len(), 1, "{report:?}");
    assert_eq!((report.installed[0].owner.as_str(), report.installed[0].values, report.installed[0].skipped), ("comfyui.sdxl", 2, 2));
    let listed = session.dispatch("node.options", &json!({"canvas": C, "id": N, "param": "checkpoint"})).unwrap();
    assert_eq!(listed["known"], true);
    assert_eq!(
        listed["options"],
        json!([{"value": "sdxl.safetensors", "label": "sdxl.safetensors"}, {"value": "juggernaut.safetensors", "label": "juggernaut.safetensors"}])
    );
    assert!(checkpoint(&mut session, "anything.safetensors").is_err(), "the server doesn't have it");
    // A stale value saved before the fetch is reported, and the rest of the card stays editable.
    checkpoint(&mut session, "juggernaut.safetensors").unwrap();
    let report = option_fetch::finish(&mut session, 2, fetch(server(&["sdxl.safetensors"]), &settings(), &sources));
    assert_eq!(report.installed.len(), 1);
    let described = session.dispatch("node.describe", &json!({"canvas": C, "id": N})).unwrap();
    let issues = described["resolution"]["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| issue["code"] == "not_an_option" && issue["param"] == "checkpoint"), "{issues:?}");
    session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "prompt", "value": {"type": "text", "value": "A harbour"}})).unwrap();
    checkpoint(&mut session, "sdxl.safetensors").unwrap();
    let status = session.dispatch("options.status", &json!({})).unwrap();
    assert_eq!(status["last"]["id"], 2);
    assert_eq!(status["last"]["installed"][0]["source"], "comfyui.checkpoints");
}

#[test]
fn a_failed_or_refused_fetch_keeps_the_list_installed_before() {
    let (mut session, asked) = session();
    let sources = refresh(&mut session, &asked);
    option_fetch::finish(&mut session, 1, fetch(server(&["kept.safetensors"]), &settings(), &sources));
    // The server goes away: the error says what happened, the list stays.
    // The server goes away, refuses the request, or sends a reply too large to read: each error
    // says what happened and is the server's, not an internal one, and the list stays.
    let too_large = Server { body: Some(vec![b' '; 64]), status: 200, cap: 32, asked: Mutex::default() };
    let failing = [
        (Server::offline(), ErrorKind::Network),
        (Server::answering(404, b"Not Found"), ErrorKind::NotFound),
        (Server::answering(500, b"Internal Server Error"), ErrorKind::Network),
        (Server::answering(401, b"Unauthorized"), ErrorKind::Authentication),
        (Arc::new(too_large), ErrorKind::InvalidInput),
    ];
    for (id, (http, kind)) in (2..).zip(failing) {
        let error = fetch(http, &settings(), &sources).unwrap_err();
        assert_eq!(error.kind, kind, "{}", error.message);
        assert!(error.message.contains("ComfyUI"), "{}", error.message);
        let report = option_fetch::finish(&mut session, id, Err(error));
        assert!(report.error.is_some() && report.installed.is_empty());
        checkpoint(&mut session, "kept.safetensors").unwrap();
        assert!(checkpoint(&mut session, "other.safetensors").is_err());
    }
    // A list past the limit is refused whole, not truncated.
    let many: Vec<String> = (0..=nodecraft_types::MAX_OPTIONS).map(|i| format!("{i}.safetensors")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let report = option_fetch::finish(&mut session, 7, fetch(server(&many), &settings(), &sources));
    assert!(report.installed.is_empty());
    assert!(report.refused[0].reason.contains("more than"), "{:?}", report.refused);
    checkpoint(&mut session, "kept.safetensors").unwrap();
    assert_eq!(session.dispatch("options.status", &json!({})).unwrap()["last"]["id"], 7);
}

#[test]
fn without_a_server_or_a_fetching_host_the_error_says_what_to_do() {
    let error = fetch(server(&[]), &ProviderSettings::default(), &["comfyui.checkpoints".into()]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("comfyui_url"), "{}", error.message);
    // A host that registers no fetch service (the CLI) refuses before anything is asked.
    let mut project = Project::new(ProjectId::from_bytes([54; 16]), "No host").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let error = session.dispatch("options.refresh", &json!({})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("desktop app"), "{}", error.message);
    assert!(session.dispatch("options.refresh", &json!({"extra": 1})).is_err());
}
