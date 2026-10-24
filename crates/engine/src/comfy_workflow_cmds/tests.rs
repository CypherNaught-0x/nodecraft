use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Project};
use nodecraft_types::ProjectId;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const CARD: NodeId = NodeId::from_bytes([5; 16]);
const OBJECT_INFO: &str = r#"{
  "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
  "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["CONDITIONING"]},
  "KSampler": {"input": {"required": {
    "positive": ["CONDITIONING"], "latent_image": ["LATENT"],
    "seed": ["INT", {"min": 0, "max": 18446744073709551615, "control_after_generate": true}], "steps": ["INT", {"min": 1, "max": 10000, "default": 20}],
    "cfg": ["FLOAT", {"min": 0.0, "max": 100.0, "step": 0.1}], "sampler_name": [["euler", "dpmpp_2m"]]
  }}, "output": ["LATENT"]},
  "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
  "PartnerImage": {"input": {"required": {"prompt": ["STRING", {}], "strength": ["FLOAT", {"min": 1.0, "max": 0.0}]}}, "output": ["IMAGE"], "api_node": true}
}"#;
const FOX: &str = r#"{
  "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
  "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
  "3": {"class_type": "KSampler", "inputs": {"positive": ["2", 0], "latent_image": ["1", 0], "seed": 7, "steps": 20, "cfg": 3.5, "sampler_name": "euler"}},
  "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "nc"}}
}"#;
/// [`FOX`] as the editor saves it.
const FOX_UI: &str = r#"{
  "last_node_id": 4, "last_link_id": 3, "version": 0.4,
  "nodes": [
    {"id": 1, "type": "LoadImage", "mode": 0, "inputs": [], "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [2]}], "widgets_values": ["example.png", "image"]},
    {"id": 2, "type": "CLIPTextEncode", "mode": 0, "inputs": [], "outputs": [{"name": "CONDITIONING", "type": "CONDITIONING", "links": [1]}], "widgets_values": ["a red fox"]},
    {"id": 3, "type": "KSampler", "mode": 0,
     "inputs": [{"name": "positive", "type": "CONDITIONING", "link": 1}, {"name": "latent_image", "type": "LATENT", "link": 2}],
     "outputs": [{"name": "LATENT", "type": "LATENT", "links": [3]}], "widgets_values": [7, "fixed", 20, 3.5, "euler"]},
    {"id": 4, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 3}], "outputs": [], "widgets_values": ["nc"]}
  ],
  "links": [[1, 2, 0, 3, 0, "CONDITIONING"], [2, 1, 0, 3, 1, "LATENT"], [3, 3, 0, 4, 0, "IMAGE"]],
  "groups": [], "config": {}, "extra": {}
}"#;

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Import").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Session::new(project).unwrap()
}

fn catalog() -> Arc<NodeCatalog> {
    Arc::new(NodeCatalog::parse(OBJECT_INFO.as_bytes()).unwrap().0)
}

/// A session that has read [`OBJECT_INFO`].
fn ready() -> Session {
    let mut s = session();
    let report = finish(&mut s, 1, Ok(catalog().into()));
    assert_eq!(report, Report { id: 1, nodes: Some(5), error: None, server: None });
    s
}

fn import_params(workflow: &str) -> Value {
    json!({"canvas": C, "id": CARD, "pos": {"x": 10.0, "y": 20.0}, "workflow": workflow})
}

fn card(s: &Session) -> &nodecraft_graph::Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap()
}

#[test]
fn an_import_is_refused_until_the_servers_nodes_are_read() {
    let mut s = session();
    let error = s.dispatch("comfy.workflow.import", &import_params(FOX)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("comfy.nodes.refresh"), "{}", error.message);
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&CARD));
}

#[test]
fn an_api_workflow_becomes_one_card_with_its_file_ports_prompts_and_seeds() {
    let mut s = ready();
    let result = s.dispatch("comfy.workflow.import", &import_params(FOX)).unwrap();
    assert_eq!(result["id"], json!(CARD));
    assert_eq!(result["ports"], json!(["loadimage_image"]));
    assert_eq!(result["outputs"], json!(["image"]));
    let params: Vec<&str> = result["params"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(params.len(), 2, "the prompt and the seed: {params:?}");
    let node = card(&s);
    assert_eq!(node.kind, comfy_workflow::ID);
    assert_eq!((node.pos.x, node.pos.y), (10.0, 20.0));
    let card = comfy_workflow::card(node).unwrap();
    assert_eq!(card.mapping.outputs.len(), 1);
    assert!(card.mapping.params.iter().all(|p| p.node != "3" || p.input == "seed"), "steps and cfg stay in the workflow");
    s.dispatch("edit.undo", &Value::Null).unwrap();
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&CARD), "the import is one undo step");
}

#[test]
fn a_ui_workflow_is_converted_with_the_servers_nodes() {
    let mut s = ready();
    let result =
        s.dispatch("comfy.workflow.import", &json!({"canvas": C, "id": CARD, "pos": {"x": 0.0, "y": 0.0}, "workflow": FOX_UI, "title": "Fox"})).unwrap();
    assert_eq!(result["ports"], json!(["loadimage_image"]));
    assert_eq!(card(&s).title, "Fox");
    let card = comfy_workflow::card(card(&s)).unwrap();
    let workflow: serde_json::Value = serde_json::from_str(&card.workflow).unwrap();
    assert_eq!(workflow["3"]["inputs"]["steps"], 20, "widget values line up with the server's inputs");
}

#[test]
fn named_params_replace_the_default_selection() {
    let mut s = ready();
    let mut params = import_params(FOX);
    params["params"] = json!(["3.steps", "3.cfg"]);
    let result = s.dispatch("comfy.workflow.import", &params).unwrap();
    assert_eq!(result["ports"], json!(["loadimage_image"]), "file inputs stay ports");
    let card = comfy_workflow::card(card(&s)).unwrap();
    let named: Vec<&str> = card.mapping.params.iter().map(|p| p.input.as_str()).collect();
    assert_eq!(named, ["steps", "cfg"]);

    let mut s = ready();
    params["params"] = json!(["3.positive"]);
    let error = s.dispatch("comfy.workflow.import", &params).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("3.positive"), "{}", error.message);
    params["params"] = json!(["1.image"]);
    assert!(s.dispatch("comfy.workflow.import", &params).is_err(), "a file input can't be a parameter");
}

#[test]
fn a_workflow_using_nodes_the_server_lacks_is_refused_by_name() {
    let mut s = ready();
    let workflow = FOX.replace("\"KSampler\"", "\"FancySampler\"");
    let error = s.dispatch("comfy.workflow.import", &import_params(&workflow)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("FancySampler"), "{}", error.message);
}

#[test]
fn hostile_imports_are_errors() {
    let mut s = ready();
    for (workflow, why) in [("", "empty"), ("[]", "not an object"), ("{\"1\": 5}", "a node that isn't one"), ("{", "truncated")] {
        assert!(s.dispatch("comfy.workflow.import", &import_params(workflow)).is_err(), "{why}");
    }
    let nothing_saved = r#"{"2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}}}"#;
    assert!(s.dispatch("comfy.workflow.import", &import_params(nothing_saved)).is_err(), "a workflow that saves nothing");
    let mut far = import_params(FOX);
    far["pos"] = json!({"x": 1e30, "y": 0.0});
    assert!(s.dispatch("comfy.workflow.import", &far).is_err());
    let mut many = import_params(FOX);
    many["params"] = json!(vec!["3.seed"; MAX_EXPOSE + 1]);
    assert!(s.dispatch("comfy.workflow.import", &many).is_err());
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&CARD));
}

#[test]
fn a_failed_read_keeps_the_nodes_read_before_and_status_reports_it() {
    let mut s = ready();
    let report = finish(&mut s, 2, Err(Error::new(ErrorKind::Network, "the server didn't answer")));
    assert_eq!(report.nodes, None);
    let status = s.dispatch("comfy.nodes.status", &json!({})).unwrap();
    assert_eq!(status["last"]["id"], 2);
    assert_eq!(status["last"]["error"], "the server didn't answer");
    assert_eq!(status["nodes"], 5, "the list read before stays in use");
    s.dispatch("comfy.workflow.import", &import_params(FOX)).unwrap();
}

#[test]
fn refresh_asks_the_host_and_is_refused_without_one() {
    let mut s = session();
    let error = s.dispatch("comfy.nodes.refresh", &json!({})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let status = s.dispatch("comfy.nodes.status", &json!({})).unwrap();
    assert_eq!(status, json!({"last": null, "nodes": null, "servers": {}}));

    let mut s = session();
    s.register_host_service(NODES_SERVICE, |_| Ok(json!({"pending": 1}))).unwrap();
    assert_eq!(s.dispatch("comfy.nodes.refresh", &json!({})).unwrap(), json!({"pending": 1}));
    assert!(s.dispatch("comfy.nodes.refresh", &json!({"extra": 1})).is_err());
}

#[test]
fn a_seed_comfyui_randomised_is_imported_fixed_with_a_warning() {
    let mut s = ready();
    let workflow = FOX_UI.replace(r#"[7, "fixed","#, r#"[7, "randomize","#);
    let result = s.dispatch("comfy.workflow.import", &import_params(&workflow)).unwrap();
    let warnings: Vec<&str> = result["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(warnings.iter().any(|w| w.contains("3.seed") && w.contains("randomize")), "{warnings:?}");
}

const SYSTEM_STATS: &str = r#"{"system": {"os": "posix", "comfyui_version": "0.3.40", "ram_total": 1, "ram_free": 1},
  "devices": [{"name": "cuda:0 NVIDIA GeForce RTX 4090", "type": "cuda", "vram_total": 24000000000, "vram_free": 18200000000}]}"#;
const QUEUE: &str = r#"{"queue_running": [[0, "a", {}, {}, []]], "queue_pending": [[1, "b", {}, {}, []], [2, "c", {}, {}, []]]}"#;

/// A ComfyUI server answering by path; `healthy` false answers 404 to all but `/object_info`, as
/// a proxy that only passes the API might.
struct Server {
    healthy: bool,
}

impl nodecraft_providers::http::Http for Server {
    fn send(
        &self,
        request: nodecraft_providers::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        let path = request.url.split('?').next().unwrap_or_default().rsplit('/').next().unwrap_or_default().to_owned();
        let body = match path.as_str() {
            "object_info" => Some(OBJECT_INFO),
            "system_stats" if self.healthy => Some(SYSTEM_STATS),
            "queue" if self.healthy => Some(QUEUE),
            _ => None,
        };
        let response = match body {
            Some(body) => nodecraft_providers::HttpResponse { status: 200, headers: Vec::new(), body: body.as_bytes().to_vec() },
            None => nodecraft_providers::HttpResponse { status: 404, headers: Vec::new(), body: Vec::new() },
        };
        Box::pin(async move { Ok(response) })
    }
}

fn fetch_from(healthy: bool) -> Result<Fetched, Error> {
    let settings = crate::settings::ProviderSettings::with_comfyui("http://127.0.0.1:8188", "salt");
    let keys = crate::secrets::MemoryStore::default();
    let env = HostEnv { keys: &keys, http: Arc::new(Server { healthy }), settings: &settings, now_ms: Arc::new(|| 0), progress: None };
    futures::executor::block_on(fetch(&env))
}

#[test]
fn a_check_reads_the_nodes_and_the_servers_health() {
    let mut s = session();
    let report = finish(&mut s, 1, fetch_from(true));
    let health = report.server.clone().unwrap();
    assert_eq!(report.nodes, Some(5));
    assert_eq!(health.summary(), "ComfyUI 0.3.40 · cuda:0 NVIDIA GeForce RTX 4090 · 18.2 / 24.0 GB free · 1 running, 2 waiting");
    let status = s.dispatch("comfy.nodes.status", &json!({})).unwrap();
    assert_eq!(
        status["last"]["server"],
        json!({"version": "0.3.40", "device": "cuda:0 NVIDIA GeForce RTX 4090 · 18.2 / 24.0 GB free", "running": 1, "pending": 2})
    );
    assert_eq!(last(&s), (Some(&report), Some(5)));
}

#[test]
fn a_server_without_health_still_has_its_nodes_read() {
    let mut s = session();
    let report = finish(&mut s, 1, fetch_from(false));
    assert_eq!(report, Report { id: 1, nodes: Some(5), error: None, server: None });
    s.dispatch("comfy.workflow.import", &import_params(FOX)).unwrap();
}

#[test]
fn forgetting_the_server_drops_its_nodes() {
    let mut s = ready();
    forget(&mut s);
    assert_eq!(last(&s), (None, None));
    assert_eq!(s.dispatch("comfy.nodes.status", &json!({})).unwrap(), json!({"last": null, "nodes": null, "servers": {}}));
    assert!(s.dispatch("comfy.workflow.import", &import_params(FOX)).is_err());
}

#[test]
fn server_addresses_are_checked_without_echoing_them() {
    assert_eq!(check_url("   ").unwrap(), None);
    assert_eq!(check_url(" http://gpu-box:8188/comfy \n").unwrap().as_deref(), Some("http://gpu-box:8188/comfy"));
    assert_eq!(check_url("https://comfy.example.com").unwrap().as_deref(), Some("https://comfy.example.com"));
    for bad in [
        "ftp://gpu-box:8188",
        "gpu-box:8188",
        "http://",
        "http://me:hunter2@gpu-box:8188",
        "http://gpu-box:8188/?token=hunter2",
        "http://gpu-box:8188/#hunter2",
    ] {
        let error = check_url(bad).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{bad}");
        assert!(!error.message.contains("hunter2") && !error.message.contains("gpu-box"), "{bad}: {}", error.message);
    }
    let long = format!("http://gpu-box:8188/{}", "a".repeat(MAX_URL));
    assert!(check_url(&long).unwrap_err().message.contains("longer than"));
}

#[test]
fn server_reported_text_is_one_bounded_line() {
    assert_eq!(shown("0.3.40"), "0.3.40");
    assert_eq!(shown("RTX\n4090\u{1b}[31m"), "RTX4090[31m");
    let long = shown(&"é".repeat(10_000));
    assert_eq!(long.chars().count(), MAX_SHOWN + 1);
    assert!(long.ends_with('…'));
}

#[test]
fn forgetting_the_server_drops_its_option_lists_only() {
    let mut session = session();
    let mut lists = OptionLists::default();
    let choice = |v: &str| vec![nodecraft_types::Choice { value: v.into(), label: v.into() }];
    let key = |source| nodecraft_types::OptionKey { owner: "m", source, scope: None };
    lists.set(key("comfyui.checkpoints"), choice("old.safetensors")).unwrap();
    lists.set(key("fal.voices"), choice("v")).unwrap();
    session.swap_resource(Some(lists));
    forget(&mut session);
    let lists = session.resource::<OptionLists>().unwrap();
    assert!(lists.get(key("comfyui.checkpoints")).is_none());
    assert!(lists.get(key("fal.voices")).is_some());
}

#[test]
fn a_workflow_with_partner_nodes_warns_and_a_bad_input_is_left_out() {
    let mut s = ready();
    let workflow = r#"{
      "1": {"class_type": "PartnerImage", "inputs": {"prompt": "a fox", "strength": 0.5}},
      "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "nc"}}
    }"#;
    let mut params = import_params(workflow);
    params["params"] = json!(["1.prompt", "1.strength"]);
    let result = s.dispatch("comfy.workflow.import", &params).unwrap();
    let warnings: Vec<&str> = result["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(warnings.iter().any(|w| w.contains("PartnerImage") && w.contains("comfy.org")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("1.strength")), "a bad input is left out, not refused: {warnings:?}");
    assert_eq!(result["params"].as_array().unwrap().len(), 1);
}
