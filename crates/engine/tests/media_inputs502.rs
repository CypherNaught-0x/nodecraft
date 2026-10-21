//! Media inputs on generation cards (#502): planning accepts wired assets on the roles the
//! selected mode declares, and the engine reads them from the asset store into the provider
//! job, refusing oversized calls before any bytes are read. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generate::{self, MAX_MEDIA_INPUT_BYTES, MediaSource},
    generation_catalog::GenerationCatalog,
    jobs::{CompletionDelivery, JobBudget, ProviderRunner},
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    run::RunOptions,
    scheduler::{JobId, JobState, Scheduler},
};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::{
    GenerateInput, InputValue, JobSpec, PlanCtx,
    generation::{KIND, TEXT_KIND},
};
use nodecraft_providers::{JobInput, mock::MockProvider};
use nodecraft_types::{
    AssetRef, Blake3, CanvasId, EdgeId, Error, ErrorKind, MediaMeta, Modality, NodeId, PortType, ProjectId, ProviderTarget, TargetSelection, Value,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

const C: CanvasId = CanvasId::from_bytes([50; 16]);
const READER: NodeId = NodeId::from_bytes([1; 16]);
const READER_MODEL: &str = "fixture.reader";
const MIXER_MODEL: &str = "fixture.mixer";
const PAIR_MODEL: &str = "fixture.pair";

fn model(id: &str, output: &str, modes: &str) -> Catalog {
    Catalog::from_toml(&format!(
        r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "mock-{output}"
label = "Fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
{modes}"#
    ))
    .unwrap()
}

/// A text model that reads up to two images, one that compares two or three, and an image model
/// with one optional role of every media modality.
fn catalog() -> Catalog {
    let mut catalog = model(
        READER_MODEL,
        "text",
        r#"[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.modes]]
id = "image-to-text"
output = "text"
inputs = [
  { role = "prompt", modality = "text", min = 1, max = 1, inline = true },
  { role = "image", modality = "image", min = 1, max = 2 },
]
"#,
    );
    let mixer = model(
        MIXER_MODEL,
        "image",
        r#"[[model.modes]]
id = "mix"
output = "image"
inputs = [
  { role = "prompt", modality = "text", min = 1, max = 1, inline = true },
  { role = "reference", modality = "image", min = 0, max = 2 },
  { role = "clip", modality = "video", min = 0, max = 1 },
  { role = "voice", modality = "audio", min = 0, max = 1 },
  { role = "shape", modality = "mesh", min = 0, max = 1 },
  { role = "brief", modality = "document", min = 0, max = 1 },
]
"#,
    );
    let pair = model(
        PAIR_MODEL,
        "text",
        r#"[[model.modes]]
id = "compare"
output = "text"
inputs = [
  { role = "prompt", modality = "text", min = 1, max = 1, inline = true },
  { role = "image", modality = "image", min = 2, max = 3 },
]
"#,
    );
    catalog.merge(mixer).unwrap();
    catalog.merge(pair).unwrap();
    catalog
}

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[20, 30, shade, 255].repeat(4), &Default::default()).unwrap()
}

fn asset(n: u8, mime: &str) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: mime.parse().unwrap(), meta: MediaMeta::default() })
}

fn wired(port: &str, modality: Modality, value: Value) -> InputValue {
    InputValue { port: port.into(), port_type: PortType { modality, many: true }, value, origins: vec![] }
}

/// Plan a lone card of `kind` running `model` with `inputs`, with no session around it.
fn plan(kind: &str, model: &str, inputs: &[InputValue]) -> Result<Vec<GenerateInput>, Error> {
    let kinds = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap().registry().unwrap();
    let kind = kinds.get(kind).unwrap();
    let node = kind
        .instantiate_instance(
            READER,
            Default::default(),
            &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Describe it".into()))].into(),
        )
        .unwrap();
    let mut targets = TargetSelection::empty();
    targets.insert(READER, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
    let mut jobs = kind.plan(&PlanCtx { node: &node, inputs, targets: &targets })?;
    let Some(JobSpec::Generate { inputs, .. }) = jobs.pop() else { panic!("not a generation job") };
    Ok(inputs)
}

#[test]
fn every_media_modality_plans_in_declared_role_order_and_wire_order() {
    // Wires arrive in an order unrelated to the roles; the job lists roles as the mode declares
    // them, and a role's assets in wire order.
    let inputs = [
        wired("brief", Modality::Document, asset(6, "application/pdf")),
        wired("reference", Modality::Image, asset(2, "image/png")),
        wired("shape", Modality::Mesh, asset(5, "model/gltf-binary")),
        wired("voice", Modality::Audio, asset(4, "audio/wav")),
        wired("reference", Modality::Image, asset(1, "image/jpeg")),
        wired("clip", Modality::Video, asset(3, "video/mp4")),
    ];
    let sent = plan(KIND, MIXER_MODEL, &inputs).unwrap();
    let roles: Vec<_> = sent.iter().map(|input| input.role.as_str()).collect();
    assert_eq!(roles, ["prompt", "reference", "reference", "clip", "voice", "shape", "brief"]);
    assert_eq!(sent[0].value, Value::Text("Describe it".into()));
    assert_eq!(sent[1].value, asset(2, "image/png"));
    assert_eq!(sent[2].value, asset(1, "image/jpeg"));
    assert_eq!(sent[6].value, asset(6, "application/pdf"));
    // A list wire is one wire carrying several assets, kept in order.
    let list = [wired("reference", Modality::Image, Value::List(vec![asset(7, "image/png"), asset(8, "image/webp")]))];
    let sent = plan(KIND, MIXER_MODEL, &list).unwrap();
    assert_eq!(sent.iter().skip(1).map(|input| &input.value).collect::<Vec<_>>(), [&asset(7, "image/png"), &asset(8, "image/webp")]);
}

#[test]
fn media_that_breaks_a_role_is_refused_while_planning() {
    let refused = |inputs: &[InputValue]| plan(KIND, MIXER_MODEL, inputs).unwrap_err();
    // An asset of another modality on a role.
    let wrong = refused(&[wired("reference", Modality::Image, asset(1, "audio/wav"))]);
    assert_eq!(wrong.kind, ErrorKind::InvalidInput);
    assert!(wrong.message.contains("takes Image assets, not"), "{wrong}");
    // More assets than the role takes, even through one list wire.
    let many = refused(&[wired("reference", Modality::Image, Value::List(vec![asset(1, "image/png"); 3]))]);
    assert!(many.message.contains("at most 2"), "{many}");
    // Text on a media role, and a role the model doesn't declare.
    assert!(refused(&[wired("reference", Modality::Image, Value::Text("not an image".into()))]).message.contains("must be assets"));
    let undeclared = refused(&[wired("mask", Modality::Image, asset(1, "image/png"))]);
    assert!(undeclared.message.contains("unavailable port"), "{undeclared}");
    // A wire whose type disagrees with the port it reaches.
    assert!(refused(&[wired("reference", Modality::Audio, asset(1, "audio/wav"))]).message.contains("unavailable port"));
    // The text model's image mode needs an image; with none, its text mode runs instead.
    assert_eq!(plan(TEXT_KIND, READER_MODEL, &[]).unwrap().len(), 1);
    let three = [
        wired("image", Modality::Image, asset(1, "image/png")),
        wired("image", Modality::Image, asset(2, "image/png")),
        wired("image", Modality::Image, asset(3, "image/png")),
    ];
    assert!(plan(TEXT_KIND, READER_MODEL, &three).is_err(), "the image mode takes two wires at most");
}

/// Media sizes from metadata; counts reads so a refusal can prove it read nothing.
#[derive(Default)]
struct Sized {
    sizes: BTreeMap<Blake3, u64>,
    reads: AtomicUsize,
}
impl MediaSource for Sized {
    fn size(&self, asset: &AssetRef) -> Result<u64, Error> {
        self.sizes.get(&asset.hash).copied().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing"))
    }
    fn read(&self, asset: &AssetRef, limit: u64) -> Result<Arc<[u8]>, Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let size = self.size(asset)?;
        assert!(size <= limit, "read past the limit it was given");
        Ok(vec![0; usize::try_from(size).unwrap()].into())
    }
}

fn job(media: Vec<Value>) -> JobSpec {
    let mut inputs = vec![GenerateInput { role: "prompt".into(), value: Value::Text("Describe it".into()) }];
    inputs.extend(media.into_iter().map(|value| GenerateInput { role: "image".into(), value }));
    JobSpec::Generate {
        node: READER,
        outputs: vec![nodecraft_nodes::GenerateOutput { port: "out".into(), modality: Modality::Text, style_family: None }],
        provider: "mock".into(),
        instance: ProviderTarget::default_instance("mock").unwrap().instance(),
        model: "mock-text".into(),
        params: Default::default(),
        inputs,
    }
}

#[test]
fn oversized_media_is_refused_before_any_bytes_are_read() {
    let half = MAX_MEDIA_INPUT_BYTES / 2 + 1;
    let source = Sized { sizes: [(Blake3::from_bytes([1; 32]), half), (Blake3::from_bytes([2; 32]), half)].into(), ..Default::default() };
    // Each fits alone; together they pass the per-call cap, and nothing is read.
    let both = job(vec![asset(1, "image/png"), asset(2, "image/png")]);
    let refused = generate::provider_job_with_media(&both, "k", &source).unwrap_err();
    assert!(refused.message.contains("larger than it allows"), "{refused}");
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    let (_, one) = generate::provider_job_with_media(&job(vec![asset(1, "image/png")]), "k", &source).unwrap();
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    let Some(JobInput::Media { role, mime, bytes }) = one.inputs.get(1) else { panic!("no media input: {:?}", one.inputs) };
    assert_eq!((role.as_str(), mime.as_str(), bytes.len() as u64), ("image", "image/png", half));
    // A missing asset is refused, and without a source media is unsupported, as before #502.
    assert!(generate::provider_job_with_media(&job(vec![asset(9, "image/png")]), "k", &source).is_err());
    assert_eq!(generate::provider_job(&job(vec![asset(1, "image/png")]), "k").unwrap_err().kind, ErrorKind::Unsupported);
}

#[derive(Default)]
struct Immediate(Arc<AtomicU64>);
impl Runtime for Immediate {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let w = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_ready());
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let t = self.0.clone();
        Box::pin(async move {
            t.fetch_add(ms, Ordering::SeqCst);
        })
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Runs the session's host was handed.
type Captured = Arc<Mutex<Vec<PreparedRun>>>;

/// A reader card with a real PNG from the session's asset store wired into its `port`.
fn session(port: &str) -> (Session, Captured, MockProvider, Vec<u8>) {
    let (s, captured, mock, mut images) = session_with(READER_MODEL, port, 1);
    (s, captured, mock, images.remove(0))
}

/// A text card running `model` with `wires` distinct stored PNGs wired into its `port`. Edge
/// order runs opposite to creation order, so the job's order proves which one the planner used.
fn session_with(model: &str, port: &str, wires: u8) -> (Session, Captured, MockProvider, Vec<Vec<u8>>) {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([51; 16]), "Media inputs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let reader = kinds
        .get(TEXT_KIND)
        .unwrap()
        .instantiate_instance(
            READER,
            Default::default(),
            &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Describe it".into()))].into(),
        )
        .unwrap();
    Change::AddNode { canvas: C, node: reader }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    let captured: Captured = Arc::default();
    let sink = captured.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |p| {
            sink.lock().unwrap().push(p);
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    let mock = MockProvider::standard();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(READER, "mock", binding.target()).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    s.set_provider_routing(routing).unwrap();
    let mut images = Vec::new();
    for i in 0..wires {
        let bytes = png(40 + i);
        let hash = s.import_asset(&bytes).unwrap();
        let supplied = [(
            "asset".into(),
            Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(2), height: Some(2), ..Default::default() } }),
        )]
        .into();
        let id = NodeId::from_bytes([2 + i; 16]);
        let picture = s.kinds().get("source.image").unwrap().instantiate_instance(id, Default::default(), &supplied).unwrap();
        s.dispatch("node.add", &json!({"canvas":C,"node":picture})).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([30 + i; 16]),
            from: Endpoint { node: id, port: "out".into() },
            to: Endpoint { node: READER, port: port.into() },
            order: u32::from(wires - 1 - i),
        };
        s.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
        images.push(bytes);
    }
    (s, captured, mock, images)
}

fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[READER]})
}

#[test]
fn a_wired_image_reaches_a_text_model_through_the_mock() {
    let (mut s, captured, mock, bytes) = session("image");
    let q = s.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let mut start = request();
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    s.dispatch("run.start", &start).unwrap();
    let prepared = captured.lock().unwrap().pop().unwrap();
    let (mut run, progress) =
        s.authorize_prepared_run(prepared, RunOptions { id: [52; 16], first_job: 1, limits: Default::default(), retry: Default::default() }).unwrap();
    let spec = &progress.jobs[0];
    // A host with no asset store still refuses media, before anything is sent.
    assert_eq!(run.queued_job(spec.id, &spec.idempotency_key).err().unwrap().kind, ErrorKind::Unsupported);
    let queued = run.queued_job_with_media(spec.id, &spec.idempotency_key, s.assets()).unwrap();
    let deliveries: Arc<Mutex<BTreeMap<JobId, CompletionDelivery>>> = Arc::default();
    let out = deliveries.clone();
    let runtime = Arc::new(Immediate::default());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "media-fixture".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let runner = Arc::new(
        ProviderRunner::new(
            runtime.clone(),
            Default::default(),
            Arc::new(move |id, d| {
                assert!(out.lock().unwrap().insert(id, d).is_none());
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(budget.clone()),
    );
    runner.register(spec.id, queued);
    let mut scheduler = Scheduler::new(Default::default());
    scheduler.add(progress.jobs.clone()).unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let end = futures::executor::block_on(driver.run(runner, runtime, |_| {}));
    assert_eq!(end.state(spec.id), Some(&JobState::Done));
    // The provider saw the prompt, then the stored image's exact bytes in the image role.
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let [JobInput::Text { role: prompt, .. }, JobInput::Media { role, mime, bytes: sent }] = requests[0].inputs.as_slice() else {
        panic!("unexpected inputs: {:?}", requests[0].inputs)
    };
    assert_eq!((prompt.as_str(), role.as_str(), mime.as_str()), ("prompt", "image", "image/png"));
    assert_eq!(&sent[..], &bytes[..]);
    let delivery = deliveries.lock().unwrap().remove(&spec.id).unwrap();
    let JobSpec::Generate { outputs: routes, .. } = run.work(spec.id).unwrap() else { panic!("not a generation job") };
    let converted = generate::produced(routes, &delivery.completed).unwrap();
    let done = run.job_done(s.kinds(), spec.id, converted.outputs);
    assert!(done.failed.is_empty());
    assert_eq!(budget.log.lock().unwrap().entries().count(), 1, "the paid call is accounted");
}

#[test]
fn a_card_offers_only_the_media_roles_its_model_declares() {
    // The reader declares `image` and no `reference`, so its card has only the one media port.
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let reader =
        kinds.get(TEXT_KIND).unwrap().instantiate_instance(READER, Default::default(), &[("model".into(), Value::Text(READER_MODEL.into()))].into()).unwrap();
    assert!(!reader.inputs.iter().any(|port| port.name == "reference"));
    assert!(reader.inputs.iter().any(|port| port.name == "image"));
}

#[test]
fn a_role_that_needs_two_assets_runs_on_two_real_wires_in_edge_order() {
    // The planner folds a port's wires into one list; the mode still sees two assets (#506).
    let (mut s, captured, _mock, images) = session_with(PAIR_MODEL, "image", 2);
    let q = s.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let mut start = request();
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    s.dispatch("run.start", &start).unwrap();
    let prepared = captured.lock().unwrap().pop().unwrap();
    let (run, progress) =
        s.authorize_prepared_run(prepared, RunOptions { id: [53; 16], first_job: 1, limits: Default::default(), retry: Default::default() }).unwrap();
    let spec = &progress.jobs[0];
    let queued = run.queued_job_with_media(spec.id, &spec.idempotency_key, s.assets()).unwrap();
    let sent: Vec<_> = queued
        .job
        .inputs
        .iter()
        .filter_map(|input| match input {
            JobInput::Media { bytes, .. } => Some(bytes.to_vec()),
            _ => None,
        })
        .collect();
    // Edge order puts the second picture first.
    assert_eq!(sent, [images[1].clone(), images[0].clone()]);
    // The real store refuses an asset it doesn't have, and one larger than the read's limit.
    let Value::Media(missing) = asset(9, "image/png") else { unreachable!() };
    assert!(s.assets().size(&missing).is_err());
    let hash = s.import_asset(&images[0]).unwrap();
    let stored = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() };
    assert_eq!(s.assets().size(&stored).unwrap(), images[0].len() as u64);
    assert!(s.assets().read(&stored, images[0].len() as u64 - 1).is_err());
    assert_eq!(&s.assets().read(&stored, images[0].len() as u64).unwrap()[..], &images[0][..]);
    // One wire is too few for the only mode, and the run is refused.
    let (mut one, ..) = session_with(PAIR_MODEL, "image", 1);
    let refused = match one.dispatch("run.quote", &request()) {
        Ok(q) => q["allowed"] == false,
        Err(_) => true,
    };
    assert!(refused, "a single image must not run the two-image mode");
}
