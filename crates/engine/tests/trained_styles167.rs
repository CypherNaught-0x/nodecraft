//! Trained styles (#167) through the engine: a Train style card with wired pictures is quoted by
//! its steps, runs once on the mock trainer and keeps a style descriptor naming its family; an
//! image card of that family sends the style in its `style` role, and one of another family is
//! refused before anything is quoted or sent, naming both. Mock provider only.
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
    planner::{Planned, RunMode},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Node, Position, Project};
use nodecraft_nodes::generation::{KIND, STYLE_KIND};
use nodecraft_providers::JobInput;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{AssetRef, CanvasId, EdgeId, NodeId, ProjectId, STYLE_MIME, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([67; 16]);
const TRAIN: NodeId = NodeId::from_bytes([1; 16]);
const PAINT: NodeId = NodeId::from_bytes([2; 16]);
const PICTURES: [NodeId; 2] = [NodeId::from_bytes([3; 16]), NodeId::from_bytes([4; 16])];
const TRAINER: &str = "fixture.trainer";
const PAINTER: &str = "fixture.painter";
const OTHER: &str = "fixture.other-painter";

fn painter(id: &str, family: &str) -> String {
    format!(
        r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "mock-image"
label = "Fixture painter"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
style_family = "{family}"
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
[[model.modes]]
id = "styled"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "style", modality = "style", min = 1, max = 1 }}]
"#
    )
}

fn catalog() -> Catalog {
    let trainer = format!(
        r#"[[model]]
id = "{TRAINER}"
provider = "mock"
provider_model = "mock-style"
label = "Fixture trainer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
style_family = "flux-1"
pricing = {{ type = "per_unit", unit = "thousand_steps", price = "2 USD", count = "steps" }}
[[model.modes]]
id = "images-to-style"
output = "style"
inputs = [{{ role = "images", modality = "image", min = 2, max = 20 }}]
[[model.params.params]]
name = "steps"
label = "Steps"
control = {{ type = "unsigned", min = "1", max = "10000", step = "1" }}
default = {{ type = "unsigned", value = "1000" }}
[[model.params.params]]
name = "family"
label = "Family"
control = {{ type = "text", multiline = false, max_bytes = 64 }}
default = {{ type = "text", value = "flux-1" }}
"#
    );
    Catalog::from_toml(&format!("{trainer}{}{}", painter(PAINTER, "flux-1"), painter(OTHER, "sdxl-1"))).unwrap()
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

/// A Train style card fed two pictures, and an image card on the trainer's family, unwired.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([67; 16]), "Styles").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let train =
        kinds.get(STYLE_KIND).unwrap().instantiate_instance(TRAIN, Default::default(), &[("model".into(), Value::Text(TRAINER.into()))].into()).unwrap();
    let values = [("model".into(), Value::Text(PAINTER.into())), ("prompt".into(), Value::Text("A lighthouse at dusk".into()))].into();
    let paint = kinds.get(KIND).unwrap().instantiate_instance(PAINT, Default::default(), &values).unwrap();
    for node in [train, paint] {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "styles".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    for node in [TRAIN, PAINT] {
        routing.select(node, "mock", binding.target()).unwrap();
    }
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let pngs: Vec<Vec<u8>> = [40u8, 200].iter().map(|shade| nodecraft_media::encode_png(4, 4, &[*shade; 4 * 4 * 4], &Default::default()).unwrap()).collect();
    let files: Vec<Incoming<'_>> = pngs.iter().map(|bytes| Incoming { name: "picture.png", bytes }).collect();
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &files, &PICTURES).unwrap();
    for (n, picture) in PICTURES.iter().enumerate() {
        let edge = json!({"id": EdgeId::from_bytes([10 + n as u8; 16]), "from": {"node": picture, "port": "out"}, "to": {"node": TRAIN, "port": "images"}, "order": n});
        session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    Fixture { session, host, mock }
}

fn node(session: &Session, id: NodeId) -> &Node {
    session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap()
}

/// The card's active result, as one asset.
fn result(node: &Node) -> AssetRef {
    let result = node.history.iter().find(|g| Some(g.id) == node.active).expect("an active result");
    match result.outputs.as_slice() {
        [Value::Media(asset)] => asset.clone(),
        [Value::List(items)] => match items.as_slice() {
            [Value::Media(asset)] => asset.clone(),
            other => panic!("one asset: {other:?}"),
        },
        other => panic!("one asset: {other:?}"),
    }
}

fn quote(f: &mut Fixture, card: NodeId) -> Result<serde_json::Value, nodecraft_types::Error> {
    f.session.dispatch("run.quote", &json!({"canvas":C,"mode":"selected","nodes":[card]}))
}

/// Quotes, accepts and runs `card` at the quoted `micros`.
fn run(f: &mut Fixture, card: NodeId, micros: u64) {
    let notices = attempt(f, card, micros);
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
}

/// Quotes, accepts and runs `card`, returning what the run reported.
fn attempt(f: &mut Fixture, card: NodeId, micros: u64) -> Vec<RunNotice> {
    let q = quote(f, card).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], micros, "the cost is shown before the run: {q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = json!({"canvas":C,"mode":"selected","nodes":[card]});
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
    notices
}

#[test]
fn a_trained_style_is_quoted_by_steps_kept_as_a_descriptor_and_fed_to_its_family() {
    let mut f = fixture();
    assert_eq!(f.session.kinds().get(STYLE_KIND).unwrap().descriptor().label, "Train style");
    // 1000 steps at $2 per 1000; 2500 steps cost $5.
    assert_eq!(quote(&mut f, TRAIN).unwrap()["totals"]["USD"]["micros"], 2_000_000);
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":TRAIN,"name":"steps","value":{"type":"unsigned","value":"2500"}})).unwrap();
    run(&mut f, TRAIN, 5_000_000);
    let requests = f.mock.requests();
    let [job] = requests.as_slice() else { panic!("one training request: {requests:?}") };
    assert_eq!(job.model, "mock-style");
    assert_eq!(job.params.get("steps"), Some(&json!(2500)));
    let images = job.inputs.iter().filter(|input| matches!(input, JobInput::Media { role, mime, .. } if role == "images" && mime == "image/png")).count();
    assert_eq!((images, job.inputs.len()), (2, 2), "the pictures are the dataset, and a trainer takes no prompt");

    let style = result(node(&f.session, TRAIN));
    assert_eq!((style.mime.essence(), style.mime.param("family")), (STYLE_MIME, Some("flux-1")));

    let edge = json!({"id": EdgeId::from_bytes([30; 16]), "from": {"node": TRAIN, "port": "out"}, "to": {"node": PAINT, "port": "style"}, "order": 0});
    f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    run(&mut f, PAINT, 10_000);
    let requests = f.mock.requests();
    let job = requests.last().unwrap();
    let sent = job.inputs.iter().find_map(|input| match input {
        JobInput::Media { role, mime, bytes } if role == "style" => Some((mime.clone(), bytes.clone())),
        _ => None,
    });
    let (mime, bytes) = sent.expect("the style goes in its role");
    assert_eq!(mime, STYLE_MIME);
    let descriptor = nodecraft_media::style::decode(&bytes).unwrap();
    assert_eq!((descriptor.family.as_str(), descriptor.provider.as_str()), ("flux-1", "mock"));
    assert!(descriptor.weights.starts_with("https://"));
    assert!(matches!(result(node(&f.session, PAINT)).mime.essence(), "image/png"));

    // Another family's model refuses the style before anything is quoted or sent, naming both.
    let before = f.mock.requests().len();
    let preview = f.session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[PAINT],"model":OTHER})).unwrap();
    let commit = json!({"canvas":C,"nodes":[PAINT],"model":OTHER,"expected_revision":preview["revision"],"preview_binding":preview["binding"]});
    f.session.dispatch("node.set_model", &commit).unwrap();
    let q = quote(&mut f, PAINT).unwrap();
    assert_eq!((q["allowed"].clone(), q["blocked"].to_string().contains("node_validation")), (json!(false), true), "{q}");
    // The quote keeps only fixed reasons; the card's own words name both families.
    let canvas = f.session.document().canvases().get(&C).unwrap();
    let planned = nodecraft_exec::planner::plan(canvas, f.session.kinds(), &RunMode::Selected, &[PAINT].into(), &Default::default()).unwrap();
    let Planned::Blocked(blockers) = planned else { panic!("blocked: {planned:?}") };
    let message = blockers.iter().map(|b| b.message(canvas)).collect::<Vec<_>>().join("; ");
    assert!(message.contains("trained for flux-1") && message.contains("takes sdxl-1"), "{message}");
    assert_eq!(f.mock.requests().len(), before);
}

#[test]
fn a_retrained_style_keeps_the_first_in_history_survives_a_save_and_never_trains_twice() {
    let mut f = fixture();
    run(&mut f, TRAIN, 2_000_000);
    let first = result(node(&f.session, TRAIN));
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":TRAIN,"name":"steps","value":{"type":"unsigned","value":"1500"}})).unwrap();
    run(&mut f, TRAIN, 3_000_000);
    let card = node(&f.session, TRAIN);
    assert_ne!(result(card), first, "the retrained style is the active one");
    assert_eq!(card.history.len(), 2, "and the first stays in the card's history");
    // A saved project keeps both styles and their families, and reads them back intact.
    let snapshot = nodecraft_format::Snapshot { project: f.session.document().clone(), assets: f.session.assets().clone(), epoch: 0 };
    let reopened = nodecraft_format::decode(&nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    let saved = reopened.project.canvases().get(&C).unwrap().nodes().get(&TRAIN).unwrap();
    assert_eq!(saved, card);
    for asset in [&first, &result(saved)] {
        assert_eq!(asset.mime.param("family"), Some("flux-1"));
        let style = nodecraft_media::style::decode(reopened.assets.get(&asset.hash).expect("the descriptor is saved")).unwrap();
        assert_eq!(style.family, "flux-1");
    }
    let keys: std::collections::BTreeSet<_> = f.mock.requests().iter().map(|job| job.idempotency_key.clone()).collect();
    assert_eq!((keys.len(), f.mock.billed()), (2, 2), "one paid training per run");
}

#[test]
fn a_trainer_returning_another_family_s_style_keeps_nothing() {
    let mut f = fixture();
    // The mock trains for whatever family it is told; the catalog says this trainer trains flux-1.
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":TRAIN,"name":"family","value":{"type":"text","value":"sdxl-1"}})).unwrap();
    let notices = attempt(&mut f, TRAIN, 2_000_000);
    assert!(notices.iter().any(|n| matches!(n, RunNotice::Failed { .. })), "{notices:?}");
    assert!(format!("{notices:?}").contains("different model family"), "{notices:?}");
    assert!(node(&f.session, TRAIN).active.is_none(), "no style of the wrong family is offered");
}

#[test]
fn styles_are_listed_with_their_family_and_used_only_by_cards_that_take_them() {
    let mut f = fixture();
    let list = |f: &mut Fixture| f.session.dispatch("style.list", &json!({"canvas": C, "for": PAINT})).unwrap();
    let before = list(&mut f);
    let [untrained] = before["styles"].as_array().unwrap().as_slice() else { panic!("{before}") };
    assert_eq!(
        (untrained["ready"].clone(), untrained["compatible"].clone(), untrained["family"].clone()),
        (json!(false), json!(false), json!(null)),
        "{before}"
    );
    let use_it = json!({"canvas": C, "style": TRAIN, "target": PAINT, "edge": EdgeId::from_bytes([40; 16])});
    let err = f.session.dispatch("style.use", &use_it).unwrap_err();
    assert!(err.message.contains("run it first"), "{err:?}");

    run(&mut f, TRAIN, 2_000_000);
    let listed = list(&mut f);
    let style = &listed["styles"][0];
    assert_eq!(
        (style["ready"].clone(), style["family"].clone(), style["trainer"].clone(), style["compatible"].clone()),
        (json!(true), json!("flux-1"), json!(TRAINER), json!(true)),
        "{listed}"
    );
    assert!(style["trigger"].is_string(), "{listed}");

    // One changeset wires it; a second style card's style replaces it in one more.
    let used = f.session.dispatch("style.use", &use_it).unwrap();
    assert_eq!(used["replaced"], json!([]));
    let wired = |f: &Fixture| -> Vec<NodeId> {
        f.session.document().canvases().get(&C).unwrap().edges().values().filter(|e| e.to.node == PAINT && e.to.port == "style").map(|e| e.from.node).collect()
    };
    assert_eq!(wired(&f), [TRAIN]);
    assert_eq!(f.session.dispatch("style.use", &use_it).unwrap_err().kind, nodecraft_types::ErrorKind::Conflict, "already wired");
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(wired(&f).is_empty(), "one undo step");
    f.session.dispatch("edit.redo", &json!({})).unwrap();
    run(&mut f, PAINT, 10_000);
    assert!(f.mock.requests().last().unwrap().inputs.iter().any(|input| matches!(input, JobInput::Media { role, .. } if role == "style")));

    // A card on another family is refused, with both families and the models that would take it.
    let preview = f.session.dispatch("node.model_preview", &json!({"canvas":C,"nodes":[PAINT],"model":OTHER})).unwrap();
    let commit = json!({"canvas":C,"nodes":[PAINT],"model":OTHER,"expected_revision":preview["revision"],"preview_binding":preview["binding"]});
    f.session.dispatch("node.set_model", &commit).unwrap();
    let listed = list(&mut f);
    let reason = listed["styles"][0]["reason"].as_str().unwrap().to_owned();
    assert_eq!(listed["styles"][0]["compatible"], false);
    assert!(reason.contains("trained for flux-1") && reason.contains("takes sdxl-1") && reason.contains(PAINTER), "{reason}");
    let again = json!({"canvas": C, "style": TRAIN, "target": PAINT, "edge": EdgeId::from_bytes([41; 16])});
    assert!(f.session.dispatch("style.use", &again).unwrap_err().message.contains(PAINTER));
    // Nor is a style wired into a card that isn't a generation card, or from one that isn't a style.
    let into_picture = json!({"canvas": C, "style": TRAIN, "target": PICTURES[0], "edge": EdgeId::from_bytes([42; 16])});
    assert!(f.session.dispatch("style.use", &into_picture).is_err());
    let from_painter = json!({"canvas": C, "style": PAINT, "target": PAINT, "edge": EdgeId::from_bytes([43; 16])});
    assert!(f.session.dispatch("style.use", &from_painter).unwrap_err().message.contains("Train style card"));
}

#[test]
fn a_style_card_starts_a_card_on_a_model_that_takes_the_style() {
    let mut f = fixture();
    const NEW: NodeId = NodeId::from_bytes([50; 16]);
    let params = |model: Option<&str>| {
        let mut p = json!({"canvas": C, "style": TRAIN, "id": NEW, "edge": EdgeId::from_bytes([51; 16]), "pos": {"x": 400, "y": 300}});
        if let Some(model) = model {
            p["model"] = json!(model);
        }
        p
    };
    assert!(f.session.dispatch("style.card", &params(None)).unwrap_err().message.contains("run it first"));
    run(&mut f, TRAIN, 2_000_000);
    let refused = f.session.dispatch("style.card", &params(Some(OTHER))).unwrap_err();
    assert!(refused.message.contains("flux-1") && refused.message.contains(PAINTER), "{refused:?}");
    let made = f.session.dispatch("style.card", &params(None)).unwrap();
    assert_eq!(made, json!({"id": NEW, "model": PAINTER}));
    let card = node(&f.session, NEW).clone();
    assert_eq!(card.params.get("model"), Some(&Value::Text(PAINTER.into())));
    let trigger = f.session.dispatch("style.list", &json!({"canvas": C})).unwrap()["styles"][0]["trigger"].as_str().unwrap().to_owned();
    assert!(trigger.starts_with("style "), "the mock names a trigger: {trigger:?}");
    assert!(matches!(card.params.get("prompt"), Some(Value::Text(p)) if p.starts_with(&trigger)), "{:?}", card.params);
    let wires: Vec<_> =
        f.session.document().canvases().get(&C).unwrap().edges().values().filter(|e| e.to.node == NEW).map(|e| (e.from.node, e.to.port.clone())).collect();
    assert_eq!(wires, [(TRAIN, "style".to_owned())]);
    f.session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(f.session.document().canvases().get(&C).unwrap().nodes().get(&NEW).is_none(), "one undo step");
}
