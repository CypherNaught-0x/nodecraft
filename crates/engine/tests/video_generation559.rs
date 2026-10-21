//! Video generation cards (#559) through the engine: the shipped catalog's video models get a
//! Generate video card; a mock run is quoted first and leaves one `video/*` clip on the card,
//! with a wired first frame sent in its role; and a model choice moves between video models but
//! never to an image model. Mock provider only.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_cmds::RunCatalog,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Node, Position, Project};
use nodecraft_nodes::generation::VIDEO_KIND;
use nodecraft_providers::JobInput;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([59; 16]);
const CARD: NodeId = NodeId::from_bytes([3; 16]);
const FRAME: NodeId = NodeId::from_bytes([4; 16]);
const CLIP: &str = "fixture.clip";
const QUICK: &str = "fixture.quick-clip";
const PAINTER: &str = "fixture.painter";

fn catalog() -> Catalog {
    let seed = r#"
[[model.params.params]]
name = "seed"
label = "Seed"
control = { type = "unsigned", min = "0", max = "4294967295", step = "1" }
"#;
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{CLIP}"
provider = "mock"
provider_model = "mock-video"
label = "Fixture clip"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.05 USD" }}
[[model.modes]]
id = "text-to-video"
output = "video"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
[[model.modes]]
id = "image-to-video"
output = "video"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "first_frame", modality = "image", min = 1, max = 1 }}]
params = ["seed"]
[[model.params.params]]
name = "duration"
label = "Duration"
control = {{ type = "unsigned", min = "2", max = "10", step = "1" }}
default = {{ type = "unsigned", value = "5" }}
{seed}
[[model]]
id = "{QUICK}"
provider = "mock"
provider_model = "mock-video"
label = "Fixture quick clip"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.02 USD" }}
[[model.modes]]
id = "text-to-video"
output = "video"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
{seed}
[[model]]
id = "{PAINTER}"
provider = "mock"
provider_model = "mock-image"
label = "Fixture painter"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .unwrap()
}

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct Fixture {
    session: Session,
    host: RunHost,
    mock: MockProvider,
}

/// One Generate video card with a seed set, and one imported picture, nothing wired yet.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([59; 16]), "Video").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [
        ("model".into(), Value::Text(CLIP.into())),
        ("prompt".into(), Value::Text("A paper boat on a pond".into())),
        ("seed".into(), Value::Unsigned(nodecraft_types::Unsigned(42))),
    ]
    .into();
    let card = kinds.get(VIDEO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "video".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(CARD, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let png = nodecraft_media::encode_png(4, 4, &[90; 4 * 4 * 4], &Default::default()).unwrap();
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &[Incoming { name: "frame.png", bytes: &png }], &[FRAME]).unwrap();
    Fixture { session, host, mock }
}

fn card(session: &Session) -> &Node {
    session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap()
}

/// Quotes, accepts and runs the card; the quote must come first and be the model's price.
fn run(f: &mut Fixture) {
    let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], 50_000, "the cost is shown before the run: {q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token":q["token"],"partial":q["complete"] != true,"unpriced_jobs":0,"amounts":amounts});
    f.session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut notices = Vec::new();
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
}

/// The active result's media types, flattened.
fn result_mimes(node: &Node) -> Vec<String> {
    let result = node.history.iter().find(|g| Some(g.id) == node.active).expect("an active result");
    let mut mimes = Vec::new();
    for value in &result.outputs {
        let items = match value {
            Value::List(items) => items.clone(),
            single => vec![single.clone()],
        };
        for item in items {
            if let Value::Media(asset) = item {
                mimes.push(asset.mime.essence().to_owned());
            }
        }
    }
    mimes
}

#[test]
fn the_shipped_catalog_offers_a_video_card_for_its_video_models() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let kind = kinds.get(VIDEO_KIND).unwrap();
    assert_eq!(kind.descriptor().label, "Generate video");
    assert!(snapshot.model(VIDEO_KIND, "fal.wan22").is_some(), "Wan 2.2 text to video is a video model");
    assert!(snapshot.model(nodecraft_nodes::generation::KIND, "fal.wan22").is_none(), "and not an image model");
}

#[test]
fn a_mock_run_leaves_one_clip_on_the_card_and_a_first_frame_goes_in_its_role() {
    let mut f = fixture();
    run(&mut f);
    let requests = f.mock.requests();
    let [job] = requests.as_slice() else { panic!("one request: {requests:?}") };
    assert_eq!(job.model, "mock-video");
    assert!(job.inputs.iter().all(|input| matches!(input, JobInput::Text { role, .. } if role == "prompt")), "text to video sends only its prompt");
    assert_eq!(job.params.get("duration"), Some(&json!(5)));
    assert_eq!(result_mimes(card(&f.session)), ["video/mp4"], "one clip, as a video the card's player takes");

    let edge = json!({"id": EdgeId::from_bytes([20; 16]), "from": {"node": FRAME, "port": "out"}, "to": {"node": CARD, "port": "first_frame"}, "order": 0});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    run(&mut f);
    let requests = f.mock.requests();
    let job = requests.last().unwrap();
    let roles: Vec<&str> = job
        .inputs
        .iter()
        .map(|input| match input {
            JobInput::Text { role, .. } | JobInput::Media { role, .. } => role.as_str(),
        })
        .collect();
    assert_eq!(roles, ["prompt", "first_frame"]);
    assert!(job.params.get("duration").is_none(), "image to video takes no duration");
    assert_eq!(job.params.get("seed"), Some(&json!(42)));
    assert_eq!(result_mimes(card(&f.session)), ["video/mp4"]);
}

#[test]
fn a_model_choice_moves_between_video_models_but_not_to_an_image_model() {
    let mut f = fixture();
    let s = &mut f.session;
    let preview = s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[CARD],"model":QUICK})).unwrap();
    let commit = json!({"canvas":C,"nodes":[CARD],"model":QUICK,"expected_revision":preview["revision"],"preview_binding":preview["binding"]});
    s.dispatch("node.set_model", &commit).unwrap();
    let node = card(s);
    assert_eq!(node.kind, VIDEO_KIND);
    assert_eq!(node.params.get("model"), Some(&Value::Text(QUICK.into())));
    assert_eq!(node.params.get("prompt"), Some(&Value::Text("A paper boat on a pond".into())), "the prompt carries over");
    // A shared name alone claims no compatibility: without a declared meaning the value resets.
    assert_ne!(node.params.get("seed"), Some(&Value::Unsigned(nodecraft_types::Unsigned(42))), "an untagged setting does not carry over");
    assert!(!node.params.contains_key("duration"), "a setting the new model lacks is dropped");

    let before = s.document().clone();
    let refused = s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[CARD],"model":PAINTER}));
    assert!(refused.is_err(), "a video card never takes an image model: {refused:?}");
    assert_eq!(s.document(), &before);
    let error = s.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[CARD],"model":"fixture.missing"})).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::NotFound | ErrorKind::Unsupported | ErrorKind::InvalidInput), "{error:?}");
}
