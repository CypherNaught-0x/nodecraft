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

/// Answers every request with `body`, or fails it as offline; keeps the urls asked for.
struct Server {
    body: Option<Vec<u8>>,
    asked: Mutex<Vec<String>>,
}

impl Http for Server {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.asked.lock().unwrap().push(request.url.clone());
        let answer = match &self.body {
            Some(body) => Ok(HttpResponse { status: 200, headers: vec![("content-type".into(), "application/json".into())], body: body.clone() }),
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
    Arc::new(Server { body: Some(serde_json::to_vec(&info).unwrap()), asked: Mutex::default() })
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
    assert_eq!(*http.asked.lock().unwrap(), ["http://127.0.0.1:8188/object_info"]);
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
    checkpoint(&mut session, "juggernaut.safetensors").unwrap();
    let status = session.dispatch("options.status", &json!({})).unwrap();
    assert_eq!(status["last"]["id"], 1);
    assert_eq!(status["last"]["installed"][0]["source"], "comfyui.checkpoints");
}

#[test]
fn a_failed_or_refused_fetch_keeps_the_list_installed_before() {
    let (mut session, asked) = session();
    let sources = refresh(&mut session, &asked);
    option_fetch::finish(&mut session, 1, fetch(server(&["kept.safetensors"]), &settings(), &sources));
    // The server goes away: the error says what happened, the list stays.
    let offline = Arc::new(Server { body: None, asked: Mutex::default() });
    let error = fetch(offline, &settings(), &sources).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("ComfyUI"), "{}", error.message);
    let report = option_fetch::finish(&mut session, 2, Err(error));
    assert!(report.error.is_some() && report.installed.is_empty());
    checkpoint(&mut session, "kept.safetensors").unwrap();
    assert!(checkpoint(&mut session, "other.safetensors").is_err());
    // A list past the limit is refused whole, not truncated.
    let many: Vec<String> = (0..=nodecraft_types::MAX_OPTIONS).map(|i| format!("{i}.safetensors")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let report = option_fetch::finish(&mut session, 3, fetch(server(&many), &settings(), &sources));
    assert!(report.installed.is_empty());
    assert!(report.refused[0].reason.contains("more than"), "{:?}", report.refused);
    checkpoint(&mut session, "kept.safetensors").unwrap();
    assert_eq!(session.dispatch("options.status", &json!({})).unwrap()["last"]["id"], 3);
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
