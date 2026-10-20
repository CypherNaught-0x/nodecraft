//! A batch of prompts into an image Generate card (#519): one job per prompt, the images kept per
//! prompt, and a downstream card's image input collecting all of them, in this run or the next.
//! Mock provider only.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
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
use nodecraft_graph::{Canvas, Change, Generation, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::JobInput;
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([51; 16]);
const BATCH: NodeId = NodeId::from_bytes([1; 16]);
const PAINTER: NodeId = NodeId::from_bytes([2; 16]);
const EDITOR: NodeId = NodeId::from_bytes([3; 16]);
const MODEL: &str = "fixture.painter";

fn catalog() -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
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
[[model.modes]]
id = "edit"
output = "image"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "image", modality = "image", min = 1, max = 8 }}]
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

/// A batch of two prompts → a painter card → an editor card's image input.
fn fixture() -> Fixture {
    let mock = MockProvider::standard();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([52; 16]), "Media fan-out").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let card = |id: NodeId, prompt: &str| {
        let values = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text(prompt.into()))].into();
        kinds.get(KIND).unwrap().instantiate_instance(id, Default::default(), &values).unwrap()
    };
    let mut batch = kinds.get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(BATCH, Default::default(), &Default::default()).unwrap();
    batch.params.insert("text".into(), Value::Text("a fox\nan owl".into()));
    for node in [batch, card(PAINTER, ""), card(EDITOR, "Make it night")] {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "media-fanout".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    for node in [PAINTER, EDITOR] {
        routing.select(node, "mock", binding.target()).unwrap();
    }
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let mut f = Fixture { session, host, mock };
    for (e, (from, port, to, to_port)) in [(10u8, (BATCH, "item", PAINTER, "prompt")), (11, (PAINTER, "out", EDITOR, "image"))] {
        let edge = json!({"id": EdgeId::from_bytes([e; 16]), "from": {"node": from, "port": port}, "to": {"node": to, "port": to_port}, "order": 0});
        f.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    f
}

/// Quotes, checks the quoted price, accepts (an estimate where an input is made in this run), and drives the run until the host is idle.
fn run(f: &mut Fixture, nodes: &[NodeId], micros: u64) {
    let request = json!({"canvas":C,"mode":"selected","nodes":nodes});
    let q = f.session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    assert_eq!(q["totals"]["USD"]["micros"], micros, "{q}");
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

fn shown(f: &Fixture, id: NodeId) -> Generation {
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap();
    node.history.iter().find(|g| Some(g.id) == node.active).cloned().unwrap()
}

/// The images each editor request was sent.
fn sent_images(f: &Fixture) -> Vec<usize> {
    let requests = f.mock.requests();
    let edits = requests.iter().filter(|job| job.inputs.iter().any(|i| matches!(i, JobInput::Text { text, .. } if text.contains("night"))));
    edits.map(|job| job.inputs.iter().filter(|i| matches!(i, JobInput::Media { role, .. } if role == "image")).count()).collect()
}

#[test]
fn a_batch_of_prompts_paints_per_prompt_and_the_next_card_collects_every_image() {
    let mut f = fixture();
    // Two painter calls and one editor call.
    run(&mut f, &[PAINTER, EDITOR], 30_000);
    let painted = shown(&f, PAINTER);
    assert_eq!(painted.items.len(), 2);
    let [Value::List(lists)] = painted.outputs.as_slice() else { panic!("{painted:?}") };
    assert_eq!(lists.len(), 2, "one list of images per prompt");
    assert!(lists.iter().all(|l| matches!(l, Value::List(images) if !images.is_empty() && images.iter().all(|i| matches!(i, Value::Media(_))))), "{lists:?}");
    let images: usize = lists.iter().map(|l| if let Value::List(images) = l { images.len() } else { 0 }).sum();
    assert_eq!(sent_images(&f), vec![images], "one editor call with every painted image");
    let edited = shown(&f, EDITOR);
    assert!(edited.items.is_empty(), "a collecting input doesn't fan the editor out");

    // Changing the editor's prompt re-runs it alone, from the painter's stored result.
    f.session.dispatch("node.param_set", &json!({"canvas":C,"id":EDITOR,"name":"prompt","value":{"type":"text","value":"Make it night again"}})).unwrap();
    run(&mut f, &[EDITOR], 10_000);
    assert_eq!(sent_images(&f), vec![images, images]);
    assert_eq!(f.mock.requests().len(), 4);
}
