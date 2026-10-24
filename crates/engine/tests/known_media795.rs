//! Known media limits must stop a deferred card before its upstream mock writer is paid.
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
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::{
    InputValue, PlanCtx,
    generation::{TEXT_KIND, VIDEO_KIND},
};
use nodecraft_providers::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::{AssetRef, CanvasId, Currency, EdgeId, MediaMeta, Modality, Money, NodeId, PortType, ProjectId, ProviderTarget, TargetSelection, Value};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const C: CanvasId = CanvasId::from_bytes([95; 16]);
const WRITER: NodeId = NodeId::from_bytes([1; 16]);
const TARGET: NodeId = NodeId::from_bytes([2; 16]);
const SOURCE: NodeId = NodeId::from_bytes([3; 16]);

#[derive(Clone, Copy)]
enum Case {
    Pdf,
    Video,
    Audio,
}
impl Case {
    fn role(self) -> &'static str {
        match self {
            Self::Pdf => "document",
            Self::Video => "source",
            Self::Audio => "audio",
        }
    }
    fn modality(self) -> Modality {
        match self {
            Self::Pdf => Modality::Document,
            Self::Video => Modality::Video,
            Self::Audio => Modality::Audio,
        }
    }
    fn source(self) -> &'static str {
        match self {
            Self::Pdf => "source.document",
            Self::Video => "source.video",
            Self::Audio => "source.audio",
        }
    }
    fn target(self) -> &'static str {
        match self {
            Self::Video => VIDEO_KIND,
            _ => TEXT_KIND,
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Video => "video/mp4",
            Self::Audio => "audio/wav",
        }
    }
    fn metadata(self, invalid: bool) -> MediaMeta {
        match self {
            Self::Pdf => MediaMeta { page_count: Some(if invalid { 300 } else { 10 }), ..Default::default() },
            Self::Video => MediaMeta { duration_ms: Some(if invalid { 1000 } else { 5000 }), ..Default::default() },
            Self::Audio => MediaMeta { duration_ms: Some(if invalid { 10_001 } else { 5000 }), ..Default::default() },
        }
    }
}
fn catalog(case: Case) -> Catalog {
    let output = if matches!(case, Case::Video) { "video" } else { "text" };
    let remote = if matches!(case, Case::Video) { "mock-video" } else { "mock-text" };
    let modality = match case {
        Case::Pdf => "document",
        Case::Video => "video",
        Case::Audio => "audio",
    };
    let limits = match case {
        Case::Pdf => "document = { role = \"document\", max_pages = 100, tokens_per_page = 1000 }",
        Case::Video => {
            "edit = { operation = \"object_swap\", role = \"source\", min_ms = 4000, uses_ms = 30000, formats = [\"video/mp4\"], sound = \"unknown\" }"
        }
        Case::Audio => "audio = { role = \"audio\", duration = \"follows_audio\", max_ms = 10000, longer = \"refuse\", sound = \"input\" }",
    };
    Catalog::from_toml(&format!(
        r#"
[[model]]
id = "fixture.writer"
provider = "mock"
provider_model = "mock-text"
label = "Prompt writer"
description = "Public deterministic fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]

[[model]]
id = "fixture.target"
provider = "mock"
provider_model = "{remote}"
label = "Bounded media reader"
description = "Public deterministic fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "media-to-{output}"
output = "{output}"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "{}", modality = "{modality}", min = 1, max = 1 }}]
{limits}
"#,
        case.role()
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
            std::thread::sleep(Duration::from_millis(ms.min(5)));
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
    asset: AssetRef,
    case: Case,
    budget: JobBudget,
}
fn fixture(case: Case, invalid: bool) -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog(case)).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([95; 16]), "Known media bounds").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind, model) in [(WRITER, TEXT_KIND, "fixture.writer"), (TARGET, case.target(), "fixture.target")] {
        let values = [("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Write a short summary".into()))].into();
        let node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "known-media".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget.clone()).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut paid = MockModel::new(MockOutput::Text);
    paid.cost = Some(Money::new(Currency::USD, 10_000));
    let mock = MockProvider::new("mock", [("mock-text".into(), paid), ("mock-video".into(), MockModel::new(MockOutput::Video))]);
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
    routing.select(WRITER, "mock", binding.target()).unwrap();
    routing.select(TARGET, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    // Stored public fixture bytes and descriptive metadata. No decoder or live provider runs.
    let bytes: &[u8] = match case {
        Case::Pdf => b"%PDF-1.7\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n",
        Case::Video => include_bytes!("../../providers/src/fal/fixtures/blue.mp4"),
        Case::Audio => include_bytes!("../../providers/src/fal/fixtures/tone.wav"),
    };
    let hash = session.import_asset(bytes).unwrap();
    let asset = AssetRef { hash, mime: case.mime().parse().unwrap(), meta: case.metadata(invalid) };
    let node = session
        .kinds()
        .get(case.source())
        .unwrap()
        .instantiate_instance(SOURCE, Default::default(), &[("asset".into(), Value::Media(asset.clone()))].into())
        .unwrap();
    session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    for (n, from, port) in [(10, WRITER, "prompt"), (11, SOURCE, case.role())] {
        let edge = json!({"id":EdgeId::from_bytes([n;16]),"from":{"node":from,"port":"out"},"to":{"node":TARGET,"port":port},"order":0});
        session.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    }
    Fixture { session, host, mock, asset, case, budget }
}
fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"with_upstream","nodes":[TARGET]})
}
fn accepted_request(quote: &serde_json::Value) -> serde_json::Value {
    let amounts: serde_json::Map<_, _> =
        quote["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request();
    // This fixture has exactly two provider cards. Consent covers their quoted amounts
    // and explicitly accepts the deferred reader's partial/unknown price, never an unlimited run.
    start["accept"] = json!({"token":quote["token"],"partial":!quote["complete"].as_bool().unwrap(),"unpriced_jobs":2,"amounts":amounts});
    start
}
fn complete_refusal(f: &Fixture) -> nodecraft_types::Error {
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&TARGET).unwrap();
    let inputs = [
        InputValue {
            port: "prompt".into(),
            port_type: PortType { modality: Modality::Text, many: true },
            value: Value::Text("The completed writer's prompt".into()),
            origins: vec![WRITER],
        },
        InputValue {
            port: f.case.role().into(),
            port_type: PortType { modality: f.case.modality(), many: true },
            value: Value::Media(f.asset.clone()),
            origins: vec![SOURCE],
        },
    ];
    let mut targets = TargetSelection::empty();
    targets.insert(TARGET, &ProviderTarget::default_instance("mock").unwrap()).unwrap();
    f.session.kinds().get(f.case.target()).unwrap().plan(&PlanCtx { node, inputs: &inputs, targets: &targets }).unwrap_err()
}
fn refused_before_the_writer(case: Case) {
    let mut f = fixture(case, true);
    let expected = complete_refusal(&f);
    let quote = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(quote["allowed"], false, "{quote}");
    assert!(
        quote["blocked"].as_array().unwrap().iter().any(|reason| reason.as_str().is_some_and(|reason| reason.contains(&expected.message))),
        "{quote}; expected {expected}"
    );
    let error = f.session.dispatch("run.start", &accepted_request(&quote)).unwrap_err();
    assert!(error.message.contains(&expected.message), "{error}; expected {expected}");
    f.host.pump(&mut f.session);
    assert!(!f.host.is_busy());
    assert!(f.mock.requests().is_empty(), "even the upstream writer must receive no paid request");
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), 0);
}
#[test]
fn known_pdf_pages_block_a_deferred_reader_before_the_writer_is_paid() {
    refused_before_the_writer(Case::Pdf);
}
#[test]
fn known_short_video_blocks_a_deferred_edit_before_the_writer_is_paid() {
    refused_before_the_writer(Case::Video);
}
#[test]
fn known_overlong_audio_blocks_a_deferred_card_before_the_writer_is_paid() {
    refused_before_the_writer(Case::Audio);
}

#[test]
fn valid_known_media_still_runs_the_paid_upstream_writer_and_reader() {
    let mut f = fixture(Case::Pdf, false);
    let quote = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    let start = accepted_request(&quote);
    f.session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut notices = Vec::new();
    loop {
        notices.extend(f.host.pump(&mut f.session));
        if !f.host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "{notices:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!notices.iter().any(|notice| matches!(notice, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    assert_eq!(f.mock.requests().len(), 2, "the same fixture really submits both paid cards when its known media fits");
    assert_eq!(f.budget.log.lock().unwrap().entries().count(), 2);
}
